//! A minimal sRGB ICC profile (v2.1, matrix/TRC), built in code so no profile data has to ship.
//!
//! Colorants are the sRGB primaries adapted to the D50 profile connection space (Bradford), the
//! tone curve is the sRGB transfer function sampled at 1024 points.

const D50: [f64; 3] = [0.9642, 1.0, 0.8249];
const RED: [f64; 3] = [0.43607, 0.22249, 0.01392];
const GREEN: [f64; 3] = [0.38515, 0.71687, 0.09708];
const BLUE: [f64; 3] = [0.14307, 0.06061, 0.71410];
const CURVE_POINTS: usize = 1024;
const DESCRIPTION: &str = "sRGB IEC61966-2.1 (x3tif)";
const COPYRIGHT: &str = "Public domain";

fn s15f16(v: f64) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}

fn xyz_tag(v: [f64; 3]) -> Vec<u8> {
    let mut b = b"XYZ \0\0\0\0".to_vec();
    for c in v {
        b.extend_from_slice(&s15f16(c));
    }
    b
}

fn text_tag(s: &str) -> Vec<u8> {
    let mut b = b"text\0\0\0\0".to_vec();
    b.extend_from_slice(s.as_bytes());
    b.push(0);
    b
}

/// v2 `desc` tag: ASCII string, empty Unicode and ScriptCode parts.
fn desc_tag(s: &str) -> Vec<u8> {
    let mut b = b"desc\0\0\0\0".to_vec();
    b.extend_from_slice(&(s.len() as u32 + 1).to_be_bytes());
    b.extend_from_slice(s.as_bytes());
    b.push(0);
    b.extend_from_slice(&[0; 4 + 4]); // Unicode language code and count
    b.extend_from_slice(&[0; 2 + 1 + 67]); // ScriptCode code, count and filler
    b
}

fn curve_tag() -> Vec<u8> {
    let mut b = b"curv\0\0\0\0".to_vec();
    b.extend_from_slice(&(CURVE_POINTS as u32).to_be_bytes());
    for i in 0..CURVE_POINTS {
        let e = i as f64 / (CURVE_POINTS - 1) as f64;
        let l = if e <= 0.04045 {
            e / 12.92
        } else {
            ((e + 0.055) / 1.055).powf(2.4)
        };
        b.extend_from_slice(&((l * 65535.0).round() as u16).to_be_bytes());
    }
    b
}

/// The profile as bytes, ready for the TIFF ICCProfile tag.
pub fn srgb() -> Vec<u8> {
    // The three tone curves are identical, so one stored copy serves all of them.
    let curve = curve_tag();
    let tags: [(&[u8; 4], Vec<u8>); 9] = [
        (b"cprt", text_tag(COPYRIGHT)),
        (b"desc", desc_tag(DESCRIPTION)),
        (b"wtpt", xyz_tag(D50)),
        (b"rXYZ", xyz_tag(RED)),
        (b"gXYZ", xyz_tag(GREEN)),
        (b"bXYZ", xyz_tag(BLUE)),
        (b"rTRC", curve.clone()),
        (b"gTRC", Vec::new()),
        (b"bTRC", Vec::new()),
    ];

    let table_len = 4 + tags.len() * 12;
    let mut offset = 128 + table_len;
    let mut table = (tags.len() as u32).to_be_bytes().to_vec();
    let mut data = Vec::new();
    let mut curve_at = (0, 0);
    for (sig, body) in &tags {
        let (at, len) = if body.is_empty() {
            curve_at // gTRC and bTRC point at the red curve
        } else {
            let at = offset + data.len();
            data.extend_from_slice(body);
            data.resize(data.len().next_multiple_of(4), 0);
            (at, body.len())
        };
        if *sig == b"rTRC" {
            curve_at = (at, len);
        }
        table.extend_from_slice(*sig);
        table.extend_from_slice(&(at as u32).to_be_bytes());
        table.extend_from_slice(&(len as u32).to_be_bytes());
    }
    offset += data.len();

    let mut p = Vec::with_capacity(offset);
    p.extend_from_slice(&(offset as u32).to_be_bytes()); // size
    p.extend_from_slice(&[0; 4]); // preferred CMM
    p.extend_from_slice(&[2, 0x10, 0, 0]); // version 2.1
    p.extend_from_slice(b"mntrRGB XYZ ");
    for v in [2024u16, 1, 1, 0, 0, 0] {
        p.extend_from_slice(&v.to_be_bytes()); // creation date (fixed, so output is reproducible)
    }
    p.extend_from_slice(b"acsp");
    p.extend_from_slice(&[0; 4 + 4 + 4 + 4 + 8 + 4]); // platform, flags, maker, model, attributes, intent
    for c in D50 {
        p.extend_from_slice(&s15f16(c)); // illuminant
    }
    p.extend_from_slice(&[0; 4 + 16 + 28]); // creator, profile id, reserved
    p.extend_from_slice(&table);
    p.extend_from_slice(&data);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(b: &[u8], o: usize) -> usize {
        u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as usize
    }

    #[test]
    fn header_and_size_are_consistent() {
        let p = srgb();
        assert_eq!(u32_at(&p, 0), p.len());
        assert_eq!(p.len() % 4, 0);
        assert_eq!(&p[12..16], b"mntr");
        assert_eq!(&p[16..24], b"RGB XYZ ");
        assert_eq!(&p[36..40], b"acsp");
        assert_eq!(p[8], 2);
    }

    #[test]
    fn every_tag_lies_inside_the_profile() {
        let p = srgb();
        let n = u32_at(&p, 128);
        assert_eq!(n, 9);
        let mut sigs = Vec::new();
        for i in 0..n {
            let e = 132 + i * 12;
            let (off, len) = (u32_at(&p, e + 4), u32_at(&p, e + 8));
            assert!(off >= 132 + n * 12 && off % 4 == 0, "tag {i} offset {off}");
            assert!(off + len <= p.len(), "tag {i} runs past the end");
            sigs.push(p[e..e + 4].to_vec());
        }
        for want in [
            &b"rXYZ"[..],
            b"gXYZ",
            b"bXYZ",
            b"rTRC",
            b"gTRC",
            b"bTRC",
            b"wtpt",
            b"desc",
            b"cprt",
        ] {
            assert!(sigs.iter().any(|s| s == want), "missing {:?}", want);
        }
    }

    #[test]
    fn tone_curve_is_the_srgb_curve() {
        let p = srgb();
        let n = u32_at(&p, 128);
        let e = (0..n)
            .map(|i| 132 + i * 12)
            .find(|&e| &p[e..e + 4] == b"gTRC")
            .unwrap();
        let off = u32_at(&p, e + 4);
        assert_eq!(&p[off..off + 4], b"curv");
        let count = u32_at(&p, off + 8);
        assert_eq!(count, CURVE_POINTS);
        let at = |i: usize| u16::from_be_bytes([p[off + 12 + 2 * i], p[off + 13 + 2 * i]]);
        assert_eq!(at(0), 0);
        assert_eq!(at(count - 1), 65535);
        // sRGB 0.5 encodes linear 0.2140; index 511.5 sits between two samples.
        let mid = (at(511) as f64 + at(512) as f64) / 2.0 / 65535.0;
        assert!((mid - 0.2140).abs() < 0.001, "{mid}");
    }

    #[test]
    fn colorants_sum_to_the_d50_white_point() {
        for c in 0..3 {
            let sum = RED[c] + GREEN[c] + BLUE[c];
            assert!((sum - D50[c]).abs() < 0.0005, "channel {c}: {sum}");
        }
    }
}
