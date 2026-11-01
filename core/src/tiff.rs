//! Minimal baseline TIFF writer: little-endian, uncompressed, 16 bit RGB, chunky.

use std::io::{self, Write};

const ROWS_PER_STRIP: usize = 16;

#[derive(Default)]
pub struct Metadata {
    pub make: Option<String>,
    pub model: Option<String>,
    pub software: Option<String>,
    /// "YYYY:MM:DD HH:MM:SS"
    pub datetime: Option<String>,
    /// TIFF Orientation tag value, 1..=8.
    pub orientation: Option<u16>,
    pub exif: Exif,
    /// Raw XMP packet (tag 700).
    pub xmp: Option<Vec<u8>>,
}

/// Exposure data stored in the EXIF sub-IFD. Rationals are (numerator, denominator).
#[derive(Default)]
pub struct Exif {
    pub exposure_time: Option<(u32, u32)>,
    pub f_number: Option<(u32, u32)>,
    pub iso: Option<u16>,
    /// "YYYY:MM:DD HH:MM:SS"
    pub datetime_original: Option<String>,
    pub exposure_bias: Option<(i32, i32)>,
    pub focal_length: Option<(u32, u32)>,
}

impl Exif {
    fn is_empty(&self) -> bool {
        self.exposure_time.is_none()
            && self.f_number.is_none()
            && self.iso.is_none()
            && self.datetime_original.is_none()
            && self.exposure_bias.is_none()
            && self.focal_length.is_none()
    }
}

const SHORT: u16 = 3;
const LONG: u16 = 4;
const ASCII: u16 = 2;
const RATIONAL: u16 = 5;
const UNDEFINED: u16 = 7;
const SRATIONAL: u16 = 10;

struct Tag {
    id: u16,
    ty: u16,
    count: u32,
    value: Value,
}

enum Value {
    /// Fits in the 4 byte value field.
    Inline(u32),
    /// Stored after the IFD; the entry holds its offset.
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
        Tag {
            id,
            ty,
            count,
            value: Value::Inline(u32::from_le_bytes(v)),
        }
    } else {
        Tag {
            id,
            ty,
            count,
            value: Value::Extra(bytes),
        }
    }
}

fn long(id: u16, v: u32) -> Tag {
    Tag {
        id,
        ty: LONG,
        count: 1,
        value: Value::Inline(v),
    }
}

fn short(id: u16, v: u16) -> Tag {
    Tag {
        id,
        ty: SHORT,
        count: 1,
        value: Value::Inline(v as u32),
    }
}

fn rational(id: u16, (n, d): (u32, u32)) -> Tag {
    let bytes = [n.to_le_bytes(), d.to_le_bytes()].concat();
    extra(id, RATIONAL, 1, bytes)
}

fn srational(id: u16, (n, d): (i32, i32)) -> Tag {
    let bytes = [n.to_le_bytes(), d.to_le_bytes()].concat();
    extra(id, SRATIONAL, 1, bytes)
}

/// Serializes `tags` (already in ascending id order) as an IFD placed at file offset `base`,
/// followed by its out-of-line values. The result length does not depend on tag values.
fn serialize_ifd(tags: &[Tag], base: usize) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(tags.len() as u16).to_le_bytes());
    let mut next = base + 2 + tags.len() * 12 + 4;
    let mut tail = Vec::new();
    for t in tags {
        out.extend_from_slice(&t.id.to_le_bytes());
        out.extend_from_slice(&t.ty.to_le_bytes());
        out.extend_from_slice(&t.count.to_le_bytes());
        match &t.value {
            Value::Inline(v) => out.extend_from_slice(&v.to_le_bytes()),
            Value::Extra(b) => {
                out.extend_from_slice(&(next as u32).to_le_bytes());
                tail.extend_from_slice(b);
                next += b.len();
                if b.len() & 1 == 1 {
                    tail.push(0);
                    next += 1;
                }
            }
        }
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&tail);
    out
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
    let too_large = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "image too large for classic TIFF",
        )
    };
    let total = row_bytes
        .checked_mul(height)
        .filter(|&t| t < u32::MAX as usize - (1 << 20))
        .ok_or_else(too_large)?;

    // Layout: header | EXIF IFD (optional) | IFD0 | pixel data.
    let exif_block = if meta.exif.is_empty() {
        None
    } else {
        let e = &meta.exif;
        let mut tags = Vec::new();
        if let Some(v) = e.exposure_time {
            tags.push(rational(33434, v));
        }
        if let Some(v) = e.f_number {
            tags.push(rational(33437, v));
        }
        if let Some(v) = e.iso {
            tags.push(short(34855, v));
        }
        if let Some(v) = &e.datetime_original {
            tags.push(ascii(36867, v));
        }
        if let Some(v) = e.exposure_bias {
            tags.push(srational(37380, v));
        }
        if let Some(v) = e.focal_length {
            tags.push(rational(37386, v));
        }
        Some(serialize_ifd(&tags, 8))
    };
    let exif_len = exif_block.as_ref().map_or(0, Vec::len);
    let ifd0_at = 8 + exif_len;

    let mut tags: Vec<Tag> = vec![
        long(256, width as u32),
        long(257, height as u32),
        extra(
            258,
            SHORT,
            3,
            [16u16, 16, 16]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect(),
        ),
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
    if let Some(v) = meta.orientation {
        tags.push(short(274, v));
    }
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
    if exif_block.is_some() {
        tags.push(long(34665, 8));
    }

    let pixel_start = ifd0_at + serialize_ifd(&tags, ifd0_at).len();
    if pixel_start + total > u32::MAX as usize {
        return Err(too_large());
    }
    if let Value::Extra(b) = &mut tags[strip_offsets_at].value {
        for s in 0..strips {
            let off = (pixel_start + s * ROWS_PER_STRIP * row_bytes) as u32;
            b[s * 4..s * 4 + 4].copy_from_slice(&off.to_le_bytes());
        }
    }

    out.write_all(b"II*\0")?;
    out.write_all(&(ifd0_at as u32).to_le_bytes())?;
    if let Some(b) = &exif_block {
        out.write_all(b)?;
    }
    out.write_all(&serialize_ifd(&tags, ifd0_at))?;

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
        let meta = Metadata {
            model: Some("TEST".into()),
            xmp: Some(b"<x/>".to_vec()),
            ..Default::default()
        };
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

    #[test]
    fn exif_sub_ifd_is_linked_and_readable() {
        let mut out = Vec::new();
        let meta = Metadata {
            exif: Exif {
                f_number: Some((28, 5)),
                iso: Some(200),
                ..Default::default()
            },
            orientation: Some(6),
            ..Default::default()
        };
        write_rgb16(&mut out, 2, 2, &meta, |_, row| row.fill(7)).unwrap();

        let ifd0 = u32_at(&out, 4) as usize;
        let n = u16_at(&out, ifd0) as usize;
        let orientation = (0..n)
            .map(|i| ifd0 + 2 + i * 12)
            .find(|&e| u16_at(&out, e) == 274)
            .map(|e| u16_at(&out, e + 8));
        assert_eq!(orientation, Some(6));
        let exif_at = (0..n)
            .map(|i| ifd0 + 2 + i * 12)
            .find(|&e| u16_at(&out, e) == 34665)
            .map(|e| u32_at(&out, e + 8) as usize)
            .expect("EXIF pointer tag");
        let en = u16_at(&out, exif_at) as usize;
        assert_eq!(en, 2);
        assert_eq!(u16_at(&out, exif_at + 2), 33437); // FNumber
        let rat = u32_at(&out, exif_at + 2 + 8) as usize;
        assert_eq!((u32_at(&out, rat), u32_at(&out, rat + 4)), (28, 5));
        assert_eq!(u16_at(&out, exif_at + 2 + 12), 34855); // ISOSpeedRatings
        assert_eq!(u16_at(&out, exif_at + 2 + 12 + 8), 200);
    }
}
