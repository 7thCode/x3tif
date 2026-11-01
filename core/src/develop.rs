//! The two output looks.
//!
//! * `Raw`: sensor values untouched, only shifted up to fill the 16 bit range (lossless).
//! * `Developed`: black level, white balance, colour matrix and the sRGB curve, using the
//!   calibration stored in the file's CAMF block.

use crate::camf::Camf;
use crate::x3f::{Error, Raw};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Raw,
    Developed,
}

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
    pub white_balance: String,
}

const CURVE_SIZE: usize = 4096;

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
        let (name, gain, cc) = [canonical_wb(&wanted), "Auto", "Sunlight"]
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
            })
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
            .map(|i| srgb_encode(i as f64 / CURVE_SIZE as f64) * 65535.0)
            .collect();
        Ok(Developer {
            black,
            white: [max; 3],
            matrix,
            curve,
            white_balance: name,
        })
    }

    /// Converts one row of interleaved raw RGB (sensor units) in place.
    pub fn apply(&self, row: &mut [u16]) {
        for px in row.chunks_exact_mut(3) {
            let n: [f64; 3] = std::array::from_fn(|c| {
                ((px[c] as f64 - self.black[c]) / (self.white[c] - self.black[c])).max(0.0)
            });
            for (c, out) in px.iter_mut().enumerate() {
                let m = &self.matrix[c];
                let lin = (m[0] * n[0] + m[1] * n[1] + m[2] * n[2]).clamp(0.0, 1.0);
                let pos = lin * CURVE_SIZE as f64;
                let i = (pos as usize).min(CURVE_SIZE - 1);
                let t = pos - i as f64;
                *out = (self.curve[i] * (1.0 - t) + self.curve[i + 1] * t).round() as u16;
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
    let mut areas: Vec<[f64; 4]> = Vec::new();
    // The bottom strip is specified incorrectly by sd Quattro H firmware.
    let skip_bottom = camf.number("CAMERAID") == Some(41.0);
    areas.extend(rect("DarkShieldTop"));
    if !skip_bottom {
        areas.extend(rect("DarkShieldBottom"));
    }
    if let Some(c) = camf.matrix("DarkShieldColRange").filter(|m| m.len() == 4) {
        areas.push([c[0], 0.0, c[1], f64::MAX]);
        areas.push([c[2], 0.0, c[3], f64::MAX]);
    }
    if areas.is_empty() {
        return Err(Error::MissingCalibration("DarkShield"));
    }

    // (width, height, scale of the area coordinates, sample accessor) per channel.
    let mut out = [0.0; 3];
    for (c, slot) in out.iter_mut().enumerate() {
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
        let (mut sum, mut count) = (0u64, 0u64);
        for a in &areas {
            let x0 = (a[0] * scale) as usize;
            let y0 = (a[1] * scale) as usize;
            let x1 = ((a[2] * scale).min(w as f64 - 1.0)) as usize;
            let y1 = ((a[3] * scale).min(h as f64 - 1.0)) as usize;
            for y in y0..=y1.min(h - 1) {
                for x in x0..=x1 {
                    sum += sample(raw, c, x, y) as u64;
                    count += 1;
                }
            }
        }
        if count == 0 {
            return Err(Error::MissingCalibration("DarkShield"));
        }
        *slot = sum as f64 / count as f64;
    }
    Ok(out)
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
        // Normalised input 0.1 on every channel; red gain 2 -> 0.2 linear, others 0.1.
        let v = 100.0 + 0.1 * (4095.0 - 100.0);
        let mut row = [v as u16; 3];
        d.apply(&mut row);
        let enc = |l: f64| (srgb_encode(l) * 65535.0).round();
        assert!((row[0] as f64 - enc(0.2)).abs() < 40.0, "{row:?}");
        assert!((row[1] as f64 - enc(0.1)).abs() < 40.0, "{row:?}");
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
