//! Output sharpening for the developed picture.
//!
//! Against Photo Pro's TIFFs of the same sd Quattro files the developed output loses contrast at
//! high spatial frequencies: relative to its own low-frequency level, the response at the Nyquist
//! frequency is 0.54 and 0.47 in the two daylight files measured (1.0 would be Photo Pro's).
//! An unsharp mask with a 0.8 px Gaussian and amount 1.2 brings that to 0.96 and 0.91.
//!
//! In the dark file the same mask more than doubled the noise (Photo Pro denoises there), so the
//! amount fades out for dark areas: zero at an encoded level of 0.15, full at 0.45. With that the
//! dark file's high-frequency response is 1.05 and its flat-area noise 1.56 times Photo Pro's
//! (1.42 times without sharpening). It acts on luminance and adds the same amount to every
//! channel, so hues stay put.

const SIGMA: f64 = 0.8;
const AMOUNT: f32 = 1.2;
/// Encoded levels (0..65535) of the blurred luminance between which the amount fades in.
const FADE: (f32, f32) = (0.15 * 65535.0, 0.45 * 65535.0);
/// Rows the filter looks at: the one being written and two on each side.
const WINDOW: usize = 5;

pub struct Sharpener {
    width: usize,
    height: usize,
    kernel: [f32; WINDOW],
    amount: f32,
    /// Developed rows, stored at `y % WINDOW`.
    rows: Vec<Vec<u16>>,
    /// Luminance of each stored row blurred horizontally.
    blurred: Vec<Vec<f32>>,
    /// First row that has not been developed yet.
    next: usize,
}

impl Sharpener {
    pub fn new(width: usize, height: usize) -> Sharpener {
        let mut kernel = [0f32; WINDOW];
        for (i, k) in kernel.iter_mut().enumerate() {
            let d = i as f64 - 2.0;
            *k = (-d * d / (2.0 * SIGMA * SIGMA)).exp() as f32;
        }
        let sum: f32 = kernel.iter().sum();
        kernel.iter_mut().for_each(|k| *k /= sum);
        Sharpener {
            width,
            height,
            kernel,
            amount: AMOUNT,
            rows: vec![vec![0; width * 3]; WINDOW],
            blurred: vec![vec![0.0; width]; WINDOW],
            next: 0,
        }
    }

    fn luma(px: &[u16]) -> f32 {
        (px[0] as f32 + px[1] as f32 + px[2] as f32) / 3.0
    }

    /// Writes the sharpened row `y` into `out`. Rows must be asked for in order; `develop(y, row)`
    /// fills a row of the unsharpened picture and is called once per row.
    pub fn row(&mut self, y: usize, mut develop: impl FnMut(usize, &mut [u16]), out: &mut [u16]) {
        let last = self.height - 1;
        while self.next <= (y + 2).min(last) {
            let slot = self.next % WINDOW;
            develop(self.next, &mut self.rows[slot]);
            let (row, blur) = (&self.rows[slot], &mut self.blurred[slot]);
            for (x, b) in blur.iter_mut().enumerate() {
                *b = (0..WINDOW)
                    .map(|j| {
                        let xx = (x + j).saturating_sub(2).min(self.width - 1);
                        self.kernel[j] * Self::luma(&row[xx * 3..xx * 3 + 3])
                    })
                    .sum();
            }
            self.next += 1;
        }
        let slots: [usize; WINDOW] =
            std::array::from_fn(|i| (y + i).saturating_sub(2).min(last) % WINDOW);
        let own = &self.rows[y % WINDOW];
        for x in 0..self.width {
            let blur: f32 = (0..WINDOW)
                .map(|i| self.kernel[i] * self.blurred[slots[i]][x])
                .sum();
            let px = &own[x * 3..x * 3 + 3];
            let t = ((blur - FADE.0) / (FADE.1 - FADE.0)).clamp(0.0, 1.0);
            let delta = self.amount * t * t * (3.0 - 2.0 * t) * (Self::luma(px) - blur);
            for c in 0..3 {
                out[x * 3 + c] = (px[c] as f32 + delta).round().clamp(0.0, 65535.0) as u16;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(width: usize, height: usize, src: impl Fn(usize, usize) -> [u16; 3]) -> Vec<Vec<u16>> {
        let mut s = Sharpener::new(width, height);
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

    #[test]
    fn flat_pictures_are_unchanged() {
        let out = run(9, 7, |_, _| [20000, 30000, 40000]);
        assert!(out
            .iter()
            .all(|r| r.chunks(3).all(|p| p == [20000, 30000, 40000])));
    }

    #[test]
    fn an_edge_gets_overshoot_on_both_sides_and_keeps_its_midpoint() {
        let out = run(16, 5, |x, _| if x < 8 { [30000; 3] } else { [50000; 3] });
        let row = &out[2];
        assert!(row[7 * 3] < 30000, "dark side dips below its level");
        assert!(row[8 * 3] > 50000, "bright side rises above its level");
        assert_eq!(row[0], 30000);
        assert_eq!(row[15 * 3], 50000);
        let mid = (row[7 * 3] as i32 + row[8 * 3] as i32) / 2;
        assert!((mid - 40000).abs() < 300, "{mid}");
    }

    #[test]
    fn it_works_in_both_directions() {
        let out = run(5, 16, |_, y| if y < 8 { [30000; 3] } else { [50000; 3] });
        assert!(out[7][6] < 30000 && out[8][6] > 50000);
    }

    #[test]
    fn every_channel_moves_by_the_same_amount() {
        let out = run(16, 5, |x, _| {
            if x < 8 {
                [10000, 20000, 30000]
            } else {
                [30000, 40000, 50000]
            }
        });
        let p = &out[2][8 * 3..8 * 3 + 3];
        let d: Vec<i32> = p
            .iter()
            .zip([30000, 40000, 50000])
            .map(|(&a, b)| a as i32 - b)
            .collect();
        assert!(
            d[0] > 0 && (d[0] - d[1]).abs() <= 1 && (d[1] - d[2]).abs() <= 1,
            "{d:?}"
        );
    }

    #[test]
    fn dark_areas_are_sharpened_less_than_bright_ones() {
        let step = |lo: u16, hi: u16| {
            let out = run(16, 5, |x, _| if x < 8 { [lo; 3] } else { [hi; 3] });
            (hi as i32 - out[2][8 * 3] as i32).abs()
        };
        // The same relative step: overshoot is far smaller below the fade-in level.
        assert!(step(3000, 6000) < step(30000, 60000) / 10);
        assert_eq!(step(1000, 2000), 0, "nothing at all in the deepest shadows");
    }

    #[test]
    fn output_is_clamped_and_tiny_pictures_work() {
        let out = run(16, 5, |x, _| if x < 8 { [0; 3] } else { [65535; 3] });
        assert_eq!(out[2][7 * 3], 0);
        assert_eq!(out[2][8 * 3], 65535);
        let one = run(1, 1, |_, _| [100, 200, 300]);
        assert_eq!(one[0], [100, 200, 300]);
        let thin = run(2, 2, |x, y| [(x * 1000 + y * 100) as u16; 3]);
        assert_eq!(thin.len(), 2);
    }
}
