//! X3F → 16 bit TIFF conversion (raw sensor values, no development).

use crate::tiff::{self, Metadata};
use crate::x3f::{Error, Properties, Raw, X3f};
use std::io::{self, Write};

#[derive(Debug)]
pub enum ConvertError {
    X3f(Error),
    Io(io::Error),
}

impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConvertError::X3f(e) => e.fmt(f),
            ConvertError::Io(e) => write!(f, "入出力エラー: {e}"),
        }
    }
}

impl std::error::Error for ConvertError {}

impl From<Error> for ConvertError {
    fn from(e: Error) -> Self {
        ConvertError::X3f(e)
    }
}

impl From<io::Error> for ConvertError {
    fn from(e: io::Error) -> Self {
        ConvertError::Io(e)
    }
}

/// Converts an X3F file image (bytes) into a TIFF written to `out`.
pub fn x3f_to_tiff<W: Write>(data: Vec<u8>, out: &mut W) -> Result<(), ConvertError> {
    let x3f = X3f::parse(data)?;
    let props = x3f.properties()?;
    let meta = metadata(&props);
    match x3f.decode()? {
        Raw::Merrill { width, height, rgb } => {
            tiff::write_rgb16(out, width, height, &meta, |y, row| {
                row.copy_from_slice(&rgb[y * width * 3..(y + 1) * width * 3]);
            })?;
        }
        Raw::Quattro { low_width, low_height, low, top_width, top_height, top } => {
            // The top layer is wider than the area the lower layers cover; as in x3f_tools'
            // Quattro expansion, the output is limited to that area (both share the origin).
            let width = top_width.min(2 * low_width);
            let height = top_height.min(2 * low_height);
            let xs = taps(width, low_width);
            let ys = taps(height, low_height);
            tiff::write_rgb16(out, width, height, &meta, |y, row| {
                let (y0, y1, wy) = ys[y];
                for (x, px) in row.chunks_exact_mut(3).enumerate() {
                    let (x0, x1, wx) = xs[x];
                    for c in 0..2 {
                        let at = |yy: usize, xx: usize| low[(yy * low_width + xx) * 3 + c] as u32;
                        // Bilinear with quarter-pixel weights.
                        let top_row = at(y0, x0) * (4 - wx) + at(y0, x1) * wx;
                        let bot_row = at(y1, x0) * (4 - wx) + at(y1, x1) * wx;
                        px[c] = ((top_row * (4 - wy) + bot_row * wy + 8) / 16) as u16;
                    }
                    px[2] = top[y * top_width + x];
                }
            })?;
        }
    }
    Ok(())
}

/// For each full-resolution coordinate, the two neighbouring low-resolution samples and the
/// weight (in quarters) of the second one. Each low sample covers two full-resolution samples.
fn taps(full: usize, low: usize) -> Vec<(usize, usize, u32)> {
    let last = low - 1;
    (0..full)
        .map(|i| {
            let k = i / 2;
            if i % 2 == 0 {
                (k.saturating_sub(1).min(last), k.min(last), 3)
            } else {
                (k.min(last), (k + 1).min(last), 1)
            }
        })
        .collect()
}

fn metadata(props: &Properties) -> Metadata {
    Metadata {
        make: Some("SIGMA".into()),
        model: props.get("CAMMODEL").map(str::to_owned),
        software: Some(concat!("x3tif ", env!("CARGO_PKG_VERSION")).into()),
        datetime: None,
        xmp: Some(xmp(props)),
    }
}

fn xml_escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '"' => "&quot;".into(),
            c if (c as u32) < 0x20 => String::new(),
            c => c.to_string(),
        })
        .collect()
}

/// All X3F PROP key/values under an `x3f:` namespace so nothing is lost.
fn xmp(props: &Properties) -> Vec<u8> {
    let mut s = String::from(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
         <x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
         <rdf:Description rdf:about=\"\" xmlns:x3f=\"https://github.com/7thCode/x3tif/ns/1.0/\">\n\
         <x3f:Properties><rdf:Bag>\n",
    );
    for (k, v) in &props.0 {
        s.push_str(&format!(
            "<rdf:li x3f:name=\"{}\" x3f:value=\"{}\"/>\n",
            xml_escape(k),
            xml_escape(v)
        ));
    }
    s.push_str("</rdf:Bag></x3f:Properties>\n</rdf:Description></rdf:RDF></x:xmpmeta>\n<?xpacket end=\"w\"?>");
    s.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taps_interpolate_with_origin_aligned_quarter_weights() {
        let t = taps(6, 3);
        assert_eq!(t[0], (0, 0, 3)); // clamped at the left edge
        assert_eq!(t[1], (0, 1, 1));
        assert_eq!(t[2], (0, 1, 3));
        assert_eq!(t[5], (2, 2, 1)); // clamped at the right edge
    }

    #[test]
    fn rejects_non_x3f_input() {
        let mut out = Vec::new();
        let err = x3f_to_tiff(vec![0; 64], &mut out).unwrap_err();
        assert!(matches!(err, ConvertError::X3f(Error::NotX3f)));
    }
}
