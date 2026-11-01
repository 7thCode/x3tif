//! Reads the few EXIF fields we need (orientation, date, exposure time) from the preview JPEG
//! embedded in an X3F file.

const TAG_ORIENTATION: u16 = 0x0112;
const TAG_DATETIME: u16 = 0x0132;
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_EXPOSURE_TIME: u16 = 0x829a;
const TAG_DATETIME_ORIGINAL: u16 = 0x9003;

const TYPE_ASCII: u16 = 2;
const TYPE_SHORT: u16 = 3;
const TYPE_RATIONAL: u16 = 5;

#[derive(Debug, Default, PartialEq)]
pub struct PreviewExif {
    /// 1..=8, as in the TIFF/EXIF Orientation tag.
    pub orientation: Option<u16>,
    pub datetime: Option<String>,
    pub datetime_original: Option<String>,
    pub exposure_time: Option<(u32, u32)>,
}

/// Parses the EXIF APP1 segment of a JPEG. Returns `None` if there is none.
pub fn from_jpeg(jpeg: &[u8]) -> Option<PreviewExif> {
    if jpeg.get(..2)? != [0xff, 0xd8] {
        return None;
    }
    let mut p = 2;
    while p + 4 <= jpeg.len() {
        if jpeg[p] != 0xff {
            return None;
        }
        let marker = jpeg[p + 1];
        // Image data starts at SOS; EXIF always precedes it.
        if marker == 0xda || marker == 0xd9 {
            return None;
        }
        let len = u16::from_be_bytes([jpeg[p + 2], jpeg[p + 3]]) as usize;
        if len < 2 {
            return None;
        }
        let payload = jpeg.get(p + 4..p + 2 + len)?;
        if marker == 0xe1 && payload.starts_with(b"Exif\0\0") {
            return parse_tiff(&payload[6..]);
        }
        p += 2 + len;
    }
    None
}

struct Tiff<'a> {
    b: &'a [u8],
    le: bool,
}

impl Tiff<'_> {
    fn u16(&self, o: usize) -> Option<u16> {
        let s = self.b.get(o..o.checked_add(2)?)?;
        Some(if self.le {
            u16::from_le_bytes([s[0], s[1]])
        } else {
            u16::from_be_bytes([s[0], s[1]])
        })
    }

    fn u32(&self, o: usize) -> Option<u32> {
        let s = self.b.get(o..o.checked_add(4)?)?;
        let a = [s[0], s[1], s[2], s[3]];
        Some(if self.le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        })
    }

    /// Entries of the IFD at `at` as (tag, type, count, offset of the 4 byte value field).
    fn entries(&self, at: usize) -> Vec<(u16, u16, u32, usize)> {
        let n = self.u16(at).unwrap_or(0) as usize;
        (0..n)
            .filter_map(|i| {
                let e = at + 2 + i * 12;
                Some((self.u16(e)?, self.u16(e + 2)?, self.u32(e + 4)?, e + 8))
            })
            .collect()
    }

    /// Where the entry's data lives: inline in the value field, or at the offset stored there.
    fn data_at(&self, size: usize, field: usize) -> Option<usize> {
        if size <= 4 {
            Some(field)
        } else {
            Some(self.u32(field)? as usize)
        }
    }

    fn ascii(&self, (_, ty, count, field): (u16, u16, u32, usize)) -> Option<String> {
        if ty != TYPE_ASCII {
            return None;
        }
        let at = self.data_at(count as usize, field)?;
        let raw = self.b.get(at..at.checked_add(count as usize)?)?;
        let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
        let s = String::from_utf8_lossy(&raw[..end]).trim().to_owned();
        (!s.is_empty()).then_some(s)
    }

    fn short(&self, (_, ty, count, field): (u16, u16, u32, usize)) -> Option<u16> {
        (ty == TYPE_SHORT && count >= 1)
            .then(|| self.u16(field))
            .flatten()
    }

    fn rational(&self, (_, ty, count, field): (u16, u16, u32, usize)) -> Option<(u32, u32)> {
        if ty != TYPE_RATIONAL || count < 1 {
            return None;
        }
        let at = self.data_at(8, field)?;
        let (n, d) = (self.u32(at)?, self.u32(at + 4)?);
        (n > 0 && d > 0).then_some((n, d))
    }
}

fn parse_tiff(b: &[u8]) -> Option<PreviewExif> {
    let le = match b.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let t = Tiff { b, le };
    if t.u16(2)? != 42 {
        return None;
    }
    let mut out = PreviewExif::default();
    let mut exif_ifd = None;
    for e in t.entries(t.u32(4)? as usize) {
        match e.0 {
            TAG_ORIENTATION => out.orientation = t.short(e).filter(|o| (1..=8).contains(o)),
            TAG_DATETIME => out.datetime = t.ascii(e),
            TAG_EXIF_IFD => exif_ifd = t.u32(e.3),
            _ => {}
        }
    }
    if let Some(at) = exif_ifd {
        for e in t.entries(at as usize) {
            match e.0 {
                TAG_EXPOSURE_TIME => out.exposure_time = t.rational(e),
                TAG_DATETIME_ORIGINAL => out.datetime_original = t.ascii(e),
                _ => {}
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A big-endian EXIF block: IFD0 {Orientation=6, DateTime, ExifIFD}, ExifIFD {ExposureTime, DateTimeOriginal}.
    fn sample_tiff() -> Vec<u8> {
        let mut b = b"MM\0*".to_vec();
        b.extend_from_slice(&8u32.to_be_bytes());
        // IFD0 at 8: 3 entries -> 2 + 36 + 4 = 42 bytes, data from 50
        b.extend_from_slice(&3u16.to_be_bytes());
        let mut e = |tag: u16, ty: u16, count: u32, val: [u8; 4]| {
            b.extend_from_slice(&tag.to_be_bytes());
            b.extend_from_slice(&ty.to_be_bytes());
            b.extend_from_slice(&count.to_be_bytes());
            b.extend_from_slice(&val);
        };
        e(0x0112, 3, 1, [0, 6, 0, 0]);
        e(0x0132, 2, 20, 50u32.to_be_bytes());
        e(0x8769, 4, 1, 70u32.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(b"2023:02:25 10:20:30\0"); // at 50
                                                       // ExifIFD at 70: 2 entries -> 2 + 24 + 4 = 30, data from 100
        b.extend_from_slice(&2u16.to_be_bytes());
        let mut e = |tag: u16, ty: u16, count: u32, val: [u8; 4]| {
            b.extend_from_slice(&tag.to_be_bytes());
            b.extend_from_slice(&ty.to_be_bytes());
            b.extend_from_slice(&count.to_be_bytes());
            b.extend_from_slice(&val);
        };
        e(0x829a, 5, 1, 100u32.to_be_bytes());
        e(0x9003, 2, 20, 108u32.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&1u32.to_be_bytes()); // at 100
        b.extend_from_slice(&250u32.to_be_bytes());
        b.extend_from_slice(b"2023:02:25 10:20:29\0"); // at 108
        b
    }

    fn jpeg_with(tiff: &[u8]) -> Vec<u8> {
        let mut j = vec![0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0, 0xff, 0xe1];
        j.extend_from_slice(&((tiff.len() + 8) as u16).to_be_bytes());
        j.extend_from_slice(b"Exif\0\0");
        j.extend_from_slice(tiff);
        j.extend_from_slice(&[0xff, 0xda, 0, 2]);
        j
    }

    #[test]
    fn reads_orientation_dates_and_exposure() {
        let e = from_jpeg(&jpeg_with(&sample_tiff())).unwrap();
        assert_eq!(e.orientation, Some(6));
        assert_eq!(e.datetime.as_deref(), Some("2023:02:25 10:20:30"));
        assert_eq!(e.datetime_original.as_deref(), Some("2023:02:25 10:20:29"));
        assert_eq!(e.exposure_time, Some((1, 250)));
    }

    #[test]
    fn rejects_non_jpeg_and_missing_exif() {
        assert_eq!(from_jpeg(b"not a jpeg"), None);
        assert_eq!(from_jpeg(&[0xff, 0xd8, 0xff, 0xda, 0, 2]), None);
    }

    #[test]
    fn truncated_data_does_not_panic() {
        let j = jpeg_with(&sample_tiff());
        for n in 0..j.len() {
            let _ = from_jpeg(&j[..n]);
        }
    }

    #[test]
    fn out_of_range_orientation_is_ignored() {
        let mut t = sample_tiff();
        t[8 + 2 + 8 + 1] = 9; // Orientation value 6 -> 9
        let e = from_jpeg(&jpeg_with(&t)).unwrap();
        assert_eq!(e.orientation, None);
    }
}
