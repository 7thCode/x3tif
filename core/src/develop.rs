//! The two output looks.
//!
//! * `Raw`: sensor values untouched, only shifted up to fill the 16 bit range (lossless).
//! * `Developed`: black level, white balance, colour matrix, exposure and a tone curve, using the
//!   calibration stored in the file's CAMF block. The exposure defaults to whatever makes the
//!   picture as bright as the camera's own embedded JPEG preview.

use crate::camf::Camf;
use crate::x3f::{Error, Raw};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Mode {
    #[default]
    Raw,
    Developed(Exposure),
}

/// Exposure compensation for the developed output.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Exposure {
    /// Match the brightness of the embedded JPEG preview.
    #[default]
    Auto,
    /// Fixed compensation in EV (stops).
    Ev(f64),
}

/// Exposure compensation limits (EV), for both the automatic fit and explicit values.
pub const EV_RANGE: (f64, f64) = (-4.0, 4.0);

/// Sensor bit depth from CAMF (`ImageDepth`); 14 bit is what Quattro and 12 bit what Merrill report.
fn image_depth(camf: &Camf) -> Option<u32> {
    camf.number("ImageDepth")
        .map(|d| d as u32)
        .filter(|d| (1..=16).contains(d))
}

/// Left-shift that moves the sensor's full scale to 16 bit, or 0 when the depth is unknown.
pub fn raw_shift(camf: &Camf) -> u32 {
    image_depth(camf).map_or(0, |d| 16 - d)
}

pub struct Developer {
    black: [f64; 3],
    white: [f64; 3],
    /// raw-normalised RGB -> linear sRGB, with white balance gains and ISO scaling folded in.
    matrix: [[f64; 3]; 3],
    /// sRGB-encoded 16 bit value for evenly spaced linear inputs in 0..=1.
    curve: Vec<f64>,
    /// Per-channel linear gain applied before the curve (2^EV).
    gain: [f64; 3],
    pub white_balance: String,
}

const CURVE_SIZE: usize = 4096;
/// Normalised raw level where the highlight blend towards white begins.
const CLIP_START: f64 = 0.9;
/// How far the S-shaped tone curve is mixed in (0 = plain sRGB).
const TONE_STRENGTH: f64 = 0.5;

/// sRGB encoding followed by a gentle S-curve, like the contrast a camera JPEG has.
fn encode(l: f64) -> f64 {
    let e = srgb_encode(l.clamp(0.0, 1.0));
    e + TONE_STRENGTH * (e * e * (3.0 - 2.0 * e) - e)
}

fn srgb_encode(l: f64) -> f64 {
    if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// White balance names as they appear in the CAMF property lists.
fn canonical_wb(name: &str) -> &str {
    match name {
        "Shadow" => "Shade",
        "Florescent" => "Fluorescent",
        "Daylight" => "Sunlight",
        other => other,
    }
}

fn wb_from_code(code: u32) -> &'static str {
    match code {
        2 => "Sunlight",
        3 => "Shade",
        4 => "Overcast",
        5 => "Incandescent",
        6 => "Fluorescent",
        7 => "Flash",
        8 => "Custom",
        11 => "ColorTemp",
        12 => "AutoLSP",
        _ => "Auto",
    }
}

/// Photo Pro renders Merrill files with a red gain about 4.5% above the file's white balance
/// gain (fitted against its TIFF output; the camera JPEGs of four more files agree within
/// 1.038-1.044). Quattro needs no such trim (0.8%).
const MERRILL_RED_TRIM: f64 = 1.04;

/// The table's gains are relative to a green gain of about 0.855, while the per-white-balance
/// `WBGain` entries are relative to 1. Found by comparing with those entries (about 1.16) and then
/// tuned against the camera's own JPEG: with 1.17 neutral areas of dp0 Quattro shots come out
/// within 3% (1.0 would be off by 50%).
const COLOR_TEMP_GAIN_SCALE: f64 = 1.17;

/// White balance by colour temperature (Quattro "ColorTemp" setting): the gains and colour
/// matrix are interpolated from `ColorTempTableInfo` at the file's `ColorTempValue`. The table has
/// one row per temperature: kelvin, blue gain, red gain, then the 3x3 colour matrix.
fn color_temp_calibration(camf: &Camf) -> Option<(f64, [f64; 3], [f64; 9])> {
    let kelvin = camf.number("ColorTempValue")?;
    let table = camf.matrix("ColorTempTableInfo")?;
    let rows: Vec<&[f64]> = table.chunks_exact(12).collect();
    let (first, last) = (rows.first()?, rows.last()?);
    let row: [f64; 12] = if kelvin <= first[0] {
        (*first).try_into().ok()?
    } else if kelvin >= last[0] {
        (*last).try_into().ok()?
    } else {
        let i = rows.windows(2).position(|w| kelvin <= w[1][0])?;
        let (a, b) = (rows[i], rows[i + 1]);
        let t = (kelvin - a[0]) / (b[0] - a[0]);
        std::array::from_fn(|c| a[c] + (b[c] - a[c]) * t)
    };
    let scale = COLOR_TEMP_GAIN_SCALE;
    let gain = [row[2] * scale, 1.0, row[1] * scale];
    let cc: [f64; 9] = std::array::from_fn(|c| row[3 + c]);
    Some((kelvin, gain, cc))
}

impl Developer {
    /// `header_wb` is the white balance name from the X3F header (older files only).
    pub fn new(raw: &Raw, camf: &Camf, header_wb: Option<&str>) -> Result<Developer, Error> {
        let depth = image_depth(camf).ok_or(Error::MissingCalibration("ImageDepth"))?;
        let max = ((1u32 << depth) - 1) as f64;
        let black = black_levels(raw, camf)?;

        // The file's own setting first; Auto / Sunlight when that has no matrices (e.g. ColorTemp).
        let wanted = match camf.number("WhiteBalance") {
            Some(code) => wb_from_code(code as u32).to_owned(),
            None => header_wb.unwrap_or("Auto").to_owned(),
        };
        let color_temp = (wanted == "ColorTemp")
            .then(|| color_temp_calibration(camf))
            .flatten();
        let (name, gain, cc) = color_temp
            .map(|(k, gain, cc)| (format!("ColorTemp {k:.0}K"), gain.to_vec(), cc.to_vec()))
            .into_iter()
            .chain(
                [canonical_wb(&wanted), "Auto", "Sunlight"]
                    .into_iter()
                    .find_map(|wb| {
                        let gain = camf
                            .property("WhiteBalanceGains", wb)
                            .and_then(|m| camf.matrix(m))
                            .filter(|m| m.len() == 3)?;
                        let cc = camf
                            .property("WhiteBalanceColorCorrections", wb)
                            .and_then(|m| camf.matrix(m))
                            .filter(|m| m.len() == 9)?;
                        Some((wb.to_owned(), gain.to_vec(), cc.to_vec()))
                    }),
            )
            .next()
            .ok_or(Error::MissingCalibration(
                "WhiteBalanceGains / WhiteBalanceColorCorrections",
            ))?;

        let mut gain = [gain[0], gain[1], gain[2]];
        for fact in [
            "SensorAdjustmentGainFact",
            "TempGainFact",
            "FNumberGainFact",
        ] {
            if let Some(f) = camf.matrix(fact).filter(|m| m.len() == 3) {
                for (g, f) in gain.iter_mut().zip(f) {
                    *g *= f;
                }
            }
        }
        if matches!(raw, Raw::Merrill { .. }) {
            gain[0] *= MERRILL_RED_TRIM;
        }
        let iso = match (camf.number("CaptureISO"), camf.number("SensorISO")) {
            (Some(c), Some(s)) if s > 0.0 => c / s,
            _ => 1.0,
        };
        let mut matrix = [[0.0; 3]; 3];
        for (i, row) in matrix.iter_mut().enumerate() {
            for (j, m) in row.iter_mut().enumerate() {
                *m = iso * cc[i * 3 + j] * gain[j];
            }
        }
        let curve = (0..=CURVE_SIZE)
            .map(|i| encode(i as f64 / CURVE_SIZE as f64) * 65535.0)
            .collect();
        Ok(Developer {
            black,
            white: [max; 3],
            matrix,
            curve,
            gain: [1.0; 3],
            white_balance: name,
        })
    }

    /// Exposure compensation per channel (R, G, B) in EV.
    pub fn set_exposure_ev(&mut self, ev: [f64; 3]) {
        self.gain = ev.map(|e| 2f64.powf(e.clamp(EV_RANGE.0, EV_RANGE.1)));
    }

    /// Per-channel median of the linear output (before exposure) in the area
    /// `(x, y, width, height)`, sampled on a coarse grid.
    pub fn median_linear(&self, raw: &Raw, area: (usize, usize, usize, usize)) -> [f64; 3] {
        const GRID: usize = 96;
        let (x0, y0, w, h) = area;
        let mut chans: [Vec<f64>; 3] = std::array::from_fn(|_| Vec::with_capacity(GRID * GRID));
        for gy in 0..GRID {
            for gx in 0..GRID {
                let px = sample_rgb(raw, x0 + gx * w / GRID, y0 + gy * h / GRID);
                let n: [f64; 3] = std::array::from_fn(|c| {
                    ((px[c] as f64 - self.black[c]) / (self.white[c] - self.black[c])).max(0.0)
                });
                for (c, v) in chans.iter_mut().enumerate() {
                    let m = &self.matrix[c];
                    v.push((m[0] * n[0] + m[1] * n[1] + m[2] * n[2]).max(0.0));
                }
            }
        }
        chans.map(|mut v| {
            v.sort_by(|a, b| a.total_cmp(b));
            v[v.len() / 2]
        })
    }

    /// The EV that renders a scene of median luminance `scene` at the encoded level `target`.
    pub fn exposure_for(scene: f64, target: f64) -> f64 {
        let (mut lo, mut hi) = EV_RANGE;
        for _ in 0..40 {
            let mid = (lo + hi) / 2.0;
            if encode(scene * 2f64.powf(mid)) < target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (lo + hi) / 2.0
    }

    /// Converts one row of interleaved raw RGB (sensor units) in place.
    pub fn apply(&self, row: &mut [u16]) {
        for px in row.chunks_exact_mut(3) {
            let n: [f64; 3] = std::array::from_fn(|c| {
                ((px[c] as f64 - self.black[c]) / (self.white[c] - self.black[c])).max(0.0)
            });
            let mut lin: [f64; 3] = std::array::from_fn(|c| {
                let m = &self.matrix[c];
                ((m[0] * n[0] + m[1] * n[1] + m[2] * n[2]) * self.gain[c]).clamp(0.0, 1.0)
            });
            // A saturated channel under-reports its colour, which turns bright skies yellow-green.
            // Blend those pixels towards white as the brightest channel nears saturation.
            let peak = n.iter().cloned().fold(0.0, f64::max);
            let t = ((peak - CLIP_START) / (1.0 - CLIP_START)).clamp(0.0, 1.0);
            if t > 0.0 {
                let top = lin.iter().cloned().fold(0.0, f64::max);
                for l in &mut lin {
                    *l += (top - *l) * t;
                }
            }
            for (out, l) in px.iter_mut().zip(lin) {
                let pos = l * CURVE_SIZE as f64;
                let i = (pos as usize).min(CURVE_SIZE - 1);
                let f = pos - i as f64;
                *out = (self.curve[i] * (1.0 - f) + self.curve[i + 1] * f).round() as u16;
            }
        }
    }
}

/// Per-channel black level: the mean of the dark-shielded areas (top/bottom strips and the two
/// side column ranges), measured on the data itself as x3f_tools does.
fn black_levels(raw: &Raw, camf: &Camf) -> Result<[f64; 3], Error> {
    let rect = |name: &str| {
        camf.matrix(name)
            .filter(|m| m.len() == 4)
            .map(|m| [m[0], m[1], m[2], m[3]])
    };
    // (is a rectangle, area) -- the column ranges are only trusted if they agree with the
    // rectangles: some files (DP1 Merrill) list a column range that is not actually shielded.
    let mut areas: Vec<(bool, [f64; 4])> = Vec::new();
    // The bottom strip is specified incorrectly by sd Quattro H firmware.
    let skip_bottom = camf.number("CAMERAID") == Some(41.0);
    areas.extend(rect("DarkShieldTop").map(|a| (true, a)));
    if !skip_bottom {
        areas.extend(rect("DarkShieldBottom").map(|a| (true, a)));
    }
    if let Some(c) = camf.matrix("DarkShieldColRange").filter(|m| m.len() == 4) {
        areas.push((false, [c[0], 0.0, c[1], f64::MAX]));
        areas.push((false, [c[2], 0.0, c[3], f64::MAX]));
    }
    if areas.is_empty() {
        return Err(Error::MissingCalibration("DarkShield"));
    }

    let mut out = [0.0; 3];
    for (c, slot) in out.iter_mut().enumerate() {
        // Area coordinates are in full-resolution pixels; the low Quattro layers are half size.
        let (w, h, scale) = match raw {
            Raw::Merrill { width, height, .. } => (*width, *height, 1.0),
            Raw::Quattro {
                low_width,
                low_height,
                top_width,
                top_height,
                ..
            } => {
                if c < 2 {
                    (*low_width, *low_height, 0.5)
                } else {
                    (*top_width, *top_height, 1.0)
                }
            }
        };
        // (is a rectangle, sum, count) per area.
        let mut stats: Vec<(bool, u64, u64)> = Vec::new();
        for (is_rect, a) in &areas {
            let x0 = (a[0] * scale) as usize;
            let y0 = (a[1] * scale) as usize;
            let x1 = ((a[2] * scale).min(w as f64 - 1.0)) as usize;
            let y1 = ((a[3] * scale).min(h as f64 - 1.0)) as usize;
            let (mut sum, mut count) = (0u64, 0u64);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    sum += sample(raw, c, x, y) as u64;
                    count += 1;
                }
            }
            if count > 0 {
                stats.push((*is_rect, sum, count));
            }
        }
        let pooled = |keep: &dyn Fn(&(bool, u64, u64)) -> bool| {
            let (s, n) = stats
                .iter()
                .filter(|a| keep(a))
                .fold((0, 0), |t, a| (t.0 + a.1, t.1 + a.2));
            (n > 0).then(|| s as f64 / n as f64)
        };
        let reference = pooled(&|a| a.0).or_else(|| pooled(&|_| true));
        let Some(reference) = reference else {
            return Err(Error::MissingCalibration("DarkShield"));
        };
        let tolerance = reference * 0.01 + 2.0;
        let trusted =
            |a: &(bool, u64, u64)| a.0 || (a.1 as f64 / a.2 as f64 - reference).abs() <= tolerance;
        *slot = pooled(&trusted).unwrap_or(reference);
    }
    Ok(out)
}

fn sample_rgb(raw: &Raw, x: usize, y: usize) -> [u16; 3] {
    match raw {
        Raw::Merrill { width, height, .. } => {
            let (x, y) = (x.min(width - 1), y.min(height - 1));
            std::array::from_fn(|c| sample(raw, c, x, y))
        }
        Raw::Quattro {
            low_width,
            low_height,
            top_width,
            top_height,
            ..
        } => {
            let lo = |v: usize, max: usize| (v / 2).min(max - 1);
            [
                sample(raw, 0, lo(x, *low_width), lo(y, *low_height)),
                sample(raw, 1, lo(x, *low_width), lo(y, *low_height)),
                sample(raw, 2, x.min(top_width - 1), y.min(top_height - 1)),
            ]
        }
    }
}

/// Per-channel median (0..1, sRGB-encoded) of a JPEG, decoded at reduced size.
pub fn jpeg_median_rgb(jpeg: &[u8]) -> Option<[f64; 3]> {
    let mut dec = jpeg_decoder::Decoder::new(jpeg);
    dec.read_info().ok()?;
    dec.scale(512, 512).ok()?;
    let pixels = dec.decode().ok()?;
    let info = dec.info()?;
    let mut chans: [Vec<u8>; 3] = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => {
            std::array::from_fn(|c| pixels.chunks_exact(3).map(|p| p[c]).collect())
        }
        jpeg_decoder::PixelFormat::L8 => std::array::from_fn(|_| pixels.clone()),
        _ => return None,
    };
    if chans[0].is_empty() {
        return None;
    }
    for v in &mut chans {
        v.sort_unstable();
    }
    Some(chans.map(|v| v[v.len() / 2] as f64 / 255.0))
}

fn sample(raw: &Raw, c: usize, x: usize, y: usize) -> u16 {
    match raw {
        Raw::Merrill { width, rgb, .. } => rgb[(y * width + x) * 3 + c],
        Raw::Quattro {
            low_width,
            low,
            top_width,
            top,
            ..
        } => {
            if c < 2 {
                low[(y * low_width + x) * 3 + c]
            } else {
                top[y * top_width + x]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camf::{Entry, Value};

    fn m(name: &str, values: &[f64]) -> Entry {
        Entry {
            name: name.into(),
            value: Value::Matrix {
                dims: vec![values.len()],
                values: values.to_vec(),
            },
        }
    }

    fn props(name: &str, pairs: &[(&str, &str)]) -> Entry {
        Entry {
            name: name.into(),
            value: Value::Properties(
                pairs
                    .iter()
                    .map(|(a, b)| (a.to_string(), b.to_string()))
                    .collect(),
            ),
        }
    }

    /// 4x4 Merrill-style image: black 100 everywhere except a lit pixel.
    fn raw() -> Raw {
        let mut rgb = vec![100u16; 4 * 4 * 3];
        rgb[(2 * 4 + 2) * 3..(2 * 4 + 2) * 3 + 3].copy_from_slice(&[1100, 2100, 4095]);
        Raw::Merrill {
            width: 4,
            height: 4,
            rgb,
        }
    }

    fn camf(wb_code: f64) -> Camf {
        Camf(vec![
            m("ImageDepth", &[12.0]),
            m("DarkShieldTop", &[0.0, 0.0, 3.0, 0.0]),
            m("WhiteBalance", &[wb_code]),
            props(
                "WhiteBalanceGains",
                &[("Auto", "AutoGain"), ("Sunlight", "SunGain")],
            ),
            props(
                "WhiteBalanceColorCorrections",
                &[("Auto", "AutoCC"), ("Sunlight", "SunCC")],
            ),
            m("AutoGain", &[2.0, 1.0, 1.0]),
            m("SunGain", &[1.0, 1.0, 1.0]),
            m("AutoCC", &[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]),
            m("SunCC", &[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]),
        ])
    }

    #[test]
    fn raw_shift_fills_the_16_bit_range() {
        assert_eq!(raw_shift(&Camf(vec![m("ImageDepth", &[14.0])])), 2);
        assert_eq!(raw_shift(&Camf(vec![m("ImageDepth", &[12.0])])), 4);
        assert_eq!(raw_shift(&Camf::default()), 0);
    }

    #[test]
    fn black_is_measured_and_subtracted_and_white_maps_to_full_scale() {
        let d = Developer::new(&raw(), &camf(1.0), None).unwrap();
        assert_eq!(d.black, [100.0; 3]);
        let mut row = [100u16, 100, 100, 4095, 4095, 4095];
        d.apply(&mut row);
        assert_eq!(&row[..3], &[0, 0, 0]); // black stays black
        assert_eq!(row[4], 65535); // white, green gain 1
        assert_eq!(row[3], 65535); // red gain 2 clips
    }

    #[test]
    fn white_balance_gain_scales_red_before_the_curve() {
        let d = Developer::new(&raw(), &camf(1.0), None).unwrap();
        // Normalised input 0.1 on every channel; red gain 2 (times the Merrill trim) -> 0.2 linear, others 0.1.
        let v = 100.0 + 0.1 * (4095.0 - 100.0);
        let mut row = [v as u16; 3];
        d.apply(&mut row);
        let enc = |l: f64| (encode(l) * 65535.0).round();
        assert!(
            (row[0] as f64 - enc(0.2 * MERRILL_RED_TRIM)).abs() < 40.0,
            "{row:?}"
        );
        assert!((row[1] as f64 - enc(0.1)).abs() < 40.0, "{row:?}");
    }

    #[test]
    fn exposure_for_inverts_the_curve() {
        for target in [0.2, 0.5, 0.8] {
            let ev = Developer::exposure_for(0.1, target);
            assert!(
                (encode(0.1 * 2f64.powf(ev)) - target).abs() < 1e-6,
                "{target} -> {ev}"
            );
        }
        // A scene already at the target needs no compensation.
        assert!(Developer::exposure_for(0.18, encode(0.18)).abs() < 1e-6);
    }

    #[test]
    fn one_ev_doubles_linear_light() {
        let mut d = Developer::new(&raw(), &camf(1.0), None).unwrap();
        let v = 100.0 + 0.05 * (4095.0 - 100.0);
        let mut base = [v as u16; 3];
        d.apply(&mut base);
        d.set_exposure_ev([1.0; 3]);
        let mut up = [v as u16; 3];
        d.apply(&mut up);
        // Green has gain 1: 0.05 -> 0.10 linear.
        assert!(
            (up[1] as f64 - encode(0.1) * 65535.0).abs() < 60.0,
            "{up:?}"
        );
        assert!(up[1] > base[1]);
    }

    #[test]
    fn tone_curve_keeps_black_and_white() {
        assert_eq!(encode(0.0), 0.0);
        assert!((encode(1.0) - 1.0).abs() < 1e-12);
        assert!(encode(0.3) < srgb_encode(0.3) || encode(0.9) > srgb_encode(0.9));
    }

    fn color_temp_camf(kelvin: f64) -> Camf {
        let mut c = camf(11.0);
        c.0.push(m("ColorTempValue", &[kelvin]));
        c.0.push(Entry {
            name: "ColorTempTableInfo".into(),
            value: Value::Matrix {
                dims: vec![2, 12],
                values: vec![
                    // kelvin, blue gain, red gain, colour matrix (rows sum to 1)
                    4000.0, 1.0, 0.8, 3.0, -3.0, 1.0, -2.0, 4.0, -1.0, 0.6, -4.6, 5.0, //
                    6000.0, 0.8, 1.0, 3.0, -3.0, 1.0, -2.0, 4.0, -1.0, 0.6, -4.6, 5.0,
                ],
            },
        });
        c
    }

    #[test]
    fn color_temp_interpolates_the_table() {
        let (k, gain, cc) = color_temp_calibration(&color_temp_camf(5000.0)).unwrap();
        assert_eq!(k, 5000.0);
        // Halfway: red 0.9, blue 0.9, each scaled to the WBGain convention; green stays 1.
        assert!((gain[0] - 0.9 * COLOR_TEMP_GAIN_SCALE).abs() < 1e-9);
        assert!((gain[2] - 0.9 * COLOR_TEMP_GAIN_SCALE).abs() < 1e-9);
        assert_eq!(gain[1], 1.0);
        assert_eq!(&cc[..3], &[3.0, -3.0, 1.0]);
        // Outside the table the nearest row is used.
        let (_, low, _) = color_temp_calibration(&color_temp_camf(1000.0)).unwrap();
        assert!((low[0] - 0.8 * COLOR_TEMP_GAIN_SCALE).abs() < 1e-9);
    }

    #[test]
    fn color_temp_setting_uses_the_table_instead_of_auto() {
        let d = Developer::new(&raw(), &color_temp_camf(5000.0), None).unwrap();
        assert_eq!(d.white_balance, "ColorTemp 5000K");
        // Without the table it falls back to Auto as before.
        let d = Developer::new(&raw(), &camf(11.0), None).unwrap();
        assert_eq!(d.white_balance, "Auto");
    }

    #[test]
    fn unknown_white_balance_falls_back_to_auto() {
        // 11 = ColorTemp has no matrices in the lists.
        let d = Developer::new(&raw(), &camf(11.0), None).unwrap();
        assert_eq!(d.white_balance, "Auto");
    }

    #[test]
    fn missing_calibration_is_reported() {
        let err = Developer::new(&raw(), &Camf::default(), None)
            .err()
            .unwrap();
        assert!(matches!(err, Error::MissingCalibration(_)));
    }
}
