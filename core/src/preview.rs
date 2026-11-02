//! A reduced-size developed picture for the adjustment screen. The file is decoded once
//! ([`Loaded`]); every change of the settings then only re-develops a few hundred thousand
//! sampled pixels.

use crate::camf::Camf;
use crate::convert::{self, active_area, exposure_for_target, Area, ConvertError};
use crate::develop::{self, Developer, Exposure, Tuning};
use crate::exif;
use crate::x3f::{Error, Raw, X3f};

/// Sub-samples per output pixel and axis; their developed values are averaged.
const SUBSAMPLES: usize = 2;
/// Bounds for the longer side of the preview, in pixels.
pub const MAX_SIDE_RANGE: (usize, usize) = (64, 4096);

/// A decoded X3F, ready to be rendered with any settings.
pub struct Loaded {
    raw: Raw,
    camf: Camf,
    header_wb: Option<String>,
    area: Area,
    /// Median colour of the camera's own JPEG preview, for the automatic exposure.
    target: Option<[f64; 3]>,
    orientation: u16,
}

/// 8 bit RGB, row-major.
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

pub struct Rendered {
    pub image: Image,
    /// The exposure used, in EV (the automatic one when `Exposure::Auto` was asked for).
    pub exposure_ev: f64,
    /// The white balance actually used.
    pub white_balance: String,
}

impl Loaded {
    pub fn load(data: Vec<u8>) -> Result<Loaded, ConvertError> {
        let x3f = X3f::parse(data)?;
        let camf = x3f.camf().ok().flatten().unwrap_or_default();
        let orientation = x3f
            .preview_jpegs()
            .find_map(exif::from_jpeg)
            .and_then(|e| e.orientation)
            .unwrap_or(1);
        let mut raw = x3f.decode()?;
        develop::repair_highlight_pixels(&mut raw, &camf);
        develop::repair_autofocus_pixels(&mut raw, &camf);
        let (w, h) = match &raw {
            Raw::Merrill { width, height, .. } => (*width, *height),
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
        Ok(Loaded {
            area: active_area(&camf, w, h),
            target: convert::preview_target(&x3f),
            header_wb: x3f.header_white_balance(),
            raw,
            camf,
            orientation,
        })
    }

    /// Size of the full picture in pixels, as saved (before the orientation is applied).
    pub fn size(&self) -> (usize, usize) {
        (self.area.width, self.area.height)
    }

    /// The picture developed with `exposure` and `tuning`, reduced so that its longer side is
    /// `max_side` pixels (never enlarged) and turned upright according to the camera's rotation.
    pub fn render(
        &self,
        exposure: Exposure,
        tuning: &Tuning,
        max_side: usize,
    ) -> Result<Rendered, Error> {
        let max_side = max_side.clamp(MAX_SIDE_RANGE.0, MAX_SIDE_RANGE.1);
        let mut developer =
            Developer::new(&self.raw, &self.camf, self.header_wb.as_deref(), tuning)?;
        let ev = match exposure {
            Exposure::Ev(ev) => [ev; 3],
            Exposure::Auto => exposure_for_target(self.target, &developer, &self.raw, &self.area),
        };
        developer.set_exposure_ev(ev);

        let Area {
            x,
            y,
            width,
            height,
        } = self.area;
        let step = (width.max(height) as f64 / max_side as f64).max(1.0);
        let out_w = ((width as f64 / step).round() as usize).max(1);
        let out_h = ((height as f64 / step).round() as usize).max(1);

        let mut rgb = vec![0u8; out_w * out_h * 3];
        let mut line = vec![0u16; out_w * SUBSAMPLES * 3];
        let mut sum = vec![0u32; out_w * 3];
        let pos = |i: usize, extent: usize, out: usize| {
            // Centre of sub-sample `i` along an axis of `out` pixels covering `extent` source pixels.
            let t = (i as f64 + 0.5) / (out * SUBSAMPLES) as f64;
            ((t * extent as f64) as usize).min(extent - 1)
        };
        for oy in 0..out_h {
            sum.fill(0);
            for sy in 0..SUBSAMPLES {
                let src_y = y + pos(oy * SUBSAMPLES + sy, height, out_h);
                for (i, px) in line.chunks_exact_mut(3).enumerate() {
                    px.copy_from_slice(&develop::sample_rgb(
                        &self.raw,
                        x + pos(i, width, out_w),
                        src_y,
                    ));
                }
                developer.apply(&mut line);
                for (i, px) in line.chunks_exact(3).enumerate() {
                    for c in 0..3 {
                        sum[(i / SUBSAMPLES) * 3 + c] += px[c] as u32;
                    }
                }
            }
            let n = (SUBSAMPLES * SUBSAMPLES) as u32;
            for (o, s) in rgb[oy * out_w * 3..(oy + 1) * out_w * 3]
                .iter_mut()
                .zip(&sum)
            {
                // 16 bit average -> 8 bit, rounded.
                *o = ((s / n + 128) >> 8).min(255) as u8;
            }
        }
        Ok(Rendered {
            image: orient(
                Image {
                    width: out_w,
                    height: out_h,
                    rgb,
                },
                self.orientation,
            ),
            exposure_ev: ev[0],
            white_balance: developer.white_balance.clone(),
        })
    }
}

/// Turns `image` upright for the EXIF orientation 3, 6 or 8; other values leave it unchanged.
fn orient(image: Image, orientation: u16) -> Image {
    let (sw, sh) = (image.width, image.height);
    let (dw, dh) = match orientation {
        6 | 8 => (sh, sw),
        _ => (sw, sh),
    };
    if !matches!(orientation, 3 | 6 | 8) {
        return image;
    }
    let mut rgb = vec![0u8; image.rgb.len()];
    for dy in 0..dh {
        for dx in 0..dw {
            let (x, y) = match orientation {
                3 => (sw - 1 - dx, sh - 1 - dy),
                6 => (dy, sh - 1 - dx),
                _ => (sw - 1 - dy, dx),
            };
            rgb[(dy * dw + dx) * 3..][..3].copy_from_slice(&image.rgb[(y * sw + x) * 3..][..3]);
        }
    }
    Image {
        width: dw,
        height: dh,
        rgb,
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

    /// A Merrill picture of `w` x `h`: the top rows are dark (black level 100), the rest is a flat
    /// level of 1100 out of 4095.
    fn loaded(w: usize, h: usize, orientation: u16) -> Loaded {
        let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        let camf = Camf(vec![
            m("ImageDepth", &[12.0]),
            m("DarkShieldTop", &[0.0, 0.0, 3.0, 0.0]),
            m("WhiteBalance", &[1.0]),
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
            m("AutoCC", &identity),
            m("SunCC", &identity),
        ]);
        Loaded {
            raw: Raw::Merrill {
                width: w,
                height: h,
                rgb: (0..w * h * 3)
                    .map(|i| if i / 3 / w < 4 { 100 } else { 1100 })
                    .collect(),
            },
            camf,
            header_wb: None,
            area: Area {
                x: 0,
                y: 0,
                width: w,
                height: h,
            },
            target: None,
            orientation,
        }
    }

    #[test]
    fn the_longer_side_is_reduced_to_the_requested_size_and_never_enlarged() {
        let l = loaded(300, 100, 1);
        let r = l
            .render(Exposure::Ev(0.0), &Tuning::default(), 150)
            .unwrap();
        assert_eq!((r.image.width, r.image.height), (150, 50));
        assert_eq!(r.image.rgb.len(), 150 * 50 * 3);
        let r = l
            .render(Exposure::Ev(0.0), &Tuning::default(), 4096)
            .unwrap();
        assert_eq!((r.image.width, r.image.height), (300, 100));
        // Absurd sizes are clamped instead of allocating.
        let r = l
            .render(Exposure::Ev(0.0), &Tuning::default(), usize::MAX)
            .unwrap();
        assert_eq!(r.image.width, 300);
        let r = l.render(Exposure::Ev(0.0), &Tuning::default(), 0).unwrap();
        assert_eq!(r.image.width, MAX_SIDE_RANGE.0);
    }

    #[test]
    fn the_orientation_swaps_the_sides() {
        let r = loaded(300, 100, 6)
            .render(Exposure::Ev(0.0), &Tuning::default(), 150)
            .unwrap();
        assert_eq!((r.image.width, r.image.height), (50, 150));
    }

    #[test]
    fn exposure_and_white_balance_change_the_picture() {
        let l = loaded(8, 8, 1);
        let px = |e: Exposure, t: Tuning| {
            let rgb = l.render(e, &t, 64).unwrap().image.rgb;
            rgb[rgb.len() - 3..].to_vec() // bottom-right pixel, in the lit area
        };
        let base = px(Exposure::Ev(0.0), Tuning::default());
        let bright = px(Exposure::Ev(1.0), Tuning::default());
        assert!(bright[1] > base[1], "{base:?} {bright:?}");
        // The file is shot on Auto (red gain 2); Sunlight (gain 1) has less red.
        let sun = px(
            Exposure::Ev(0.0),
            Tuning {
                white_balance: Some("Sunlight"),
                ..Tuning::default()
            },
        );
        assert!(sun[0] < base[0], "{base:?} {sun:?}");
        let r = l.render(Exposure::Auto, &Tuning::default(), 64).unwrap();
        assert_eq!((r.exposure_ev, r.white_balance.as_str()), (0.0, "Auto")); // no camera preview
    }

    /// 3x2 image whose pixel (x, y) has red = 10 * (y * 3 + x).
    fn sample() -> Image {
        let rgb = (0..6).flat_map(|i| [i * 10, 0, 0]).collect();
        Image {
            width: 3,
            height: 2,
            rgb,
        }
    }

    fn reds(i: &Image) -> Vec<u8> {
        i.rgb.chunks(3).map(|p| p[0]).collect()
    }

    #[test]
    fn orientation_1_and_unknown_values_keep_the_image() {
        for o in [1, 2, 4, 5, 7, 0, 99] {
            let i = orient(sample(), o);
            assert_eq!((i.width, i.height), (3, 2), "orientation {o}");
            assert_eq!(reds(&i), [0, 10, 20, 30, 40, 50], "orientation {o}");
        }
    }

    #[test]
    fn orientation_3_turns_the_image_half_a_turn() {
        let i = orient(sample(), 3);
        assert_eq!((i.width, i.height), (3, 2));
        assert_eq!(reds(&i), [50, 40, 30, 20, 10, 0]);
    }

    #[test]
    fn orientation_6_turns_clockwise_and_8_counter_clockwise() {
        // Stored:  0 1 2      Clockwise:  3 0     Counter-clockwise:  2 5
        //          3 4 5                  4 1                         1 4
        //                                 5 2                         0 3
        let cw = orient(sample(), 6);
        assert_eq!((cw.width, cw.height), (2, 3));
        assert_eq!(reds(&cw), [30, 0, 40, 10, 50, 20]);
        let ccw = orient(sample(), 8);
        assert_eq!((ccw.width, ccw.height), (2, 3));
        assert_eq!(reds(&ccw), [20, 50, 10, 40, 0, 30]);
    }
}
