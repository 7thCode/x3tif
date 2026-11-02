//! A minimal PNG writer (8 bit RGB), enough to show a preview in a web view.

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
/// Deflate effort (miniz_oxide levels 0-10): previews are redrawn often, so favour speed.
const DEFLATE_LEVEL: u8 = 3;

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (n, t) in table.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *t = c;
    }
    !data.iter().fold(!0u32, |c, &b| {
        table[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8)
    })
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend((body.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend(kind);
    out.extend(body);
    let crc = crc32(&out[start..]);
    out.extend(crc.to_be_bytes());
}

/// Encodes `rgb` (`width * height * 3` bytes, row-major) as a PNG. Returns `None` if the
/// buffer does not match the size.
pub fn encode_rgb8(width: usize, height: usize, rgb: &[u8]) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || width > u32::MAX as usize || height > u32::MAX as usize {
        return None;
    }
    let row = width.checked_mul(3)?;
    if rgb.len() != row.checked_mul(height)? {
        return None;
    }
    let mut raw = Vec::with_capacity((row + 1) * height);
    for line in rgb.chunks_exact(row) {
        raw.push(0); // filter: none
        raw.extend_from_slice(line);
    }
    let mut header = Vec::with_capacity(13);
    header.extend((width as u32).to_be_bytes());
    header.extend((height as u32).to_be_bytes());
    header.extend([8, 2, 0, 0, 0]); // 8 bit, RGB, deflate, no filter scheme, no interlace

    let mut out = Vec::from(SIGNATURE);
    chunk(&mut out, b"IHDR", &header);
    chunk(
        &mut out,
        b"IDAT",
        &miniz_oxide::deflate::compress_to_vec_zlib(&raw, DEFLATE_LEVEL),
    );
    chunk(&mut out, b"IEND", &[]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
    }

    /// Splits a PNG into (type, body) chunks, checking each CRC.
    fn chunks(png: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
        assert_eq!(&png[..8], &SIGNATURE);
        let mut at = 8;
        let mut out = Vec::new();
        while at < png.len() {
            let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            let kind: [u8; 4] = png[at + 4..at + 8].try_into().unwrap();
            let body = png[at + 8..at + 8 + len].to_vec();
            let crc = u32::from_be_bytes(png[at + 8 + len..at + 12 + len].try_into().unwrap());
            assert_eq!(crc, crc32(&png[at + 4..at + 8 + len]), "crc of {kind:?}");
            out.push((kind, body));
            at += 12 + len;
        }
        out
    }

    #[test]
    fn round_trips_pixels() {
        let (w, h) = (5, 3);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 7 % 256) as u8).collect();
        let png = encode_rgb8(w, h, &rgb).unwrap();
        let parts = chunks(&png);
        assert_eq!(
            parts.iter().map(|p| &p.0).collect::<Vec<_>>(),
            [b"IHDR", b"IDAT", b"IEND"]
        );
        let ihdr = &parts[0].1;
        assert_eq!(u32::from_be_bytes(ihdr[..4].try_into().unwrap()), 5);
        assert_eq!(u32::from_be_bytes(ihdr[4..8].try_into().unwrap()), 3);
        assert_eq!(&ihdr[8..], &[8, 2, 0, 0, 0]);
        let data = miniz_oxide::inflate::decompress_to_vec_zlib(&parts[1].1).unwrap();
        let expected: Vec<u8> = rgb
            .chunks(w * 3)
            .flat_map(|r| std::iter::once(0u8).chain(r.iter().copied()))
            .collect();
        assert_eq!(data, expected);
    }

    #[test]
    fn rejects_mismatched_buffers() {
        assert!(encode_rgb8(2, 2, &[0; 11]).is_none());
        assert!(encode_rgb8(0, 2, &[]).is_none());
        assert!(encode_rgb8(usize::MAX, 2, &[0; 6]).is_none());
    }
}
