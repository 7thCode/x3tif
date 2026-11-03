//! Finishing of the developed Quattro picture: colour-noise smoothing and sharpening.
//!
//! Both work on luminance `Y = (R + G + B) / 3` of the encoded 16 bit picture and the colour
//! differences `R - Y`, `G - Y`, `B - Y`, and both are fitted to Photo Pro's TIFFs of the same
//! sd Quattro files.
//!
//! **Colour noise.** The red and green layers have a quarter of the pixels of the blue layer, and
//! their noise shows as pink and green speckle. Photo Pro's colour differences are very smooth,
//! even across colour edges: against them, the RMS error of ours (in /255, mean of three files)
//! falls from 5.6 to 2.1 with a Gaussian of sigma 4 on the colour differences; sigma 7 leaves the
//! tiles about as close (1.16/2.65/1.73 against 1.37/2.42/1.78) and cuts the R-G noise of a flat
//! green background from 4.17 to 3.14 (Photo Pro 2.84), so that is the width used. Smoothing that is guided by luminance edges did worse, also in the
//! tiles with the strongest colour edges (8.9 -> 2.6 for the plain Gaussian, 2.9-4.9 guided), so
//! this one is not edge-aware. It leaves luminance untouched.
//!
//! **Sharpening.** Against Photo Pro the developed output loses contrast at high spatial
//! frequencies: relative to its own low-frequency level the response at the Nyquist frequency is
//! 0.54 and 0.47 in the two daylight files measured (1.0 would be Photo Pro's). An unsharp mask
//! on luminance with a 0.8 px Gaussian and amount 1.2 brings that to 0.96 and 0.91. In the dark
//! file the same mask more than doubled the noise (Photo Pro denoises there), so the amount fades
//! out for dark areas: zero at an encoded level of 0.15, full at 0.45. It adds the same amount to
//! every channel, so hues stay put.
//!
//! **Luminance grain.** The sd Quattro is developed with a fixed +1 EV, which lifts dark scenes by
//! a stop or more and with them their noise: in the dark file the flat-area luminance noise was
//! 2.6x Photo Pro's (2.93 vs 1.13 in /255), with the sharpening's Nyquist response at 1.39. Luminance
//! detail below 2/255 (`CORE`, a soft threshold on the difference to the blurred luminance) is
//! removed where the sharpening fades out: noise 1.99, response 0.92; in the bright file
//! 6.74 -> 6.52 (Photo Pro 5.34), response 0.96 -> 0.91. Larger values cost more texture than they
//! remove grain.
//!
//! **Mid-frequency grain.** What was still coarse is a mottle a few pixels across. The difference
//! between the luminance blurred with sigma 0.8 and with sigma 2.5 (`WIDE_SIGMA`) is cored by 3/255
//! (`MID_CORE`) over the whole picture; real texture is far above that and stays. Flat-tile noise
//! of the three references 6.52/6.12/1.99 -> about 5.4/4.9/1.0 (Photo Pro 5.34/6.15/1.13), sharp
//! response unchanged or up (0.91/0.86/0.92 -> 0.94/0.90/0.90), bokeh luminance 3.19 -> 2.9
//! (Photo Pro 2.26).

const CHROMA_SIGMA: f64 = 7.0;
const CHROMA_RADIUS: usize = 21; // 3 sigma
/// Blur that separates the mid-frequency band (a few pixels) from the low frequencies.
const WIDE_SIGMA: f64 = 2.5;
const WIDE_RADIUS: usize = 8;
/// Luminance of that band below this (levels of 65535, = 3/255) is removed everywhere.
const MID_CORE: f32 = 3.0 * 257.0;
const SHARP_SIGMA: f64 = 0.8;
const SHARP_RADIUS: usize = 2;
const AMOUNT: f32 = 1.2;
/// Encoded levels (0..65535) of the blurred luminance between which the amount fades in.
const FADE: (f32, f32) = (0.15 * 65535.0, 0.45 * 65535.0);
/// Luminance detail smaller than this (levels of 65535, = 2/255) is removed in dark areas, with the
/// same fade as the sharpening turned around: 0 where the sharpening is full.
const CORE: f32 = 2.0 * 257.0;
/// Rows kept: the one being written and `CHROMA_RADIUS` on each side.
const WINDOW: usize = 2 * CHROMA_RADIUS + 1;

fn gaussian(sigma: f64, radius: usize) -> Vec<f32> {
    let mut k: Vec<f32> = (0..=2 * radius)
        .map(|i| {
            let d = i as f64 - radius as f64;
            (-d * d / (2.0 * sigma * sigma)).exp() as f32
        })
        .collect();
    let sum: f32 = k.iter().sum();
    k.iter_mut().for_each(|v| *v /= sum);
    k
}

/// `dst[x] = sum_j k[j] * src[x + j - radius]`, with the row's edge samples repeated outward.
fn blur_row(src: &[f32], k: &[f32], dst: &mut [f32], padded: &mut Vec<f32>) {
    let radius = k.len() / 2;
    padded.clear();
    padded.extend(std::iter::repeat_n(src[0], radius));
    padded.extend_from_slice(src);
    padded.extend(std::iter::repeat_n(src[src.len() - 1], radius));
    dst.fill(0.0);
    for (j, &kj) in k.iter().enumerate() {
        for (d, s) in dst.iter_mut().zip(&padded[j..]) {
            *d += kj * s;
        }
    }
}

struct Slot {
    /// The developed row.
    px: Vec<u16>,
    /// Luminance blurred horizontally with the sharpening kernel.
    luma: Vec<f32>,
    luma2: Vec<f32>,
    /// Colour differences blurred horizontally with the colour-noise kernel.
    chroma: [Vec<f32>; 3],
}

pub struct Finisher {
    width: usize,
    height: usize,
    wide: Vec<f32>,
    sharp: Vec<f32>,
    smooth: Vec<f32>,
    slots: Vec<Slot>,
    /// First row that has not been developed yet.
    next: usize,
    scratch: Vec<f32>,
    padded: Vec<f32>,
    acc: [Vec<f32>; 3],
}

impl Finisher {
    pub fn new(width: usize, height: usize) -> Finisher {
        let slot = || Slot {
            px: vec![0; width * 3],
            luma: vec![0.0; width],
            luma2: vec![0.0; width],
            chroma: std::array::from_fn(|_| vec![0.0; width]),
        };
        Finisher {
            width,
            height,
            wide: gaussian(WIDE_SIGMA, WIDE_RADIUS),
            sharp: gaussian(SHARP_SIGMA, SHARP_RADIUS),
            smooth: gaussian(CHROMA_SIGMA, CHROMA_RADIUS),
            slots: (0..WINDOW).map(|_| slot()).collect(),
            next: 0,
            scratch: vec![0.0; width],
            padded: Vec::new(),
            acc: std::array::from_fn(|_| vec![0.0; width]),
        }
    }

    fn develop_next(&mut self, develop: &mut impl FnMut(usize, &mut [u16])) {
        let slot = &mut self.slots[self.next % WINDOW];
        develop(self.next, &mut slot.px);
        let w = self.width;
        let mut y = vec![0f32; w];
        let mut diff: [Vec<f32>; 3] = std::array::from_fn(|_| vec![0f32; w]);
        for x in 0..w {
            let p = &slot.px[x * 3..x * 3 + 3];
            y[x] = (p[0] as f32 + p[1] as f32 + p[2] as f32) / 3.0;
            for (d, &v) in diff.iter_mut().zip(p) {
                d[x] = v as f32 - y[x];
            }
        }
        blur_row(&y, &self.sharp, &mut slot.luma, &mut self.padded);
        blur_row(&y, &self.wide, &mut slot.luma2, &mut self.padded);
        for (d, out) in diff.iter().zip(slot.chroma.iter_mut()) {
            blur_row(d, &self.smooth, out, &mut self.padded);
        }
        self.next += 1;
    }

    /// Writes the finished row `y` into `out`. Rows must be asked for in order; `develop(y, row)`
    /// fills a row of the unfinished picture and is called once per row.
    pub fn row(&mut self, y: usize, mut develop: impl FnMut(usize, &mut [u16]), out: &mut [u16]) {
        let last = self.height - 1;
        while self.next <= (y + CHROMA_RADIUS).min(last) {
            self.develop_next(&mut develop);
        }
        let at = |d: isize| ((y as isize + d).clamp(0, last as isize) as usize) % WINDOW;

        // Colour differences: vertical pass of the Gaussian.
        for c in 0..3 {
            self.acc[c].fill(0.0);
            for (i, &k) in self.smooth.iter().enumerate() {
                let row = &self.slots[at(i as isize - CHROMA_RADIUS as isize)].chroma[c];
                for (a, v) in self.acc[c].iter_mut().zip(row) {
                    *a += k * v;
                }
            }
        }
        // Luminance blurred in both directions, for the unsharp mask.
        self.scratch.fill(0.0);
        for (i, &k) in self.sharp.iter().enumerate() {
            let row = &self.slots[at(i as isize - SHARP_RADIUS as isize)].luma;
            for (a, v) in self.scratch.iter_mut().zip(row) {
                *a += k * v;
            }
        }

        let mut wide = vec![0f32; self.width];
        for (i, &k) in self.wide.iter().enumerate() {
            let row = &self.slots[at(i as isize - WIDE_RADIUS as isize)].luma2;
            for (a, v) in wide.iter_mut().zip(row) {
                *a += k * v;
            }
        }
        let own = &self.slots[at(0)].px;
        for x in 0..self.width {
            let p = &own[x * 3..x * 3 + 3];
            let luma = (p[0] as f32 + p[1] as f32 + p[2] as f32) / 3.0;
            let blur = self.scratch[x];
            let t = ((blur - FADE.0) / (FADE.1 - FADE.0)).clamp(0.0, 1.0);
            let d = luma - blur;
            let d = d.signum() * (d.abs() - CORE * (1.0 - t * t * (3.0 - 2.0 * t))).max(0.0);
            let m = blur - wide[x];
            let m = m.signum() * (m.abs() - MID_CORE).max(0.0);
            let y_out = wide[x] + m + d * (1.0 + AMOUNT * t * t * (3.0 - 2.0 * t));
            for c in 0..3 {
                out[x * 3 + c] = (y_out + self.acc[c][x]).round().clamp(0.0, 65535.0) as u16;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(width: usize, height: usize, src: impl Fn(usize, usize) -> [u16; 3]) -> Vec<Vec<u16>> {
        let mut s = Finisher::new(width, height);
        let mut developed = Vec::new();
        let out = (0..height)
            .map(|y| {
                let mut row = vec![0; width * 3];
                s.row(
                    y,
                    |yy, buf| {
                        developed.push(yy);
                        for x in 0..width {
                            buf[x * 3..x * 3 + 3].copy_from_slice(&src(x, yy));
                        }
                    },
                    &mut row,
                );
                row
            })
            .collect();
        assert_eq!(
            developed,
            (0..height).collect::<Vec<_>>(),
            "each row once, in order"
        );
        out
    }

    fn luma(p: &[u16]) -> i32 {
        (p[0] as i32 + p[1] as i32 + p[2] as i32) / 3
    }

    #[test]
    fn flat_pictures_are_unchanged() {
        let out = run(9, 7, |_, _| [20000, 30000, 40000]);
        assert!(out.iter().all(|r| r.chunks(3).all(|p| p
            .iter()
            .zip([20000, 30000, 40000])
            .all(|(&a, b)| a.abs_diff(b) <= 1))));
    }

    #[test]
    fn a_luminance_edge_gets_overshoot_on_both_sides_and_keeps_its_midpoint() {
        let out = run(40, 5, |x, _| if x < 20 { [30000; 3] } else { [50000; 3] });
        let row = &out[2];
        assert!(row[19 * 3] < 30000, "dark side dips below its level");
        assert!(row[20 * 3] > 50000, "bright side rises above its level");
        assert_eq!(row[0], 30000);
        assert_eq!(row[39 * 3], 50000);
        let mid = (row[19 * 3] as i32 + row[20 * 3] as i32) / 2;
        assert!((mid - 40000).abs() < 300, "{mid}");
    }

    #[test]
    fn it_works_in_both_directions() {
        let out = run(5, 40, |_, y| if y < 20 { [30000; 3] } else { [50000; 3] });
        assert!(out[19][6] < 30000 && out[20][6] > 50000);
    }

    #[test]
    fn dark_areas_are_sharpened_less_than_bright_ones() {
        let over = |lo: u16, hi: u16| {
            let out = run(40, 5, |x, _| if x < 20 { [lo; 3] } else { [hi; 3] });
            out[2][20 * 3] as i32 - hi as i32
        };
        assert!(over(30000, 60000) > 1000, "bright edges overshoot");
        assert!(over(3000, 6000) <= 0, "dark edges do not");
        // Nothing is sharpened in the deepest shadows: no overshoot (the grain removal may soften
        // the edge by about one level of 255).
        let out = run(40, 5, |x, _| if x < 20 { [1000; 3] } else { [2000; 3] });
        assert!(out[2][20 * 3] <= 2000 && out[2][19 * 3] >= 1000);
    }

    #[test]
    fn colour_speckle_is_smoothed_and_luminance_is_not_touched() {
        // Alternating pink and green at constant luminance 30000: pure colour noise.
        let speckle = |x: usize, y: usize| {
            if (x + y).is_multiple_of(2) {
                [33000, 27000, 30000]
            } else {
                [27000, 33000, 30000]
            }
        };
        let out = run(48, 48, speckle);
        let centre = &out[24][24 * 3..24 * 3 + 3];
        assert!(centre[0].abs_diff(centre[1]) < 600, "{centre:?}"); // was 6000 apart
        assert!(luma(centre).abs_diff(30000) <= 1, "{centre:?}");
    }

    #[test]
    fn a_colour_edge_is_blurred_but_its_luminance_stays_flat() {
        let out = run(80, 5, |x, _| {
            if x < 40 {
                [36000, 30000, 24000]
            } else {
                [24000, 30000, 36000]
            }
        });
        let row = &out[2];
        assert!(row[10 * 3] > row[40 * 3], "far left stays reddish");
        assert!(row[40 * 3].abs_diff(row[39 * 3]) < 4000, "edge is soft");
        for x in [5, 30, 39, 40, 41, 60, 75] {
            assert!(luma(&row[x * 3..x * 3 + 3]).abs_diff(30000) <= 1, "x={x}");
        }
    }

    #[test]
    fn output_is_clamped_and_tiny_pictures_work() {
        let out = run(40, 5, |x, _| if x < 20 { [0; 3] } else { [65535; 3] });
        assert_eq!(out[2][19 * 3], 0);
        assert_eq!(out[2][20 * 3], 65535);
        let one = run(1, 1, |_, _| [100, 200, 300]);
        assert_eq!(one[0], [100, 200, 300]);
        let thin = run(2, 2, |x, y| [(x * 1000 + y * 100) as u16; 3]);
        assert_eq!(thin.len(), 2);
    }

    #[test]
    fn small_luminance_grain_is_removed_in_the_dark_but_not_in_the_light() {
        let grain = |level: u16| {
            let out = run(48, 48, |x, y| {
                let v = if (x + y).is_multiple_of(2) {
                    level + 300
                } else {
                    level - 300
                };
                [v; 3]
            });
            let p = &out[24][24 * 3..24 * 3 + 3];
            (p[0] as i32 - level as i32).abs()
        };
        assert!(grain(8000) < 100, "dark grain is gone");
        assert!(grain(50000) > 300, "bright detail is kept (and sharpened)");
    }
}
