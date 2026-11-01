//! Minimal baseline TIFF writer: little-endian, uncompressed, 16 bit RGB, chunky.

use std::io::{self, Write};

const ROWS_PER_STRIP: usize = 16;

#[derive(Default)]
pub struct Metadata {
    pub make: Option<String>,
    pub model: Option<String>,
    pub software: Option<String>,
    pub datetime: Option<String>,
    /// Raw XMP packet (tag 700).
    pub xmp: Option<Vec<u8>>,
}

const SHORT: u16 = 3;
const LONG: u16 = 4;
const ASCII: u16 = 2;
const UNDEFINED: u16 = 7;

struct Tag {
    id: u16,
    ty: u16,
    count: u32,
    /// Inline value (count*size <= 4) or offset into the extra area (resolved at write time).
    value: Value,
}

enum Value {
    Inline(u32),
    Extra(Vec<u8>),
}

fn ascii(id: u16, s: &str) -> Tag {
    let mut b = s.as_bytes().to_vec();
    b.push(0);
    extra(id, ASCII, b.len() as u32, b)
}

fn extra(id: u16, ty: u16, count: u32, bytes: Vec<u8>) -> Tag {
    if bytes.len() <= 4 {
        let mut v = [0u8; 4];
        v[..bytes.len()].copy_from_slice(&bytes);
        Tag { id, ty, count, value: Value::Inline(u32::from_le_bytes(v)) }
    } else {
        Tag { id, ty, count, value: Value::Extra(bytes) }
    }
}

fn long(id: u16, v: u32) -> Tag {
    Tag { id, ty: LONG, count: 1, value: Value::Inline(v) }
}

fn short(id: u16, v: u16) -> Tag {
    Tag { id, ty: SHORT, count: 1, value: Value::Inline(v as u32) }
}

/// Writes a 16 bit RGB TIFF. `fill_row(y, buf)` must fill `buf` (width*3 samples) for row `y`.
pub fn write_rgb16<W: Write>(
    out: &mut W,
    width: usize,
    height: usize,
    meta: &Metadata,
    mut fill_row: impl FnMut(usize, &mut [u16]),
) -> io::Result<()> {
    let row_bytes = width * 6;
    let strips = height.div_ceil(ROWS_PER_STRIP);
    let total = row_bytes
        .checked_mul(height)
        .filter(|&t| t < u32::MAX as usize - (1 << 20))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "image too large for classic TIFF"))?;

    // Tags in ascending id order. Strip offsets are patched once the layout is known.
    let mut tags: Vec<Tag> = vec![
        long(256, width as u32),
        long(257, height as u32),
        extra(258, SHORT, 3, [16u16, 16, 16].iter().flat_map(|v| v.to_le_bytes()).collect()),
        short(259, 1),
        short(262, 2),
    ];
    if let Some(s) = &meta.make {
        tags.push(ascii(271, s));
    }
    if let Some(s) = &meta.model {
        tags.push(ascii(272, s));
    }
    let strip_offsets_at = tags.len();
    tags.push(extra(273, LONG, strips as u32, vec![0; strips * 4]));
    tags.push(short(277, 3));
    tags.push(long(278, ROWS_PER_STRIP as u32));
    tags.push(extra(
        279,
        LONG,
        strips as u32,
        (0..strips)
            .flat_map(|s| {
                let rows = ROWS_PER_STRIP.min(height - s * ROWS_PER_STRIP);
                ((rows * row_bytes) as u32).to_le_bytes()
            })
            .collect(),
    ));
    tags.push(short(284, 1));
    if let Some(s) = &meta.software {
        tags.push(ascii(305, s));
    }
    if let Some(s) = &meta.datetime {
        tags.push(ascii(306, s));
    }
    if let Some(x) = &meta.xmp {
        tags.push(extra(700, UNDEFINED, x.len() as u32, x.clone()));
    }

    let ifd_size = 2 + tags.len() * 12 + 4;
    let mut next = 8 + ifd_size;
    let mut offsets: Vec<Option<usize>> = Vec::with_capacity(tags.len());
    for t in &tags {
        match &t.value {
            Value::Extra(b) => {
                offsets.push(Some(next));
                next += b.len() + (b.len() & 1);
            }
            Value::Inline(_) => offsets.push(None),
        }
    }
    let pixel_start = next + (next & 1);
    if pixel_start + total > u32::MAX as usize {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "image too large for classic TIFF"));
    }
    if let Value::Extra(b) = &mut tags[strip_offsets_at].value {
        for s in 0..strips {
            let off = (pixel_start + s * ROWS_PER_STRIP * row_bytes) as u32;
            b[s * 4..s * 4 + 4].copy_from_slice(&off.to_le_bytes());
        }
    }

    out.write_all(b"II*\0")?;
    out.write_all(&8u32.to_le_bytes())?;
    out.write_all(&(tags.len() as u16).to_le_bytes())?;
    for (t, off) in tags.iter().zip(&offsets) {
        out.write_all(&t.id.to_le_bytes())?;
        out.write_all(&t.ty.to_le_bytes())?;
        out.write_all(&t.count.to_le_bytes())?;
        let v = match (&t.value, off) {
            (Value::Inline(v), _) => *v,
            (Value::Extra(_), Some(o)) => *o as u32,
            _ => unreachable!(),
        };
        out.write_all(&v.to_le_bytes())?;
    }
    out.write_all(&0u32.to_le_bytes())?;
    for t in &tags {
        if let Value::Extra(b) = &t.value {
            out.write_all(b)?;
            if b.len() & 1 == 1 {
                out.write_all(&[0])?;
            }
        }
    }
    if next & 1 == 1 {
        out.write_all(&[0])?;
    }

    let mut row = vec![0u16; width * 3];
    let mut bytes = vec![0u8; row_bytes];
    for y in 0..height {
        fill_row(y, &mut row);
        for (dst, v) in bytes.chunks_exact_mut(2).zip(&row) {
            dst.copy_from_slice(&v.to_le_bytes());
        }
        out.write_all(&bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_at(b: &[u8], o: usize) -> u16 {
        u16::from_le_bytes([b[o], b[o + 1]])
    }

    fn u32_at(b: &[u8], o: usize) -> u32 {
        u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
    }

    #[test]
    fn round_trips_pixels_and_dimensions() {
        let (w, h) = (5, 37); // 3 strips, last one partial
        let mut out = Vec::new();
        let meta = Metadata { model: Some("TEST".into()), xmp: Some(b"<x/>".to_vec()), ..Default::default() };
        write_rgb16(&mut out, w, h, &meta, |y, row| {
            for (i, v) in row.iter_mut().enumerate() {
                *v = (y * 1000 + i) as u16;
            }
        })
        .unwrap();

        assert_eq!(&out[..4], b"II*\0");
        let ifd = u32_at(&out, 4) as usize;
        let n = u16_at(&out, ifd) as usize;
        let mut tags = std::collections::HashMap::new();
        for i in 0..n {
            let e = ifd + 2 + i * 12;
            tags.insert(u16_at(&out, e), (u32_at(&out, e + 4), u32_at(&out, e + 8)));
        }
        assert_eq!(tags[&256].1 as usize, w);
        assert_eq!(tags[&257].1 as usize, h);
        let strips = h.div_ceil(ROWS_PER_STRIP);
        let offs = tags[&273].1 as usize;
        let first = u32_at(&out, offs) as usize;
        assert_eq!(out.len() - first, w * h * 6);
        // Pixel (x=2, y=20, channel 1) lives in strip 1.
        let strip1 = u32_at(&out, offs + 4) as usize;
        let at = strip1 + ((20 - ROWS_PER_STRIP) * w + 2) * 6 + 2;
        assert_eq!(u16_at(&out, at) as usize, 20 * 1000 + 2 * 3 + 1);
        assert_eq!(strips, 3);
    }
}
