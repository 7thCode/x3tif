//! Finishing of the developed picture (Quattro and DP Merrill): colour-noise smoothing and
//! sharpening. The measurements below are of the sd Quattro; each camera's own values, and the
//! measurements they come from, are on its `Params`.
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
//! green background from 4.17 to 3.14 (Photo Pro 2.84), so that was the width used at first; the R-G of the bokeh was still 3.14 against 2.84 and
//! the user saw slight coarseness, so it is 8.5 now (2.87; tile errors 1.23/2.80/1.83). Smoothing that is guided by luminance edges did worse, also in the
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
//! detail below 2/255 at first, 3/255 now (`CORE`, a soft threshold on the difference to the blurred luminance) is
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
//!
//! **Second round, after "still coarse everywhere".** Raising `MID_CORE` from 3 to 10/255 only took
//! the bokeh from 2.88 to 2.79 and moved the result away from Photo Pro (RMS vs Photo Pro of the
//! bright file 8.25 -> 8.92), so it went to 5. The grain left was pixel-sized: raising `CORE` from 2
//! to 3/255 took the bokeh of 2.81 to 2.34 (Photo Pro 2.26) at the cost of the dark file's sharp
//! response, 0.90 -> 0.77; 4/255 would be 1.98 and 0.70.

/// 仕上げの強さ。Photo Pro の出力に合わせて機種ごとに決める。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    /// 色差にかけるガウスぼかしの幅(画素)。0 以下なら色差には触れない。
    pub chroma_sigma: f64,
    /// Blur that separates the mid-frequency band (a few pixels) from the low frequencies.
    pub wide_sigma: f64,
    /// Luminance of that band below this (in /255) is removed everywhere.
    pub mid_core: f32,
    pub sharp_sigma: f64,
    pub amount: f32,
    /// Encoded levels (0..1) of the blurred luminance between which the amount fades in.
    pub fade: (f32, f32),
    /// Luminance detail smaller than this (in /255) is removed in dark and medium-dark areas,
    /// with the same fade as the sharpening turned around: 0 where the sharpening is full.
    pub core: f32,
}

impl Params {
    /// sd Quattro(CAMERAID 40)と、測っていないほかの Quattro。Photo Pro 6.9.0 の既定設定の TIFF 5 枚
    /// (ISO 100)との RMSE(/255)で決めた。このファイル冒頭の測定で決めた以前の値(色差 σ8.5、
    /// 中域の除去 6/255、鮮鋭化 σ0.8・量 1.2、細部の除去 3.5/255)は 12.02/6.17/5.90/5.36/9.23。
    /// 色差のぼかしを σ2 に弱め、中域の除去をやめ、鮮鋭化を σ1.2・量 1.2(0.1〜0.4 で立ち上げ)、
    /// 細部の除去を 2/255 にすると 9.25/5.05/5.20/4.94/6.00 で 5 枚とも下がる。dp0 Quattro の値
    /// そのままだと 9.10/5.45/5.42/5.56/6.05 で平坦部の一致が落ち、色差 σ4・中域 3/255 では
    /// 10.96/5.50/5.49/4.87/8.00。以前の値は粒状感を Photo Pro より強く消していた。
    pub const SD_QUATTRO: Params = Params {
        chroma_sigma: 2.0,
        wide_sigma: 3.5,
        mid_core: 0.0,
        sharp_sigma: 1.2,
        amount: 1.2,
        fade: (0.1, 0.4),
        core: 2.0,
    };

    /// DP1 Merrill(CAMERAID 77。DP2 / DP3 Merrill=78 は参照が無いまま同じ値を使う)。Photo Pro 6.9.0 の
    /// 既定設定(色・輝度のノイズ低減とも 2/4)の TIFF 5 枚(ISO 100)と、Adobe RGB で直接比べた
    /// RMSE(/255、5 枚の平均、高周波は 16 画素ブロックの平均を引いた残り)で決めた。仕上げなしは
    /// 8.39/7.40/8.03/12.05/8.88(平均 8.95、低周波 2.87、高周波 8.46)で、差の大半は高周波、
    /// なかでも色ノイズだった。色差のぼかしだけで σ1/2/3/5 → 平均 5.91/5.70/5.72/5.83 と大きく
    /// 下がる(σ2〜3 が底)。輝度の細部の除去(`core` 1/2/4 → 5.73/5.77/5.85)と中域の除去
    /// (`mid_core` 2/4 → 6.28/6.87)は、除去なしの 5.70 より悪い。Photo Pro の輝度ノイズ低減は
    /// これより弱い。逆に鮮鋭化は効く: 量 0.5/1.0/1.5/2.0 → 5.11/5.02/5.45/6.28(σ1)、負の量は
    /// 6.67。σ0.8〜1.5 で量 0.8〜1.0 が底で、近傍(σ0.9〜1.2、量 0.8〜1.2、色差 σ2〜3、立ち上がり
    /// 0.03〜0.1 から 0.2〜0.5)は 4.99〜5.13 の平らな領域。色差 σ2.5、鮮鋭化 σ1.0・量 0.8(暗部
    /// 0.05〜0.3 で立ち上げ)にすると 4.12/4.59/4.81/6.85/4.58(平均 4.99、低周波 2.84、高周波
    /// 4.08)で 5 枚とも下がる。高感度のファイルは測っていない。
    pub const DP_MERRILL: Params = Params {
        chroma_sigma: 2.5,
        wide_sigma: 3.5,
        mid_core: 0.0,
        sharp_sigma: 1.0,
        amount: 0.8,
        fade: (0.05, 0.3),
        core: 0.0,
    };

    /// 粒状感を強く消す値。sd Quattro の以前の既定値(Photo Pro 6.9.0 の TIFF との RMSE は
    /// 12.02/6.17/5.90/5.36/9.23 で、`SD_QUATTRO` より 5 枚とも大きい)で、利用者が粒状感の
    /// 抑制(`Tuning::grain`)を上げたときに `blend` の行き先になる。
    pub const SMOOTH: Params = Params {
        chroma_sigma: 8.5,
        wide_sigma: 3.5,
        mid_core: 6.0,
        sharp_sigma: 0.8,
        amount: 1.2,
        fade: (0.15, 0.45),
        core: 3.5,
    };

    /// `self` から `to` へ、`t`(0 で `self`、1 で `to`。範囲の外は丸める)だけ寄せた値。
    pub fn blend(self, to: Params, t: f64) -> Params {
        let t = if t.is_finite() {
            t.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let f = t as f32;
        let mix = |a: f64, b: f64| a + (b - a) * t;
        let mixf = |a: f32, b: f32| a + (b - a) * f;
        Params {
            chroma_sigma: mix(self.chroma_sigma, to.chroma_sigma),
            wide_sigma: mix(self.wide_sigma, to.wide_sigma),
            mid_core: mixf(self.mid_core, to.mid_core),
            sharp_sigma: mix(self.sharp_sigma, to.sharp_sigma),
            amount: mixf(self.amount, to.amount),
            fade: (mixf(self.fade.0, to.fade.0), mixf(self.fade.1, to.fade.1)),
            core: mixf(self.core, to.core),
        }
    }

    /// dp0 Quattro(CAMERAID 83。dp1〜dp3 Quattro は参照が無いまま同じ値を使う)。Photo Pro 6.9.0 の既定設定の TIFF 5 枚(ISO 100)との RMSE(/255)で
    /// 決めた。sd Quattro の値のままだと 11.9/10.7/5.1/11.1/12.6 で、色差の高域は Photo Pro の
    /// 1/7 しか残らず(R−G、σ3 より細かい成分が 0.24〜0.39 対 2.0〜2.9)、輝度の振幅も全帯域で
    /// 0.70〜0.83 倍だった。ISO 100 の dp0 に粒状感の除去は要らない。除去をやめ、色差のぼかしを
    /// σ2 に、鮮鋭化を σ1.5・量 1.0(暗部 0.05〜0.3 で立ち上げ)にすると 8.5/6.7/4.1/7.8/9.0。
    /// 近い候補(σ0.8〜2.0、量 0.8〜1.8、色差 σ1.2〜4)は ±0.3 以内で、量 1.3 では悪化する。
    /// 高感度のファイルは測っていない。
    pub const DP0_QUATTRO: Params = Params {
        chroma_sigma: 2.0,
        wide_sigma: 3.5,
        mid_core: 0.0,
        sharp_sigma: 1.5,
        amount: 1.0,
        fade: (0.05, 0.3),
        core: 0.0,
    };
}

/// ガウス核の半径(3 シグマ)。
fn radius_for(sigma: f64) -> usize {
    (3.0 * sigma.max(0.0)).round() as usize
}

fn gaussian(sigma: f64, radius: usize) -> Vec<f32> {
    if radius == 0 {
        // 幅 0 はぼかさない(恒等)。
        return vec![1.0];
    }
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
    params: Params,
    /// 色差のぼかしの半径。前後この行数だけ先読みする。
    chroma_radius: usize,
    wide_radius: usize,
    sharp_radius: usize,
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
    pub fn new(width: usize, height: usize, params: Params) -> Finisher {
        let chroma_radius = radius_for(params.chroma_sigma);
        let wide_radius = radius_for(params.wide_sigma);
        let sharp_radius = radius_for(params.sharp_sigma);
        // 保持する行数: 書き出す行と、その前後に最も広い核の半径ぶん。
        let window = 2 * chroma_radius.max(wide_radius).max(sharp_radius) + 1;
        let slot = || Slot {
            px: vec![0; width * 3],
            luma: vec![0.0; width],
            luma2: vec![0.0; width],
            chroma: std::array::from_fn(|_| vec![0.0; width]),
        };
        Finisher {
            width,
            height,
            params,
            chroma_radius,
            wide_radius,
            sharp_radius,
            wide: gaussian(params.wide_sigma, wide_radius),
            sharp: gaussian(params.sharp_sigma, sharp_radius),
            smooth: gaussian(params.chroma_sigma, chroma_radius),
            slots: (0..window).map(|_| slot()).collect(),
            next: 0,
            scratch: vec![0.0; width],
            padded: Vec::new(),
            acc: std::array::from_fn(|_| vec![0.0; width]),
        }
    }

    fn develop_next(&mut self, develop: &mut impl FnMut(usize, &mut [u16])) {
        let window = self.slots.len();
        let slot = &mut self.slots[self.next % window];
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
        let window = self.slots.len();
        let ahead = window / 2;
        while self.next <= (y + ahead).min(last) {
            self.develop_next(&mut develop);
        }
        let at = |d: isize| ((y as isize + d).clamp(0, last as isize) as usize) % window;
        let (chroma_radius, wide_radius, sharp_radius) =
            (self.chroma_radius, self.wide_radius, self.sharp_radius);
        let p = self.params;
        let (mid_core, core) = (p.mid_core * 257.0, p.core * 257.0);
        let fade = (p.fade.0 * 65535.0, p.fade.1 * 65535.0);

        // Colour differences: vertical pass of the Gaussian.
        for c in 0..3 {
            self.acc[c].fill(0.0);
            for (i, &k) in self.smooth.iter().enumerate() {
                let row = &self.slots[at(i as isize - chroma_radius as isize)].chroma[c];
                for (a, v) in self.acc[c].iter_mut().zip(row) {
                    *a += k * v;
                }
            }
        }
        // Luminance blurred in both directions, for the unsharp mask.
        self.scratch.fill(0.0);
        for (i, &k) in self.sharp.iter().enumerate() {
            let row = &self.slots[at(i as isize - sharp_radius as isize)].luma;
            for (a, v) in self.scratch.iter_mut().zip(row) {
                *a += k * v;
            }
        }

        let mut wide = vec![0f32; self.width];
        for (i, &k) in self.wide.iter().enumerate() {
            let row = &self.slots[at(i as isize - wide_radius as isize)].luma2;
            for (a, v) in wide.iter_mut().zip(row) {
                *a += k * v;
            }
        }
        let own = &self.slots[at(0)].px;
        for x in 0..self.width {
            let px = &own[x * 3..x * 3 + 3];
            let luma = (px[0] as f32 + px[1] as f32 + px[2] as f32) / 3.0;
            let blur = self.scratch[x];
            let t = ((blur - fade.0) / (fade.1 - fade.0)).clamp(0.0, 1.0);
            let d = luma - blur;
            let d = d.signum() * (d.abs() - core * (1.0 - t * t * (3.0 - 2.0 * t))).max(0.0);
            let m = blur - wide[x];
            let m = m.signum() * (m.abs() - mid_core).max(0.0);
            let y_out = wide[x] + m + d * (1.0 + p.amount * t * t * (3.0 - 2.0 * t));
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
        let mut s = Finisher::new(width, height, Params::SMOOTH);
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

    fn run_with(
        params: Params,
        width: usize,
        height: usize,
        src: impl Fn(usize, usize) -> [u16; 3],
    ) -> Vec<Vec<u16>> {
        let mut s = Finisher::new(width, height, params);
        (0..height)
            .map(|y| {
                let mut row = vec![0; width * 3];
                s.row(
                    y,
                    |yy, buf| {
                        for x in 0..width {
                            buf[x * 3..x * 3 + 3].copy_from_slice(&src(x, yy));
                        }
                    },
                    &mut row,
                );
                row
            })
            .collect()
    }

    #[test]
    fn blend_moves_between_the_two_parameter_sets_and_clamps() {
        let (a, b) = (Params::SD_QUATTRO, Params::SMOOTH);
        assert_eq!(a.blend(b, 0.0), a);
        assert_eq!(a.blend(b, 1.0), b);
        assert_eq!(a.blend(b, -3.0), a);
        assert_eq!(a.blend(b, 9.0), b);
        assert_eq!(a.blend(b, f64::NAN), a);
        let h = a.blend(b, 0.5);
        assert!((h.chroma_sigma - (a.chroma_sigma + b.chroma_sigma) / 2.0).abs() < 1e-12);
        assert!((h.mid_core - (a.mid_core + b.mid_core) / 2.0).abs() < 1e-6);
        assert!((h.fade.0 - (a.fade.0 + b.fade.0) / 2.0).abs() < 1e-6);
    }

    #[test]
    fn more_grain_reduction_removes_more_of_a_fine_grain() {
        // 暗部の細かい明暗(±3.8/255): 抑制を上げるほど平らになる(測った残り 503 → 293 → 93)。
        let grain = |t: f64| {
            let out = run_with(
                Params::SD_QUATTRO.blend(Params::SMOOTH, t),
                48,
                48,
                |x, y| {
                    [if (x + y).is_multiple_of(2) {
                        9000
                    } else {
                        7000
                    }; 3]
                },
            );
            (out[24][24 * 3] as i32 - 8000).abs()
        };
        assert!(grain(0.0) > grain(0.5) && grain(0.5) > grain(1.0));
        assert!(grain(1.0) < 150);
    }

    #[test]
    fn kernel_radii_follow_the_widths() {
        // 以前の値は、定数だったころの半径(26, 11, 2)と同じになる。
        let p = Params::SMOOTH;
        assert_eq!(
            (
                radius_for(p.chroma_sigma),
                radius_for(p.wide_sigma),
                radius_for(p.sharp_sigma)
            ),
            (26, 11, 2)
        );
        assert_eq!(radius_for(0.0), 0);
        assert_eq!(gaussian(0.0, 0), vec![1.0]);
        let k = gaussian(1.5, radius_for(1.5));
        assert!((k.iter().sum::<f32>() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn zero_strength_params_return_the_picture_unchanged() {
        let off = Params {
            chroma_sigma: 0.0,
            mid_core: 0.0,
            amount: 0.0,
            core: 0.0,
            ..Params::SMOOTH
        };
        let src = |x: usize, y: usize| [(x * 997 + y * 13) as u16, (y * 811) as u16, 40000];
        let out = run_with(off, 12, 9, src);
        for (y, row) in out.iter().enumerate() {
            for (x, p) in row.chunks(3).enumerate() {
                let want = src(x, y);
                assert!(
                    p.iter().zip(want).all(|(&a, b)| a.abs_diff(b) <= 1),
                    "({x},{y}) {p:?} vs {want:?}"
                );
            }
        }
    }

    #[test]
    fn dp0_quattro_params_keep_fine_colour_and_dark_detail() {
        // 色のまだら: 強い値ではほぼ消え、dp0 Quattro の値では残る。
        let speckle = |x: usize, y: usize| {
            if (x / 4 + y / 4).is_multiple_of(2) {
                [33000, 27000, 30000]
            } else {
                [27000, 33000, 30000]
            }
        };
        let spread = |p: Params| {
            let out = run_with(p, 64, 64, speckle);
            let c = &out[30][30 * 3..30 * 3 + 3];
            c[0].abs_diff(c[1])
        };
        assert!(spread(Params::DP0_QUATTRO) > 4 * spread(Params::SMOOTH));
        // 暗部の細かい明暗も消さない。
        let grain = |p: Params| {
            let out = run_with(p, 48, 48, |x, y| {
                [if (x + y).is_multiple_of(2) {
                    8300
                } else {
                    7700
                }; 3]
            });
            (out[24][24 * 3] as i32 - 8000).abs()
        };
        assert!(grain(Params::SMOOTH) < 100);
        assert!(grain(Params::DP0_QUATTRO) >= 300);
        // sd Quattro の値はその間: 色のまだらは残し、暗部のごく細かい粒は少し抑える。
        assert!(spread(Params::SD_QUATTRO) > 4 * spread(Params::SMOOTH));
        assert!(grain(Params::SD_QUATTRO) < grain(Params::DP0_QUATTRO));
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
