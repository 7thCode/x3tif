//! X3F → 16 bit TIFF conversion, either as raw sensor values or developed to sRGB.

use crate::camf::{Camf, Value};
use crate::develop::{self, Developer, Exposure, Mode};
use crate::exif::{self, PreviewExif};
use crate::finish::Finisher;
use crate::tiff::{self, Compression, Exif, Metadata};
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
pub fn x3f_to_tiff<W: Write>(data: Vec<u8>, out: &mut W, mode: Mode) -> Result<(), ConvertError> {
    let x3f = X3f::parse(data)?;
    let props = x3f.properties()?;
    // Metadata is best effort: an unreadable CAMF must not block the conversion.
    let camf = x3f.camf().ok().flatten().unwrap_or_default();
    let preview = x3f
        .preview_jpegs()
        .find_map(exif::from_jpeg)
        .unwrap_or_default();
    let mut raw = x3f.decode()?;
    // Raw output stays exactly what the sensor read; only the developed look hides its defects.
    if matches!(mode, Mode::Developed(..)) {
        develop::repair_highlight_pixels(&mut raw, &camf);
        develop::repair_autofocus_pixels(&mut raw, &camf);
    }

    let (full_width, full_height) = match &raw {
        Raw::Merrill { width, height, .. } => (*width, *height),
        // See the Quattro branch below: the output covers what the low layers cover.
        Raw::Quattro {
            low_width,
            low_height,
            top_width,
            top_height,
            ..
        } => (
            (*top_width).min(2 * low_width),
            (*top_height).min(2 * low_height),
        ),
    };
    let area = active_area(&camf, full_width, full_height);

    // Per-row finishing step: shift to the 16 bit range (raw) or develop.
    let mut ev = [0.0; 3];
    let developer = match mode {
        Mode::Developed(exposure, tuning) => {
            let mut d =
                Developer::new(&raw, &camf, x3f.header_white_balance().as_deref(), &tuning)?;
            ev = match exposure {
                Exposure::Ev(ev) => [ev; 3],
                Exposure::Auto => auto_exposure(&x3f, &d, &raw, &area),
            };
            d.set_exposure_ev(ev);
            Some(d)
        }
        Mode::Raw => None,
    };
    let shift = develop::raw_shift(&camf);
    let finish = |row: &mut [u16]| match &developer {
        Some(d) => d.apply(row),
        None => {
            if shift > 0 {
                for v in row {
                    *v = (u32::from(*v) << shift).min(u32::from(u16::MAX)) as u16;
                }
            }
        }
    };

    let output = match (&developer, shift) {
        (Some(d), _) => Output::Developed(d.white_balance.clone(), ev),
        (None, s) => Output::Raw(1 << s),
    };
    let meta = metadata(&props, &camf, &preview, &output);
    match raw {
        Raw::Merrill { width, rgb, .. } => {
            tiff::write_rgb16(out, area.width, area.height, &meta, |y, row| {
                let at = ((y + area.y) * width + area.x) * 3;
                row.copy_from_slice(&rgb[at..at + area.width * 3]);
                finish(row);
            })?;
        }
        Raw::Quattro {
            low_width,
            low_height,
            low,
            top_width,
            top_height,
            top,
        } => {
            // The top layer is wider than the area the lower layers cover; as in x3f_tools'
            // Quattro expansion, the output is limited to that area (both share the origin).
            let width = top_width.min(2 * low_width);
            let height = top_height.min(2 * low_height);
            let xs = taps(width, low_width);
            let ys = taps(height, low_height);
            // Blue on the low layers' grid, for the developed picture's detail transfer.
            let blue_low = developer.as_ref().map(|_| {
                let mut plane = vec![0u16; low_width * low_height];
                for (j, row) in plane.chunks_exact_mut(low_width).enumerate() {
                    for (i, v) in row.iter_mut().enumerate() {
                        let at = |dy: usize, dx: usize| {
                            let y = (2 * j + dy).min(height - 1);
                            let x = (2 * i + dx).min(width - 1);
                            top[y * top_width + x] as u32
                        };
                        *v = ((at(0, 0) + at(0, 1) + at(1, 0) + at(1, 1) + 2) / 4) as u16;
                    }
                }
                plane
            });
            let mut develop_row = |y: usize, row: &mut [u16]| {
                let y = y + area.y;
                let (y0, y1, wy) = ys[y];
                for (i, px) in row.chunks_exact_mut(3).enumerate() {
                    let x = i + area.x;
                    let (x0, x1, wx) = xs[x];
                    // Bilinear with quarter-pixel weights.
                    let blend = |at: &dyn Fn(usize, usize) -> u32| {
                        let top_row = at(y0, x0) * (4 - wx) + at(y0, x1) * wx;
                        let bot_row = at(y1, x0) * (4 - wx) + at(y1, x1) * wx;
                        ((top_row * (4 - wy) + bot_row * wy + 8) / 16) as u16
                    };
                    for c in 0..2 {
                        px[c] = blend(&|yy, xx| low[(yy * low_width + xx) * 3 + c] as u32);
                    }
                    px[2] = top[y * top_width + x];
                    if let (Some(d), Some(plane)) = (&developer, &blue_low) {
                        d.transfer_detail(px, blend(&|yy, xx| plane[yy * low_width + xx] as u32));
                    }
                }
                finish(row);
            };
            if developer.is_some() {
                // The developed picture is finished; the raw one stays as the sensor read it.
                let mut finisher = Finisher::new(area.width, area.height);
                tiff::write_rgb16(out, area.width, area.height, &meta, |y, row| {
                    finisher.row(y, &mut develop_row, row)
                })?;
            } else {
                tiff::write_rgb16(out, area.width, area.height, &meta, &mut develop_row)?;
            }
        }
    }
    Ok(())
}

/// Median colour (0..1, sRGB-encoded) of the camera's embedded JPEG preview, if the file has a
/// decodable one.
pub(crate) fn preview_target(x3f: &X3f) -> Option<[f64; 3]> {
    let mut previews: Vec<&[u8]> = x3f.preview_jpegs().collect();
    previews.sort_by_key(|p| std::cmp::Reverse(p.len()));
    previews.into_iter().find_map(develop::jpeg_median_rgb)
}

/// EV that makes the developed picture as bright as the camera's embedded JPEG preview (0 if the
/// file has no decodable preview). One value for all channels: matching each channel's median
/// to the preview as well overshoots (a saturated sky turns the whole picture violet).
fn auto_exposure(x3f: &X3f, developer: &Developer, raw: &Raw, area: &Area) -> [f64; 3] {
    exposure_for_target(preview_target(x3f), developer, raw, area)
}

pub(crate) fn exposure_for_target(
    target: Option<[f64; 3]>,
    developer: &Developer,
    raw: &Raw,
    area: &Area,
) -> [f64; 3] {
    let Some(target) = target else {
        return [0.0; 3];
    };
    let scene = developer.median_linear(raw, (area.x, area.y, area.width, area.height));
    if scene.iter().any(|&s| s <= 0.0) {
        return [0.0; 3];
    }
    // Luminance-weighted mean of the per-channel fits.
    const LUMA: [f64; 3] = [0.2126, 0.7152, 0.0722];
    let ev: f64 = (0..3)
        .map(|c| LUMA[c] * Developer::exposure_for(scene[c], target[c], developer.tone()))
        .sum();
    [ev; 3]
}

/// The part of the decoded image that is actual picture.
pub(crate) struct Area {
    pub(crate) x: usize,
    pub(crate) y: usize,
    pub(crate) width: usize,
    pub(crate) height: usize,
}

/// CAMF `ActiveImageArea` (inclusive, in sensor coordinates) relative to the decoded image, which
/// starts at the origin of `KeepImageArea`. Everything outside it is dark shield and margins.
/// Falls back to the whole image if the file gives no usable area.
pub(crate) fn active_area(camf: &Camf, width: usize, height: usize) -> Area {
    let whole = Area {
        x: 0,
        y: 0,
        width,
        height,
    };
    let rect = |name: &str| camf.matrix(name).filter(|m| m.len() == 4);
    let Some(active) = rect("ActiveImageArea") else {
        return whole;
    };
    let (ox, oy) = rect("KeepImageArea").map_or((0.0, 0.0), |k| (k[0], k[1]));
    let (x0, y0) = (active[0] - ox, active[1] - oy);
    let (x1, y1) = (active[2] - ox, active[3] - oy);
    if x0 < 0.0 || y0 < 0.0 || x1 < x0 || y1 < y0 || x1 >= width as f64 || y1 >= height as f64 {
        return whole;
    }
    Area {
        x: x0 as usize,
        y: y0 as usize,
        width: (x1 - x0) as usize + 1,
        height: (y1 - y0) as usize + 1,
    }
}

/// How the pixel values were produced; recorded in the XMP so later tools can undo the scaling.
enum Output {
    /// Raw sensor values multiplied by this factor.
    Raw(u32),
    /// Developed with the named white balance.
    Developed(String, [f64; 3]),
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

/// Camera model from the CAMF camera id (used when the file has no PROP section).
fn model_from_id(id: u32) -> Option<&'static str> {
    Some(match id {
        77 => "dp1 Merrill",
        78 => "dp2/dp3 Merrill",
        83 => "dp0 Quattro",
        80 => "dp1 Quattro",
        81 => "dp2 Quattro",
        82 => "dp3 Quattro",
        40 => "sd Quattro",
        41 => "sd Quattro H",
        _ => return None,
    })
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// `v` as a reduced fraction with the given denominator scale.
fn ratio(v: f64, scale: u32) -> Option<(u32, u32)> {
    if !v.is_finite() || v < 0.0 {
        return None;
    }
    let n = (v * scale as f64).round() as u64;
    let g = gcd(n, scale as u64).max(1);
    Some(((n / g) as u32, (scale as u64 / g) as u32))
}

fn exposure_time(seconds: f64) -> Option<(u32, u32)> {
    if !(seconds.is_finite() && seconds > 0.0) {
        return None;
    }
    if seconds < 1.0 {
        Some((1, (1.0 / seconds).round() as u32))
    } else {
        ratio(seconds, 1000)
    }
}

/// `SH_DESC` is "1/320" for fast shutters or plain seconds ("2.5") for slow ones.
fn nominal_shutter(props: &Properties) -> Option<(u32, u32)> {
    let d = props.get("SH_DESC")?.trim();
    match d.split_once('/') {
        Some((n, den)) => Some((
            n.trim().parse().ok().filter(|&n| n > 0)?,
            den.trim().parse().ok().filter(|&d| d > 0)?,
        )),
        None => exposure_time(d.parse().ok()?),
    }
}

fn exif(props: &Properties, camf: &Camf, preview: &PreviewExif) -> Exif {
    let prop = |name: &str| props.get(name).and_then(|v| v.trim().parse::<f64>().ok());
    // PROP (Merrill) first, CAMF (Quattro) as the fallback. CAMF shutter values are not in
    // a verified unit, so exposure time comes from PROP or the preview JPEG's EXIF.
    // Prefer the nominal values the camera displays (f/5.6, 1/320) over the measured ones.
    let f_number = prop("AP_DESC")
        .or_else(|| prop("APERTURE"))
        .or_else(|| camf.number("CaptureAperture"));
    let iso = prop("ISO").or_else(|| camf.number("CaptureISO"));
    let bias = prop("EXPCOMP").or_else(|| camf.number("CaptureExpComp"));
    Exif {
        exposure_time: nominal_shutter(props)
            .or_else(|| prop("SHUTTER").and_then(exposure_time))
            .or(preview.exposure_time),
        datetime_original: preview.datetime_original.clone(),
        f_number: f_number.and_then(|v| ratio(v, 10)),
        iso: iso.map(|v| v.round().clamp(0.0, u16::MAX as f64) as u16),
        exposure_bias: bias.map(|v| ((v * 100.0).round() as i32, 100)),
        focal_length: prop("FLENGTH").and_then(|v| ratio(v, 10)),
    }
}

fn metadata(props: &Properties, camf: &Camf, preview: &PreviewExif, output: &Output) -> Metadata {
    let model = props
        .get("CAMMODEL")
        .map(|m| m.trim_start_matches("SIGMA ").to_owned())
        .or_else(|| {
            camf.number("CAMERAID")
                .and_then(|id| model_from_id(id as u32))
                .map(str::to_owned)
        });
    Metadata {
        make: Some("SIGMA".into()),
        model,
        software: Some(concat!("x3tif ", env!("CARGO_PKG_VERSION")).into()),
        datetime: preview
            .datetime
            .clone()
            .or_else(|| preview.datetime_original.clone()),
        orientation: preview.orientation,
        compression: Compression::Deflate,
        exif: exif(props, camf, preview),
        xmp: Some(xmp(props, camf, output)),
        // Only the developed picture is sRGB; raw values are the sensor's own and stay untagged.
        icc_profile: matches!(output, Output::Developed(..)).then(crate::icc::srgb),
    }
}

fn xml_escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '"' => "&quot;".into(),
            '\n' => "&#10;".into(),
            c if (c as u32) < 0x20 => String::new(),
            c => c.to_string(),
        })
        .collect()
}

fn number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{v}")
    } else {
        // CAMF floats are single precision; print them as such.
        format!("{}", v as f32)
    }
}

/// X3F PROP and CAMF entries under an `x3f:` namespace (white balance gains, colour matrices,
/// image areas, raw offset, ...) so the calibration data travels with the pixels. Large
/// per-pixel tables are left out.
fn xmp(props: &Properties, camf: &Camf, output: &Output) -> Vec<u8> {
    let how = match output {
        // Divide the samples by RawScale to get the sensor's own values back.
        Output::Raw(scale) => format!("x3f:OutputMode=\"raw\" x3f:RawScale=\"{scale}\""),
        Output::Developed(wb, ev) => format!(
            "x3f:OutputMode=\"developed\" x3f:ColorSpace=\"sRGB\" x3f:WhiteBalance=\"{}\" x3f:ExposureEV=\"{:.2},{:.2},{:.2}\"",
            xml_escape(wb),
            ev[0],
            ev[1],
            ev[2]
        ),
    };
    let mut s = format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
         <x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
         <rdf:Description rdf:about=\"\" xmlns:x3f=\"https://github.com/7thCode/x3tif/ns/1.0/\" {}>\n\
         <x3f:Properties><rdf:Bag>\n",
        how
    );
    for (k, v) in &props.0 {
        s.push_str(&format!(
            "<rdf:li x3f:name=\"{}\" x3f:value=\"{}\"/>\n",
            xml_escape(k),
            xml_escape(v)
        ));
    }
    s.push_str("</rdf:Bag></x3f:Properties>\n<x3f:Camf><rdf:Bag>\n");
    for e in &camf.0 {
        match &e.value {
            Value::Text(t) => s.push_str(&format!(
                "<rdf:li x3f:name=\"{}\" x3f:value=\"{}\"/>\n",
                xml_escape(&e.name),
                xml_escape(t)
            )),
            Value::Matrix { dims, values } if !values.is_empty() => {
                let dims: Vec<String> = dims.iter().map(usize::to_string).collect();
                let vals: Vec<String> = values.iter().map(|&v| number(v)).collect();
                s.push_str(&format!(
                    "<rdf:li x3f:name=\"{}\" x3f:dims=\"{}\" x3f:value=\"{}\"/>\n",
                    xml_escape(&e.name),
                    dims.join("x"),
                    vals.join(" ")
                ));
            }
            Value::Matrix { .. } | Value::Properties(_) => {}
        }
    }
    s.push_str(
        "</rdf:Bag></x3f:Camf>\n</rdf:Description></rdf:RDF></x:xmpmeta>\n<?xpacket end=\"w\"?>",
    );
    s.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(pairs: &[(&str, &str)]) -> Properties {
        Properties(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn exif_prefers_nominal_camera_values() {
        let p = props(&[
            ("AP_DESC", "5.6"),
            ("APERTURE", "5.65685"),
            ("SH_DESC", "1/320"),
            ("SHUTTER", "0.00301"),
            ("ISO", "100"),
            ("EXPCOMP", "+0.00"),
            ("FLENGTH", "19"),
        ]);
        let e = exif(&p, &Camf::default(), &PreviewExif::default());
        assert_eq!(e.f_number, Some((28, 5)));
        assert_eq!(e.exposure_time, Some((1, 320)));
        assert_eq!(e.iso, Some(100));
        assert_eq!(e.exposure_bias, Some((0, 100)));
        assert_eq!(e.focal_length, Some((19, 1)));
    }

    #[test]
    fn preview_exif_fills_exposure_time_and_date_when_nothing_else_has_them() {
        let preview = PreviewExif {
            exposure_time: Some((1, 250)),
            datetime_original: Some("2023:02:25 10:20:29".into()),
            ..Default::default()
        };
        let e = exif(&props(&[]), &Camf::default(), &preview);
        assert_eq!(e.exposure_time, Some((1, 250)));
        assert_eq!(e.datetime_original.as_deref(), Some("2023:02:25 10:20:29"));
        // Camera-displayed PROP values still win over the preview.
        let e = exif(&props(&[("SH_DESC", "1/320")]), &Camf::default(), &preview);
        assert_eq!(e.exposure_time, Some((1, 320)));
    }

    #[test]
    fn slow_shutter_is_written_in_seconds() {
        assert_eq!(nominal_shutter(&props(&[("SH_DESC", "2.5")])), Some((5, 2)));
        assert_eq!(nominal_shutter(&props(&[("SH_DESC", "30")])), Some((30, 1)));
        assert_eq!(nominal_shutter(&props(&[("SH_DESC", "garbage")])), None);
    }

    #[test]
    fn exif_falls_back_to_camf_when_no_properties() {
        let camf = Camf(vec![
            crate::camf::Entry {
                name: "CaptureAperture".into(),
                value: Value::Matrix {
                    dims: vec![1],
                    values: vec![5.599999904632568],
                },
            },
            crate::camf::Entry {
                name: "CaptureISO".into(),
                value: Value::Matrix {
                    dims: vec![1],
                    values: vec![100.0],
                },
            },
        ]);
        let e = exif(&props(&[]), &camf, &PreviewExif::default());
        assert_eq!(e.f_number, Some((28, 5)));
        assert_eq!(e.iso, Some(100));
        assert_eq!(e.exposure_time, None);
    }

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
        let err = x3f_to_tiff(vec![0; 64], &mut out, Mode::Raw).unwrap_err();
        assert!(matches!(err, ConvertError::X3f(Error::NotX3f)));
    }
}
