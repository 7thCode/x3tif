//! The two output looks.
//!
//! * `Raw`: sensor values untouched, only shifted up to fill the 16 bit range (lossless).
//! * `Developed`: black level, white balance, colour matrix, exposure and a tone curve, using the
//!   calibration stored in the file's CAMF block. The exposure defaults to whatever makes the
//!   picture as bright as the camera's own embedded JPEG preview; cameras measured against
//!   Photo Pro (sd Quattro, dp0 Quattro, DP1 Merrill) use its fixed gain and tone curve instead.

use crate::camf::Camf;
use crate::x3f::{Error, Raw};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Mode {
    #[default]
    Raw,
    Developed(Exposure, Tuning),
}

/// White balance names a file can carry (the ones `wb_from_code` produces).
pub const WHITE_BALANCES: [&str; 10] = [
    "Auto",
    "Sunlight",
    "Shade",
    "Overcast",
    "Incandescent",
    "Fluorescent",
    "Flash",
    "Custom",
    "ColorTemp",
    "AutoLSP",
];

/// Allowed range of the red and blue gain multipliers.
pub const GAIN_RANGE: (f64, f64) = (0.5, 2.0);

/// Adjustments on top of the calibrated defaults. `Tuning::default()` changes nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tuning {
    /// 出力の色空間。
    pub color_space: ColorSpace,
    /// Use this white balance instead of the one the file was shot with.
    pub white_balance: Option<&'static str>,
    /// Multipliers on the red and blue gains (1.0 = as calibrated).
    pub red: f64,
    pub blue: f64,
    /// How far the S-shaped tone curve is mixed in, 0 (plain sRGB) to 1.
    pub tone: f64,
    /// Quattro の粒状感・色ノイズの抑制の強さ。0 は Photo Pro に合わせた機種ごとの値、
    /// 1 は強く消す値(`finish::Params::SMOOTH`)。保存する TIFF にだけ効き、プレビューは変わらない。
    pub grain: f64,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            color_space: ColorSpace::default(),
            white_balance: None,
            red: 1.0,
            blue: 1.0,
            tone: TONE_STRENGTH,
            grain: 0.0,
        }
    }
}

impl Tuning {
    /// The same values with everything out of range or not finite replaced by its default,
    /// so a hand-edited or damaged settings file can never produce nonsense.
    pub fn sanitized(self) -> Tuning {
        let d = Tuning::default();
        let pick = |v: f64, lo: f64, hi: f64, default: f64| {
            if v.is_finite() && (lo..=hi).contains(&v) {
                v
            } else {
                default
            }
        };
        Tuning {
            color_space: self.color_space,
            white_balance: self.white_balance,
            red: pick(self.red, GAIN_RANGE.0, GAIN_RANGE.1, d.red),
            blue: pick(self.blue, GAIN_RANGE.0, GAIN_RANGE.1, d.blue),
            tone: pick(self.tone, 0.0, 1.0, d.tone),
            grain: pick(self.grain, 0.0, 1.0, d.grain),
        }
    }
}

/// 「見たまま」出力の色空間。Photo Pro は撮影時の設定に従うが、Quattro の X3F にはその設定を
/// 確実に読める場所がないので、こちらは利用者が選ぶ。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorSpace {
    #[default]
    Srgb,
    /// Adobe RGB (1998) 互換(原色とガンマ 563/256)。
    AdobeRgb,
}

/// リニア sRGB → リニア Adobe RGB (1998)(どちらも D65)。
const SRGB_TO_ADOBE: [[f64; 3]; 3] = [
    [0.715_127_6, 0.284_872_4, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.041_161_9, 0.958_838_1],
];
/// その逆のうち、赤の行(リニア Adobe RGB → リニア sRGB の赤)。
const ADOBE_TO_SRGB_RED: [f64; 3] = [1.398_350_8, -0.398_350_8, 0.0];
const ADOBE_GAMMA: f64 = 563.0 / 256.0;

impl ColorSpace {
    /// 設定ファイルと XMP に書く名前。
    pub fn name(self) -> &'static str {
        match self {
            ColorSpace::Srgb => "sRGB",
            ColorSpace::AdobeRgb => "AdobeRGB",
        }
    }

    /// `name` の逆。知らない名前は `None`。
    pub fn from_name(name: &str) -> Option<ColorSpace> {
        [ColorSpace::Srgb, ColorSpace::AdobeRgb]
            .into_iter()
            .find(|c| c.name() == name)
    }

    /// リニア値(0..=1)をこの色空間のエンコード値にする。
    fn encode(self, l: f64) -> f64 {
        match self {
            ColorSpace::Srgb => srgb_encode(l),
            ColorSpace::AdobeRgb => l.max(0.0).powf(1.0 / ADOBE_GAMMA),
        }
    }

    /// 16bit のエンコード値すべてに対するリニア値。
    fn decoding(self) -> Vec<f32> {
        match self {
            ColorSpace::Srgb => srgb_decoding(),
            ColorSpace::AdobeRgb => (0..=u16::MAX)
                .map(|v| (v as f64 / 65535.0).powf(ADOBE_GAMMA) as f32)
                .collect(),
        }
    }
}

fn srgb_decode(e: f64) -> f64 {
    if e <= 0.04045 {
        e / 12.92
    } else {
        ((e + 0.055) / 1.055).powf(2.4)
    }
}

/// The known white balance name equal to `name`, if any.
pub fn white_balance_name(name: &str) -> Option<&'static str> {
    WHITE_BALANCES.iter().copied().find(|w| *w == name)
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

/// Quattro low-resolution layers contain a regular grid of "highlight" pixels (CAMF
/// `HighlightPixelsInfo`: first column, first row, column pitch, row pitch) that read about half
/// of the true value. Left alone they show up as dotted vertical lines, so, as x3f_tools does,
/// replace each by the mean of its horizontal and vertical neighbours (the grid is sparse enough
/// that those are always ordinary pixels).
pub fn repair_highlight_pixels(raw: &mut Raw, camf: &Camf) {
    let Raw::Quattro {
        low_width,
        low_height,
        low,
        ..
    } = raw
    else {
        return;
    };
    let Some(info) = camf.matrix("HighlightPixelsInfo").filter(|m| m.len() == 4) else {
        return;
    };
    let [col0, row0, col_pitch, row_pitch] =
        [info[0], info[1], info[2], info[3]].map(|v| v as usize);
    if col_pitch < 2 || row_pitch < 2 {
        return;
    }
    let (w, h) = (*low_width, *low_height);
    for y in (row0..h).step_by(row_pitch) {
        for x in (col0..w).step_by(col_pitch) {
            let mut neighbours = Vec::with_capacity(4);
            if x > 0 {
                neighbours.push((x - 1, y));
            }
            if x + 1 < w {
                neighbours.push((x + 1, y));
            }
            if y > 0 {
                neighbours.push((x, y - 1));
            }
            if y + 1 < h {
                neighbours.push((x, y + 1));
            }
            // 1x1 の層には隣が無い(壊れたファイル)。ゼロ除算しない。
            if neighbours.is_empty() {
                continue;
            }
            for c in 0..3 {
                let sum: u32 = neighbours
                    .iter()
                    .map(|&(nx, ny)| low[(ny * w + nx) * 3 + c] as u32)
                    .sum();
                let n = neighbours.len() as u32;
                low[(y * w + x) * 3 + c] = ((sum + n / 2) / n) as u16;
            }
        }
    }
}

/// Autofocus pixels of the sd Quattro (id 40) and sd Quattro H (id 41): regular grids that read low
/// in the luma (top) and chroma (low) layers. Positions are not in the file; these are the ones
/// x3f_tools uses (first column, last column, column pitch, width, first row, last row, row pitch,
/// height). The sd Quattro grids were checked against a sample (pixels read 8-15% low); the
/// sd Quattro H ones are taken from x3f_tools as they are.
struct Grid(usize, usize, usize, usize, usize, usize, usize, usize);

impl Grid {
    fn points(&self) -> Vec<(usize, usize)> {
        let Grid(ci, cf, cp, cs, ri, rf, rp, rs) = *self;
        let mut out = Vec::new();
        for row in (ri..=rf).step_by(rp) {
            for col in (ci..=cf).step_by(cp) {
                for r in 0..rs {
                    for c in 0..cs {
                        out.push((col + c, row + r));
                    }
                }
            }
        }
        out
    }
}

/// Replaces the given pixels of an interleaved image by the mean of their usable neighbours:
/// all four when possible, otherwise the horizontal or the vertical pair (as x3f_tools does).
fn interpolate_pixels(
    data: &mut [u16],
    width: usize,
    height: usize,
    channels: usize,
    points: &[(usize, usize)],
) {
    let mut bad = vec![false; width * height];
    let points: Vec<_> = points
        .iter()
        .copied()
        .filter(|&(x, y)| x < width && y < height)
        .collect();
    for &(x, y) in &points {
        bad[y * width + x] = true;
    }
    let usable = |x: isize, y: isize| {
        (x >= 0
            && y >= 0
            && (x as usize) < width
            && (y as usize) < height
            && !bad[y as usize * width + x as usize])
            .then_some((x as usize, y as usize))
    };
    let fixes: Vec<_> = points
        .iter()
        .map(|&(x, y)| {
            let (xi, yi) = (x as isize, y as isize);
            let n = [
                usable(xi - 1, yi),
                usable(xi + 1, yi),
                usable(xi, yi - 1),
                usable(xi, yi + 1),
            ];
            let chosen: Vec<(usize, usize)> = if n.iter().all(Option::is_some) {
                n.iter().flatten().copied().collect()
            } else if n[0].is_some() && n[1].is_some() {
                vec![n[0].unwrap(), n[1].unwrap()]
            } else if n[2].is_some() && n[3].is_some() {
                vec![n[2].unwrap(), n[3].unwrap()]
            } else {
                n.iter().flatten().copied().collect()
            };
            (x, y, chosen)
        })
        .collect();
    for (x, y, chosen) in fixes {
        if chosen.is_empty() {
            continue;
        }
        for c in 0..channels {
            let sum: u32 = chosen
                .iter()
                .map(|&(nx, ny)| data[(ny * width + nx) * channels + c] as u32)
                .sum();
            let n = chosen.len() as u32;
            data[(y * width + x) * channels + c] = ((sum + n / 2) / n) as u16;
        }
    }
}

/// Interpolates over the autofocus pixels of sd Quattro / sd Quattro H files.
pub fn repair_autofocus_pixels(raw: &mut Raw, camf: &Camf) {
    let Raw::Quattro {
        low_width,
        low_height,
        low,
        top_width,
        top_height,
        top,
    } = raw
    else {
        return;
    };
    let (luma, chroma) = match camf.number("CAMERAID").map(|v| v as u32) {
        Some(40) => (
            Grid(217, 5641, 16, 1, 464, 3312, 32, 2),
            Grid(108, 2820, 8, 1, 232, 1656, 16, 1),
        ),
        Some(41) => (
            Grid(233, 6425, 16, 1, 592, 3888, 32, 2),
            Grid(116, 2820, 8, 1, 296, 1944, 16, 1),
        ),
        _ => return,
    };
    interpolate_pixels(top, *top_width, *top_height, 1, &luma.points());
    interpolate_pixels(low, *low_width, *low_height, 3, &chroma.points());
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
    /// Encoded 16 bit value (in `color_space`) for evenly spaced linear inputs in 0..=`curve_max`.
    curve: Vec<f64>,
    /// カーブが受け取るリニア値の上限(測ったカーブを持つ機種は 1 を超えることがある)。
    curve_max: f64,
    /// Per-channel linear gain applied before the curve (2^EV).
    gain: [f64; 3],
    tone: f64,
    /// Red-row correction applied to the finished picture, with the decoding it needs.
    red_row: Option<([f64; 3], Vec<f32>)>,
    color_space: ColorSpace,
    /// 場所ごとのゲイン(Merrill のシェーディング補正)。
    shading: Option<Shading>,
    /// Exposure that `Exposure::Auto` stands for, when it does not depend on the picture.
    fixed_exposure: Option<f64>,
    pub white_balance: String,
}

const CURVE_SIZE: usize = 4096;
/// Normalised raw level where the highlight blend towards white begins.
const CLIP_START: f64 = 0.9;
/// Limits of the blue-layer detail ratio applied to red and green (see `transfer_detail`).
const DETAIL_RATIO: (f64, f64) = (0.25, 4.0);
/// Blue level, as a fraction of its range, below which the detail correction fades out.
const DETAIL_FLOOR: f64 = 0.02;
/// How far the S-shaped tone curve is mixed in (0 = plain sRGB).
const TONE_STRENGTH: f64 = 0.5;

/// sRGB encoding followed by a gentle S-curve, like the contrast a camera JPEG has.
#[cfg(test)]
fn encode(l: f64) -> f64 {
    encode_with(l, TONE_STRENGTH)
}

fn encode_with(l: f64, tone: f64) -> f64 {
    let e = srgb_encode(l.clamp(0.0, 1.0));
    e + tone * (e * e * (3.0 - 2.0 * e) - e)
}

/// Exposure that "auto" means for the sd Quattro (CAMERAID 40): Photo Pro does not meter the
/// picture, it develops every file with the same gain. Against its TIFFs of three files (a bright
/// street, a sky-filled one and a dark scene) the exposure that fits best was +1.00, +1.00 and
/// +1.02 EV, while matching the camera's JPEG gave +1.30, +1.20 and -0.08. With the fixed value the
/// RMS error (/255) fell from 16.5, 10.5 and 15.1 to 10.4, 7.3 and 8.1. Other cameras keep
/// matching the JPEG; they were not measured.
const SD_QUATTRO_EXPOSURE_EV: f64 = 1.0;

/// Photo Pro's tone curve as measured on the sd Quattro at `SD_QUATTRO_EXPOSURE_EV`: the median
/// of its encoded channel value (0..255) against our linear value before the curve (log2, centres
/// of half-stop bins), pooled over the same three files. Compared with the sRGB curve with a mild S
/// (strength 0.5) it is 6-10 levels darker in the shadows (the blacks of ours were lifted, which
/// is why the picture looked flat), 3-6 lighter in the mid-tones and 8-16 lower at the top.
const SD_QUATTRO_TONE: [(f64, f64); 17] = [
    (-8.25, 4.0),
    (-7.75, 5.0),
    (-7.25, 6.0),
    (-6.75, 9.0),
    (-6.25, 12.0),
    (-5.75, 17.0),
    (-5.25, 23.0),
    (-4.75, 32.0),
    (-4.25, 44.0),
    (-3.75, 60.0),
    (-3.25, 81.0),
    (-2.75, 106.0),
    (-2.25, 132.0),
    (-1.75, 159.0),
    (-1.25, 184.0),
    (-0.75, 207.0),
    (-0.25, 228.0),
];
/// Where a fully saturated sensor value ends up (Photo Pro's skies read 251-253).
const SD_QUATTRO_WHITE: f64 = 252.0;

/// dp0 Quattro (CAMERAID 83) も Photo Pro は絵を測らず、どのファイルも同じゲインと同じトーンカーブで
/// 現像する。Photo Pro 6.9.0 の既定設定(露出 0、カラーモード Standard、ノイズリダクション既定)で
/// 書き出した 5 枚(ISO 100、色温度 5000K。夕方の明るい場面から暗い場面まで)で、平坦な 16 画素
/// ブロックの「カーブ前の自分のリニア値(log2、露出 0 EV)」に対する Photo Pro のエンコード値は、
/// 中間調で 5 枚とも ±3/255 以内に重なった(カメラの JPEG に合わせる自動露出は −0.23〜+0.94 EV と
/// ばらついていたのに)。表はその中央値(半段の箱の中心、全チャンネル)を sRGB エンコードの
/// 0..255 に直したもの。Photo Pro はリニア 1.0 を超える値も肩で受けるので、表はリニア 2.0
/// (`DP0_QUATTRO_WHITE`)まで続く。変更前は暗部が 12〜22/255 明るく、暗い 1 枚は中間調が
/// 20〜57/255 暗かった。dp1/dp2/dp3 Quattro(CAMERAID 80〜82)は参照が無いまま(ユーザーの指示で)
/// 同じ表・同じ固定露出を使う。測った値ではないので、参照が手に入ったら確かめること。
const DP0_QUATTRO_TONE: [(f64, f64); 20] = [
    (-8.75, 2.7),
    (-8.25, 4.1),
    (-7.75, 6.2),
    (-7.25, 9.3),
    (-6.75, 13.4),
    (-6.25, 18.7),
    (-5.75, 26.1),
    (-5.25, 36.2),
    (-4.75, 49.4),
    (-4.25, 67.9),
    (-3.75, 90.1),
    (-3.25, 118.6),
    (-2.75, 148.4),
    (-2.25, 175.4),
    (-1.75, 199.2),
    (-1.25, 219.3),
    (-0.75, 236.4),
    (-0.25, 249.4),
    (0.25, 253.4),
    (0.75, 254.6),
];
/// カーブが白に届くリニア値の log2 と、そのエンコード値。
const DP0_QUATTRO_WHITE: (f64, f64) = (1.0, 255.0);
/// dp0 Quattro の「自動」露出。上の表が 0 EV で測ってあるので 0。
const DP0_QUATTRO_EXPOSURE_EV: f64 = 0.0;

/// DP1 Merrill (CAMERAID 77) も同じで、Photo Pro は絵を測らない。Photo Pro 6.9.0 の既定設定(露出 0、
/// カラーモード Standard、WB オート、ISO 100)の 5 枚で、平坦な 16 画素ブロックの緑について
/// 「共通カーブを逆に引いた値 − 自分のリニア値(露出 0 EV)」の中央値は −0.02 / 0.00 / +0.02 /
/// −0.11 / +0.03 段だった(カメラの JPEG に合わせる自動露出は +1.22〜+1.40 EV)。表は緑の中央値
/// (半段の箱の中心)を sRGB エンコードに直したもの。箱の標本が少ない −3.75〜−2.75 は緑と
/// 全チャンネルの中央値の間を取った。DP2 / DP3 Merrill(CAMERAID 78)は同じセンサーと現像系なので、
/// 参照が無いまま(ユーザーの指示で)同じ表・同じ固定露出を使う。測った値ではないので、参照が
/// 手に入ったら確かめること。
/// 表はシェーディング補正(`Shading`)を入れた後に測り直したもの。補正前は画像ごとに中間調が
/// 最大 16/255 ばらついていたが、補正後は 5 枚が ±5/255 以内に重なる。
const DP1_MERRILL_TONE: [(f64, f64); 16] = [
    (-8.25, 6.5),
    (-7.75, 9.5),
    (-7.25, 13.3),
    (-6.75, 19.1),
    (-6.25, 26.5),
    (-5.75, 35.3),
    (-5.25, 47.4),
    (-4.75, 63.9),
    (-4.25, 85.8),
    (-3.75, 111.0),
    (-3.25, 131.0),
    (-2.75, 162.0),
    (-2.25, 190.0),
    (-1.75, 216.5),
    (-1.25, 238.5),
    (-0.75, 254.0),
];
/// カーブが白に届くリニア値の log2 と、そのエンコード値。
const DP1_MERRILL_WHITE: (f64, f64) = (-0.25, 255.0);
/// DP1 Merrill の「自動」露出。上の表が 0 EV で測ってあるので 0。
const DP1_MERRILL_EXPOSURE_EV: f64 = 0.0;

/// Photo Pro の出力から測ったトーンカーブ(機種ごと)。
struct MeasuredTone {
    /// (カーブ前のリニア値の log2, sRGB エンコード値 0..255)。
    nodes: &'static [(f64, f64)],
    /// 白に届く点。ここまでがカーブの定義域になる。
    white: (f64, f64),
    /// 「自動」露出が意味する固定の EV。
    exposure_ev: f64,
}

const SD_QUATTRO_MEASURED: MeasuredTone = MeasuredTone {
    nodes: &SD_QUATTRO_TONE,
    white: (0.0, SD_QUATTRO_WHITE),
    exposure_ev: SD_QUATTRO_EXPOSURE_EV,
};

const DP0_QUATTRO_MEASURED: MeasuredTone = MeasuredTone {
    nodes: &DP0_QUATTRO_TONE,
    white: DP0_QUATTRO_WHITE,
    exposure_ev: DP0_QUATTRO_EXPOSURE_EV,
};

const DP1_MERRILL_MEASURED: MeasuredTone = MeasuredTone {
    nodes: &DP1_MERRILL_TONE,
    white: DP1_MERRILL_WHITE,
    exposure_ev: DP1_MERRILL_EXPOSURE_EV,
};

impl MeasuredTone {
    /// カーブが受け取るリニア値の上限。
    fn max_linear(&self) -> f64 {
        2f64.powf(self.white.0)
    }

    /// リニア値 `l` に対する Photo Pro のエンコード値(0..=1)。
    fn curve(&self, l: f64) -> f64 {
        if l <= 0.0 {
            return 0.0;
        }
        let first = self.nodes[0];
        let x = l.log2();
        let y = if x < first.0 {
            // 測った範囲より下は黒までの直線。
            first.1 * 2f64.powf(x - first.0)
        } else {
            let mut nodes = self.nodes.to_vec();
            nodes.push(self.white);
            monotone_cubic(&nodes, x.min(self.white.0))
        };
        (y / 255.0).clamp(0.0, 1.0)
    }

    /// 強さ 0 で素の sRGB、`TONE_STRENGTH`(既定)で Photo Pro のカーブ、それ以上でさらに強く。
    fn encode(&self, l: f64, tone: f64) -> f64 {
        let e = srgb_encode(l.clamp(0.0, 1.0));
        (e + tone / TONE_STRENGTH * (self.curve(l.clamp(0.0, self.max_linear())) - e))
            .clamp(0.0, 1.0)
    }
}

/// Monotone cubic (Fritsch-Carlson) interpolation through `nodes`, extended to both ends.
fn monotone_cubic(nodes: &[(f64, f64)], x: f64) -> f64 {
    let n = nodes.len();
    let h: Vec<f64> = (0..n - 1).map(|i| nodes[i + 1].0 - nodes[i].0).collect();
    let d: Vec<f64> = (0..n - 1)
        .map(|i| (nodes[i + 1].1 - nodes[i].1) / h[i])
        .collect();
    let mut m = vec![0.0; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for i in 1..n - 1 {
        m[i] = if d[i - 1] * d[i] <= 0.0 {
            0.0
        } else {
            let w = (h[i - 1] + 2.0 * h[i], 2.0 * h[i - 1] + h[i]);
            (w.0 + w.1) / (w.0 / d[i - 1] + w.1 / d[i])
        };
    }
    let i = nodes.partition_point(|n| n.0 <= x).clamp(1, n - 1) - 1;
    let t = (x - nodes[i].0) / h[i];
    let (t2, t3) = (t * t, t * t * t);
    (2.0 * t3 - 3.0 * t2 + 1.0) * nodes[i].1
        + (t3 - 2.0 * t2 + t) * h[i] * m[i]
        + (-2.0 * t3 + 3.0 * t2) * nodes[i + 1].1
        + (t3 - t2) * h[i] * m[i + 1]
}

/// Photo Pro's sd Quattro curve for a linear value `l` in 0..=1, encoded 0..=1.
#[cfg(test)]
fn sd_quattro_curve(l: f64) -> f64 {
    SD_QUATTRO_MEASURED.curve(l)
}

/// `encode_with` for the sd Quattro: the strength moves the curve between plain sRGB (0) and
/// Photo Pro's (`TONE_STRENGTH`, the default), and beyond it for more contrast.
#[cfg(test)]
fn encode_sd_quattro(l: f64, tone: f64) -> f64 {
    SD_QUATTRO_MEASURED.encode(l, tone)
}

/// Linear light for every 16 bit sRGB-encoded value.
fn srgb_decoding() -> Vec<f32> {
    (0..=u16::MAX)
        .map(|v| {
            let e = v as f64 / 65535.0;
            (if e <= 0.04045 {
                e / 12.92
            } else {
                ((e + 0.055) / 1.055).powf(2.4)
            }) as f32
        })
        .collect()
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

/// sd Quattro files shot with auto white balance (code 1). The camera's own Auto estimate varies
/// from scene to scene, while Photo Pro's TIFFs stay close to the Sunlight preset: over 13 files
/// (gray patches, red/blue relative to green, /255) Sunlight's mean error is 2.9 against 5.0 for
/// Auto and its worst 6.2 against 10.6, better in 10 files and worse in 3. Only the sd Quattro
/// (CAMERAID 40) was measured; the sd Quattro H keeps its Auto.
fn is_sd_quattro_auto(camf: &Camf) -> bool {
    camf.number("CAMERAID") == Some(40.0) && camf.number("WhiteBalance") == Some(1.0)
}

/// sd Quattro (CAMERAID 40) greens come out yellow: against Photo Pro's TIFFs of three files
/// (sky, street, dark scene) the hue of foliage is 5-8 degrees towards yellow. The red channel of
/// the finished picture is corrected as `R + a (G - R) + b (B - R)` with a = -0.20 and b = +0.12,
/// i.e. the row below, in linear light (it sums to 1, so neutrals stay put). The strength is the
/// one that brings the green hue to within 3.5 degrees of Photo Pro's in all three files; the
/// error metric hardly moves with it. The same correction on the blue row did not help, and a full
/// 3x3 fit overfitted. Only the sd Quattro at 5000 K sunlight was measured.
const SD_QUATTRO_RED_ROW: [f64; 3] = [1.08, -0.20, 0.12];

fn is_sd_quattro(camf: &Camf) -> bool {
    camf.number("CAMERAID") == Some(40.0)
}

/// Photo Pro のカーブを測ってある機種なら、その表。
fn measured_tone(camf: &Camf) -> Option<&'static MeasuredTone> {
    match camf.number("CAMERAID").map(|v| v as u32) {
        Some(40) => Some(&SD_QUATTRO_MEASURED),
        // 78(DP2 / DP3 Merrill)は参照が無く、DP1 と同じ扱いにしている。
        Some(77 | 78) => Some(&DP1_MERRILL_MEASURED),
        // 80〜82(dp1〜dp3 Quattro)は参照が無く、dp0 と同じ扱いにしている。
        Some(80..=83) => Some(&DP0_QUATTRO_MEASURED),
        _ => None,
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

/// Photo Pro renders Merrill files with a red gain above the file's white balance gain. It was
/// 1.04 (fitted against its TIFF output; the camera JPEGs of four more files agreed within
/// 1.038-1.044) while the picture had no shading correction. With `Shading` in place the same
/// comparison on five DP1 Merrill files (Photo Pro 6.9.0, defaults) left R−G at +3.3 / +3.1 /
/// +2.4 / +2.2 / +0.8 of 255; 1.5% less red brings it to +1.4 / +0.9 / +0.2 / +0.1 / −1.1 and the
/// RMS error from 8.32 / 7.35 / 7.97 / 12.01 / 8.80 to 8.17 / 7.26 / 7.94 / 11.89 / 8.74, and 3%
/// less overshoots (−0.5 … −3.0). Quattro needs no such trim (0.8%).
const MERRILL_RED_TRIM: f64 = 1.024;

/// The table's gains are relative to a green gain of about 0.855, while the per-white-balance
/// `WBGain` entries are relative to 1. Found by comparing with those entries (about 1.16), then
/// calibrated on the developed output itself against Photo Pro's TIFF: four dp0 Quattro files
/// (all at 5000K) agree on red 1.18-1.19 and blue 1.16-1.19 (a single 1.17 left red about 4/255
/// low). Calibrate on the finished output, not on a raw-mode TIFF: that one lacks the highlight
/// pixel repair, and the colour matrix amplifies a few percent of green into ~20% of blue.
///
/// 青は、dp0 Quattro に Photo Pro のトーンカーブ(`DP0_QUATTRO_TONE`)を入れたあとで測り直した。
/// Photo Pro 6.9.0 の既定設定の TIFF 5 枚(5000K)に対し、1.17 のままだと B−G が
/// +3.4/+4.1/+1.6/+7.0/+7.6(/255)と 5 枚とも青く、B−G が 0 になる倍率は 0.983/0.982/0.986/0.975/0.976
/// 倍に揃った。0.98 倍(1.147)で B−G は −0.5/−0.3/−0.7/+1.4/+1.3、R−G は +0.5/+1.2/+0.2/+1.1/−0.2。
/// 赤は動かさない(1.005 倍にすると R−G が +0.5〜+2.0 に悪化)。
///
/// 青の 1.147 は dp0 Quattro のトーン(と、同じトーンを流用する dp1〜dp3 Quattro)に対して測ったので、
/// その機種だけに掛ける(`color_temp_blue_scale`)。ほかの Quattro(sd Quattro など)は、トーンを
/// 入れる前の 1.17 のまま。色温度指定の参照は dp0 の 5000K だけで、sd Quattro は測っていない。
const COLOR_TEMP_RED_SCALE: f64 = 1.185;
const COLOR_TEMP_BLUE_SCALE: f64 = 1.17;
const DP0_COLOR_TEMP_BLUE_SCALE: f64 = 1.147;

/// 色温度指定 WB の青の倍率。dp0〜dp3 Quattro(CAMERAID 80〜83)だけ測り直した値。
fn color_temp_blue_scale(camf: &Camf) -> f64 {
    match camf.number("CAMERAID").map(|v| v as u32) {
        Some(80..=83) => DP0_COLOR_TEMP_BLUE_SCALE,
        _ => COLOR_TEMP_BLUE_SCALE,
    }
}

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
    let gain = [
        row[2] * COLOR_TEMP_RED_SCALE,
        1.0,
        row[1] * color_temp_blue_scale(camf),
    ];
    let cc: [f64; 9] = std::array::from_fn(|c| row[3 + c]);
    Some((kelvin, gain, cc))
}

impl Developer {
    /// `header_wb` is the white balance name from the X3F header (older files only).
    pub fn new(
        raw: &Raw,
        camf: &Camf,
        header_wb: Option<&str>,
        tuning: &Tuning,
    ) -> Result<Developer, Error> {
        let tuning = tuning.sanitized();
        let depth = image_depth(camf).ok_or(Error::MissingCalibration("ImageDepth"))?;
        let max = ((1u32 << depth) - 1) as f64;
        let black = black_levels(raw, camf)?;

        // The file's own setting first; Auto / Sunlight when that has no matrices (e.g. ColorTemp).
        let wanted = match camf.number("WhiteBalance") {
            Some(code) => wb_from_code(code as u32).to_owned(),
            None => header_wb.unwrap_or("Auto").to_owned(),
        };
        let wanted = if is_sd_quattro_auto(camf) {
            "Sunlight".to_owned()
        } else {
            wanted
        };
        let wanted = tuning.white_balance.map_or(wanted, str::to_owned);
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
        gain[0] *= tuning.red;
        gain[2] *= tuning.blue;
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
        let sd_quattro = is_sd_quattro(camf);
        let measured = measured_tone(camf);
        let curve_max = measured.map_or(1.0, MeasuredTone::max_linear);
        let mut curve: Vec<f64> = (0..=CURVE_SIZE)
            .map(|i| {
                let l = i as f64 / CURVE_SIZE as f64 * curve_max;
                let e = match measured {
                    Some(m) => m.encode(l, tuning.tone),
                    None => encode_with(l, tuning.tone),
                };
                // 表とカーブは sRGB エンコードで持っている。ほかの色空間では同じリニア値を
                // その色空間のガンマでエンコードし直す(Photo Pro のカーブは出力色空間の
                // チャンネルごとに掛かっている)。
                let e = match tuning.color_space {
                    ColorSpace::Srgb => e,
                    other => other.encode(srgb_decode(e.clamp(0.0, 1.0))),
                };
                e * 65535.0
            })
            .collect();
        // A strength above the default can bend the sd Quattro curve backwards; never let it.
        for i in 1..curve.len() {
            curve[i] = curve[i].max(curve[i - 1]);
        }
        Ok(Developer {
            black,
            white: [max; 3],
            matrix,
            curve,
            curve_max,
            gain: [1.0; 3],
            tone: tuning.tone,
            red_row: sd_quattro.then(|| (SD_QUATTRO_RED_ROW, tuning.color_space.decoding())),
            color_space: tuning.color_space,
            shading: Shading::new(raw, camf),
            fixed_exposure: measured.map(|m| m.exposure_ev),
            white_balance: name,
        })
    }

    /// The exposure that "auto" means for this camera when it does not depend on the picture.
    pub fn fixed_exposure(&self) -> Option<f64> {
        self.fixed_exposure
    }

    /// 出力の色空間。
    pub fn color_space(&self) -> ColorSpace {
        self.color_space
    }

    /// The tone curve strength this developer was built with.
    pub fn tone(&self) -> f64 {
        self.tone
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
    pub fn exposure_for(scene: f64, target: f64, tone: f64) -> f64 {
        let (mut lo, mut hi) = EV_RANGE;
        for _ in 0..40 {
            let mid = (lo + hi) / 2.0;
            if encode_with(scene * 2f64.powf(mid), tone) < target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (lo + hi) / 2.0
    }

    /// Gives a Quattro pixel's red and green the fine detail of the blue layer.
    ///
    /// The red and green layers have a quarter of the pixels of the blue one, so interpolated they
    /// are softer than blue, and every edge gets a coloured fringe. `px` is the pixel as
    /// interpolated (blue at full resolution) and `blue_low` is blue as the other layers see it
    /// (averaged to their grid and interpolated the same way). Scaling red and green by the ratio
    /// of the two keeps the local colour and moves the blue layer's brightness detail onto them.
    /// Where blue is nearly black the ratio is mostly noise, so the correction fades out there.
    pub fn transfer_detail(&self, px: &mut [u16], blue_low: u16) {
        let range = self.white[2] - self.black[2];
        let low = blue_low as f64 - self.black[2];
        let full = px[2] as f64 - self.black[2];
        if low <= 0.0 || range <= 0.0 {
            return;
        }
        let ratio = (full / low).clamp(DETAIL_RATIO.0, DETAIL_RATIO.1);
        let weight = low / (low + DETAIL_FLOOR * range);
        let scale = 1.0 + (ratio - 1.0) * weight;
        for (c, v) in px.iter_mut().take(2).enumerate() {
            let scaled = (*v as f64 - self.black[c]) * scale + self.black[c];
            *v = scaled.round().clamp(0.0, u16::MAX as f64) as u16;
        }
    }

    /// Converts one row of interleaved raw RGB (sensor units) in place. The pixels have no
    /// position, so position-dependent corrections are not applied.
    pub fn apply(&self, row: &mut [u16]) {
        for px in row.chunks_exact_mut(3) {
            self.apply_pixel(px, [1.0; 3]);
        }
    }

    /// `apply` と同じだが、画素の位置(センサー全体の座標。行 `y`、`i` 番目の画素の列は
    /// `x_of(i)`)を使ってシェーディング補正も掛ける。
    pub fn apply_at(&self, row: &mut [u16], y: usize, x_of: impl Fn(usize) -> usize) {
        let Some(shading) = &self.shading else {
            return self.apply(row);
        };
        let line = shading.line(y);
        for (i, px) in row.chunks_exact_mut(3).enumerate() {
            self.apply_pixel(px, shading.at(&line, x_of(i)));
        }
    }

    fn apply_pixel(&self, px: &mut [u16], shade: [f64; 3]) {
        {
            let raw: [f64; 3] = std::array::from_fn(|c| {
                ((px[c] as f64 - self.black[c]) / (self.white[c] - self.black[c])).max(0.0)
            });
            let n: [f64; 3] = std::array::from_fn(|c| raw[c] * shade[c]);
            let srgb: [f64; 3] = std::array::from_fn(|c| {
                let m = &self.matrix[c];
                (m[0] * n[0] + m[1] * n[1] + m[2] * n[2]) * self.gain[c]
            });
            // 色空間の変換はクランプの前。sRGB の外の色(負の成分)が Adobe RGB では残る。
            let mut lin = match self.color_space {
                ColorSpace::Srgb => srgb,
                ColorSpace::AdobeRgb => {
                    SRGB_TO_ADOBE.map(|m| m[0] * srgb[0] + m[1] * srgb[1] + m[2] * srgb[2])
                }
            }
            .map(|l| l.clamp(0.0, self.curve_max));
            // A saturated channel under-reports its colour, which turns bright skies yellow-green.
            // Blend those pixels towards white as the brightest channel nears saturation.
            let peak = raw.iter().cloned().fold(0.0, f64::max);
            let t = ((peak - CLIP_START) / (1.0 - CLIP_START)).clamp(0.0, 1.0);
            if t > 0.0 {
                let top = lin.iter().cloned().fold(0.0, f64::max);
                for l in &mut lin {
                    *l += (top - *l) * t;
                }
            }
            for (out, l) in px.iter_mut().zip(lin) {
                let pos = l / self.curve_max * CURVE_SIZE as f64;
                let i = (pos as usize).min(CURVE_SIZE - 1);
                let f = pos - i as f64;
                *out = (self.curve[i] * (1.0 - f) + self.curve[i + 1] * f).round() as u16;
            }
            if let Some((row, decode)) = &self.red_row {
                let l: [f64; 3] = std::array::from_fn(|k| decode[px[k] as usize] as f64);
                match self.color_space {
                    ColorSpace::Srgb => {
                        let r: f64 = (0..3).map(|k| row[k] * l[k]).sum();
                        px[0] = (srgb_encode(r.clamp(0.0, 1.0)) * 65535.0).round() as u16;
                    }
                    space => {
                        // 補正は sRGB の赤に対するもの。Adobe RGB の緑は sRGB の緑と同じで、
                        // 青は sRGB の赤を含まないので、変わるのは Adobe RGB の赤だけ。
                        let a = &ADOBE_TO_SRGB_RED;
                        let (g, b) = (SRGB_TO_ADOBE[2][1], SRGB_TO_ADOBE[2][2]);
                        let s = [
                            a[0] * l[0] + a[1] * l[1] + a[2] * l[2],
                            l[1],
                            (l[2] - g * l[1]) / b,
                        ];
                        let r: f64 = (0..3).map(|k| row[k] * s[k]).sum();
                        let out = l[0] + SRGB_TO_ADOBE[0][0] * (r - s[0]);
                        px[0] = (space.encode(out.clamp(0.0, 1.0)) * 65535.0).round() as u16;
                    }
                }
            }
        }
    }
}

/// DP Merrill のシェーディング補正(周辺の光量落ちと、層ごとに違う色かぶり)。
///
/// CAMF には絞りごと(`SpatialGain_Fstop` = 2.8 / 4 / 8)に、センサー全体を覆う 52x78 の表
/// `SpatialGain{G,B}_{絞りの番号}_{INF,MOD}` がある(0〜255 の整数、中央が 0〜1、隅が最大)。
/// 換算式は CAMF に無いので、Photo Pro 6.9.0 の既定設定の TIFF 5 枚(DP1 Merrill、F2.8〜5.6)から
/// 求めた: 平坦な 16 画素ブロックごとに「Photo Pro と同じ色になるために各層に必要なゲイン」を
/// 逆算すると、表の値と相関 0.84〜0.97 で直線に乗る。赤の表は無く、赤に必要なゲインは緑の表より
/// 青の表によく乗る(残差 0.033〜0.053 対 0.036〜0.060)。
///
/// 層ごとのゲイン = 1 + 表の値 / `SHADING_SCALE`(赤は青の表を `SHADING_RED_SCALE` で)。
/// 灰色に近いブロックの「Photo Pro との色の比のずれ」の場所によるばらつき(段、5 枚の平均)は、
/// 補正なしで 青/緑 0.144・赤/緑 0.095、この式で 0.029・0.037。係数 400〜520 では 0.030〜0.034 と
/// 平らで、赤を緑の表にすると 0.065、赤を補正しないと 0.156 に悪化する。絞りの間の補間方法と
/// INF / MOD の違いは 0.002 以内なので、絞りは log2 で補間し、表は INF だけを使う。
/// 画像ごとの直線の傾きは 1/380〜1/805 とばらつく(F2.8 の 2 枚が緩い)。明るさの周辺落ちには
/// まだ合わせ切れていない。
struct Shading {
    rows: usize,
    cols: usize,
    /// 層(R, G, B)ごとのゲインの表。
    gain: [Vec<f32>; 3],
    width: usize,
    height: usize,
}

const SHADING_SCALE: f64 = 450.0;
const SHADING_RED_SCALE: f64 = 435.0;
/// 表の大きさの上限(壊れた CAMF で巨大な表を作らない)。
const SHADING_MAX_CELLS: usize = 1 << 16;

impl Shading {
    fn new(raw: &Raw, camf: &Camf) -> Option<Shading> {
        let Raw::Merrill { width, height, .. } = raw else {
            return None;
        };
        if *width < 2 || *height < 2 {
            return None;
        }
        let stops = camf.matrix("SpatialGain_Fstop")?;
        let aperture = camf.number("CaptureAperture").filter(|a| *a > 0.0)?;
        if stops.iter().any(|f| f.is_nan() || *f <= 0.0) {
            return None;
        }
        // 絞り: 隣り合う 2 つの表を log2 で混ぜる。範囲の外は端の表。
        let hi = stops
            .iter()
            .position(|&f| f >= aperture)
            .unwrap_or(stops.len() - 1);
        let lo = hi.saturating_sub(1);
        let t = if hi == lo || stops[hi] <= stops[lo] {
            1.0
        } else {
            ((aperture / stops[lo]).log2() / (stops[hi] / stops[lo]).log2()).clamp(0.0, 1.0)
        };
        let table = |chan: &str, scale: f64| -> Option<(usize, usize, Vec<f32>)> {
            let mut out: Option<(usize, usize, Vec<f32>)> = None;
            for (index, w) in [(lo, 1.0 - t), (hi, t)] {
                if w == 0.0 {
                    continue;
                }
                let (dims, values) =
                    camf.matrix_with_dims(&format!("SpatialGain{chan}_{index}_INF"))?;
                if dims.len() != 2
                    || dims[0] < 2
                    || dims[1] < 2
                    || values.len() > SHADING_MAX_CELLS
                    || values.iter().any(|v| !(0.0..=65535.0).contains(v))
                {
                    return None;
                }
                let o = out.get_or_insert_with(|| (dims[0], dims[1], vec![1.0; values.len()]));
                if (o.0, o.1) != (dims[0], dims[1]) {
                    return None;
                }
                for (a, v) in o.2.iter_mut().zip(values) {
                    *a += (w * v / scale) as f32;
                }
            }
            out
        };
        let g = table("G", SHADING_SCALE)?;
        let b = table("B", SHADING_SCALE).filter(|b| (b.0, b.1) == (g.0, g.1))?;
        let r = table("B", SHADING_RED_SCALE)?;
        Some(Shading {
            rows: g.0,
            cols: g.1,
            gain: [r.2, g.2, b.2],
            width: *width,
            height: *height,
        })
    }

    /// 行 `y` での、格子の列ごとのゲイン。
    fn line(&self, y: usize) -> [Vec<f32>; 3] {
        let pos = y.min(self.height - 1) as f64 / (self.height - 1).max(1) as f64
            * (self.rows - 1) as f64;
        let r = (pos as usize).min(self.rows - 2);
        let f = (pos - r as f64) as f32;
        std::array::from_fn(|c| {
            let g = &self.gain[c];
            (0..self.cols)
                .map(|i| g[r * self.cols + i] * (1.0 - f) + g[(r + 1) * self.cols + i] * f)
                .collect()
        })
    }

    fn at(&self, line: &[Vec<f32>; 3], x: usize) -> [f64; 3] {
        let pos =
            x.min(self.width - 1) as f64 / (self.width - 1).max(1) as f64 * (self.cols - 1) as f64;
        let i = (pos as usize).min(self.cols - 2);
        let f = pos - i as f64;
        std::array::from_fn(|c| line[c][i] as f64 * (1.0 - f) + line[c][i + 1] as f64 * f)
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

pub(crate) fn sample_rgb(raw: &Raw, x: usize, y: usize) -> [u16; 3] {
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

    fn with_camera(camera: f64) -> Camf {
        let mut c = camf(1.0);
        c.0.push(m("CAMERAID", &[camera]));
        c
    }

    #[test]
    fn sd_quattro_auto_uses_the_sunlight_gains() {
        let d = Developer::new(&raw(), &with_camera(40.0), None, &Tuning::default()).unwrap();
        assert_eq!(d.white_balance, "Sunlight");
    }

    #[test]
    fn other_cameras_keep_their_auto_white_balance() {
        for camera in [41.0, 83.0] {
            let d = Developer::new(&raw(), &with_camera(camera), None, &Tuning::default()).unwrap();
            assert_eq!(d.white_balance, "Auto", "camera {camera}");
        }
        let d = Developer::new(&raw(), &camf(1.0), None, &Tuning::default()).unwrap();
        assert_eq!(d.white_balance, "Auto");
    }

    fn developed(tuning: Tuning) -> Vec<u16> {
        let d = Developer::new(&raw(), &camf(1.0), None, &tuning).unwrap();
        let v = 100 + (0.1 * (4095.0 - 100.0)) as u16;
        let mut row = [v; 3];
        d.apply(&mut row);
        row.to_vec()
    }

    #[test]
    fn default_tuning_changes_nothing() {
        let t = Tuning::default();
        assert_eq!(
            (t.white_balance, t.red, t.blue, t.tone),
            (None, 1.0, 1.0, TONE_STRENGTH)
        );
        assert_eq!(t.sanitized(), t);
        // Same curve as the fixed one used before the tuning existed.
        for l in [0.0, 0.05, 0.3, 0.9, 1.0] {
            assert_eq!(encode_with(l, t.tone), encode(l));
        }
    }

    #[test]
    fn out_of_range_tuning_falls_back_to_the_defaults() {
        let bad = Tuning {
            color_space: ColorSpace::AdobeRgb,
            white_balance: Some("Sunlight"),
            red: f64::NAN,
            blue: 99.0,
            tone: -1.0,
            grain: 2.0,
        };
        let s = bad.sanitized();
        assert_eq!(s.grain, 0.0);
        assert_eq!(s.color_space, ColorSpace::AdobeRgb);
        assert_eq!((s.red, s.blue, s.tone), (1.0, 1.0, TONE_STRENGTH));
        assert_eq!(s.white_balance, Some("Sunlight"));
        let edge = Tuning {
            red: GAIN_RANGE.0,
            blue: GAIN_RANGE.1,
            tone: 1.0,
            ..Tuning::default()
        };
        assert_eq!(edge.sanitized(), edge);
    }

    #[test]
    fn white_balance_names_must_be_known() {
        assert_eq!(white_balance_name("Overcast"), Some("Overcast"));
        assert_eq!(white_balance_name("overcast"), None);
        assert_eq!(white_balance_name("../etc"), None);
    }

    #[test]
    fn red_and_blue_multipliers_scale_only_their_channel() {
        let base = developed(Tuning::default());
        let red = developed(Tuning {
            red: 1.5,
            ..Tuning::default()
        });
        let blue = developed(Tuning {
            blue: 0.7,
            ..Tuning::default()
        });
        assert!(
            red[0] > base[0] && red[1] == base[1] && red[2] == base[2],
            "{base:?} {red:?}"
        );
        assert!(
            blue[2] < base[2] && blue[0] == base[0] && blue[1] == base[1],
            "{base:?} {blue:?}"
        );
    }

    #[test]
    fn tone_zero_is_plain_srgb() {
        let plain = developed(Tuning {
            tone: 0.0,
            ..Tuning::default()
        });
        let expected = (srgb_encode(0.1) * 65535.0).round();
        assert!(
            (plain[1] as f64 - expected).abs() < 40.0,
            "{plain:?} {expected}"
        );
        assert_ne!(plain, developed(Tuning::default()));
    }

    #[test]
    fn a_chosen_white_balance_replaces_the_files_own() {
        // The file is shot on Auto (red gain 2); asking for Sunlight (gain 1) must win.
        let auto = developed(Tuning::default());
        let sun = developed(Tuning {
            white_balance: Some("Sunlight"),
            ..Tuning::default()
        });
        assert!(sun[0] < auto[0], "{auto:?} {sun:?}");
        let d = Developer::new(
            &raw(),
            &camf(1.0),
            None,
            &Tuning {
                white_balance: Some("Sunlight"),
                ..Tuning::default()
            },
        )
        .unwrap();
        assert_eq!(d.white_balance, "Sunlight");
    }

    #[test]
    fn raw_shift_fills_the_16_bit_range() {
        assert_eq!(raw_shift(&Camf(vec![m("ImageDepth", &[14.0])])), 2);
        assert_eq!(raw_shift(&Camf(vec![m("ImageDepth", &[12.0])])), 4);
        assert_eq!(raw_shift(&Camf::default()), 0);
    }

    #[test]
    fn black_is_measured_and_subtracted_and_white_maps_to_full_scale() {
        let d = Developer::new(&raw(), &camf(1.0), None, &Tuning::default()).unwrap();
        assert_eq!(d.black, [100.0; 3]);
        let mut row = [100u16, 100, 100, 4095, 4095, 4095];
        d.apply(&mut row);
        assert_eq!(&row[..3], &[0, 0, 0]); // black stays black
        assert_eq!(row[4], 65535); // white, green gain 1
        assert_eq!(row[3], 65535); // red gain 2 clips
    }

    #[test]
    fn white_balance_gain_scales_red_before_the_curve() {
        let d = Developer::new(&raw(), &camf(1.0), None, &Tuning::default()).unwrap();
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
            let ev = Developer::exposure_for(0.1, target, TONE_STRENGTH);
            assert!(
                (encode(0.1 * 2f64.powf(ev)) - target).abs() < 1e-6,
                "{target} -> {ev}"
            );
        }
        // A scene already at the target needs no compensation.
        assert!(Developer::exposure_for(0.18, encode(0.18), TONE_STRENGTH).abs() < 1e-6);
    }

    #[test]
    fn one_ev_doubles_linear_light() {
        let mut d = Developer::new(&raw(), &camf(1.0), None, &Tuning::default()).unwrap();
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
    fn highlight_pixels_are_replaced_by_their_neighbours() {
        // 4x4 low layer, all 100 except the grid pixel at (1, 1) which reads half.
        let quattro = || {
            let mut low = vec![100u16; 4 * 4 * 3];
            for c in 0..3 {
                low[(4 + 1) * 3 + c] = 50;
            }
            Raw::Quattro {
                low_width: 4,
                low_height: 4,
                low,
                top_width: 8,
                top_height: 8,
                top: vec![0; 64],
            }
        };
        let pixel = |raw: &Raw| match raw {
            Raw::Quattro { low, .. } => low[(4 + 1) * 3],
            _ => unreachable!(),
        };
        let mut raw = quattro();
        repair_highlight_pixels(
            &mut raw,
            &Camf(vec![m("HighlightPixelsInfo", &[1.0, 1.0, 2.0, 2.0])]),
        );
        assert_eq!(pixel(&raw), 100);
        // Files without the entry are left alone.
        let mut untouched = quattro();
        repair_highlight_pixels(&mut untouched, &Camf::default());
        assert_eq!(pixel(&untouched), 50);
    }

    #[test]
    fn repair_highlight_pixels_on_a_single_pixel_layer_does_not_panic() {
        // 隣の画素が 1 つも無い 1x1 の層(壊れたファイル)。そのまま残す。
        let mut raw = Raw::Quattro {
            low_width: 1,
            low_height: 1,
            low: vec![50; 3],
            top_width: 2,
            top_height: 2,
            top: vec![0; 4],
        };
        repair_highlight_pixels(
            &mut raw,
            &Camf(vec![m("HighlightPixelsInfo", &[0.0, 0.0, 2.0, 2.0])]),
        );
        let Raw::Quattro { low, .. } = &raw else {
            unreachable!()
        };
        assert_eq!(low, &[50; 3]);
    }

    #[test]
    fn white_balance_names_match_the_keys_real_files_use() {
        // 実ファイル(dp1 Merrill / dp0 Quattro / sd Quattro)の CAMF のキーは Shade / Fluorescent。
        // 旧いヘッダの綴り(Shadow / Florescent)はそこへ寄せる。逆向きにすると引けなくなり、
        // 黙って Auto に落ちる。
        assert_eq!(canonical_wb("Shadow"), "Shade");
        assert_eq!(canonical_wb("Florescent"), "Fluorescent");
        assert_eq!(wb_from_code(3), "Shade");
        assert_eq!(wb_from_code(6), "Fluorescent");
        let mut c = camf(3.0);
        c.0.retain(|e| !e.name.starts_with("WhiteBalance"));
        c.0.push(m("WhiteBalance", &[3.0]));
        c.0.push(props(
            "WhiteBalanceGains",
            &[("Auto", "AutoGain"), ("Shade", "SunGain")],
        ));
        c.0.push(props(
            "WhiteBalanceColorCorrections",
            &[("Auto", "AutoCC"), ("Shade", "SunCC")],
        ));
        let d = Developer::new(&raw(), &c, None, &Tuning::default()).unwrap();
        assert_eq!(d.white_balance, "Shade");
    }

    #[test]
    fn autofocus_grid_pixels_are_interpolated_for_sd_quattro_only() {
        let quattro = || Raw::Quattro {
            low_width: 4,
            low_height: 4,
            low: vec![100; 4 * 4 * 3],
            top_width: 8,
            top_height: 8,
            top: vec![100; 64],
        };
        // The real grids sit far outside this tiny image, so check the interpolation directly.
        let mut data = vec![100u16; 6 * 6];
        data[2 * 6 + 2] = 40;
        data[2 * 6 + 3] = 40; // two vertically-adjacent-free neighbours (horizontal pair of bad pixels)
        interpolate_pixels(&mut data, 6, 6, 1, &[(2, 2), (3, 2)]);
        assert_eq!((data[2 * 6 + 2], data[2 * 6 + 3]), (100, 100));
        // Other cameras are left alone.
        let mut raw = quattro();
        repair_autofocus_pixels(&mut raw, &Camf(vec![m("CAMERAID", &[83.0])]));
        let Raw::Quattro { top, .. } = &raw else {
            unreachable!()
        };
        assert!(top.iter().all(|&v| v == 100));
    }

    #[test]
    fn color_temp_interpolates_the_table() {
        let (k, gain, cc) = color_temp_calibration(&color_temp_camf(5000.0)).unwrap();
        assert_eq!(k, 5000.0);
        // Halfway: red 0.9, blue 0.9, each scaled to the WBGain convention; green stays 1.
        assert!((gain[0] - 0.9 * COLOR_TEMP_RED_SCALE).abs() < 1e-9);
        // 機種が分からないファイルは、測り直す前の青の倍率のまま。
        assert!((gain[2] - 0.9 * COLOR_TEMP_BLUE_SCALE).abs() < 1e-9);
        assert_eq!(gain[1], 1.0);
        assert_eq!(&cc[..3], &[3.0, -3.0, 1.0]);
        // Outside the table the nearest row is used.
        let (_, low, _) = color_temp_calibration(&color_temp_camf(1000.0)).unwrap();
        assert!((low[0] - 0.8 * COLOR_TEMP_RED_SCALE).abs() < 1e-9);
    }

    #[test]
    fn color_temp_blue_scale_is_remeasured_only_for_dp0_to_dp3_quattro() {
        let blue = |camera: Option<f64>| {
            let mut c = color_temp_camf(5000.0);
            if let Some(id) = camera {
                c.0.push(m("CAMERAID", &[id]));
            }
            let (_, gain, _) = color_temp_calibration(&c).unwrap();
            gain[2] / 0.9
        };
        for id in [80.0, 81.0, 82.0, 83.0] {
            assert!(
                (blue(Some(id)) - DP0_COLOR_TEMP_BLUE_SCALE).abs() < 1e-9,
                "{id}"
            );
        }
        // sd Quattro(40)、sd Quattro H(41)、隣の ID、機種不明は従来の倍率。
        for id in [
            Some(40.0),
            Some(41.0),
            Some(79.0),
            Some(84.0),
            Some(77.0),
            None,
        ] {
            assert!((blue(id) - COLOR_TEMP_BLUE_SCALE).abs() < 1e-9, "{id:?}");
        }
        // 赤は機種によらず同じ。
        let red = |id: f64| {
            let mut c = color_temp_camf(5000.0);
            c.0.push(m("CAMERAID", &[id]));
            color_temp_calibration(&c).unwrap().1[0]
        };
        assert_eq!(red(83.0), red(40.0));
    }

    #[test]
    fn color_temp_setting_uses_the_table_instead_of_auto() {
        let d = Developer::new(&raw(), &color_temp_camf(5000.0), None, &Tuning::default()).unwrap();
        assert_eq!(d.white_balance, "ColorTemp 5000K");
        // Without the table it falls back to Auto as before.
        let d = Developer::new(&raw(), &camf(11.0), None, &Tuning::default()).unwrap();
        assert_eq!(d.white_balance, "Auto");
    }

    #[test]
    fn unknown_white_balance_falls_back_to_auto() {
        // 11 = ColorTemp has no matrices in the lists.
        let d = Developer::new(&raw(), &camf(11.0), None, &Tuning::default()).unwrap();
        assert_eq!(d.white_balance, "Auto");
    }

    #[test]
    fn missing_calibration_is_reported() {
        let err = Developer::new(&raw(), &Camf::default(), None, &Tuning::default())
            .err()
            .unwrap();
        assert!(matches!(err, Error::MissingCalibration(_)));
    }

    fn detail_developer() -> Developer {
        Developer::new(&raw(), &camf(1.0), None, &Tuning::default()).unwrap()
    }

    /// Blue level `f` of the way from black to white, in raw units.
    fn blue_at(d: &Developer, f: f64) -> u16 {
        (d.black[2] + f * (d.white[2] - d.black[2])).round() as u16
    }

    #[test]
    fn detail_transfer_leaves_flat_areas_alone() {
        let d = detail_developer();
        let b = blue_at(&d, 0.5);
        let mut px = [1500, 2200, b];
        d.transfer_detail(&mut px, b);
        assert_eq!(px, [1500, 2200, b]);
    }

    #[test]
    fn detail_transfer_follows_the_blue_layers_brightness() {
        let d = detail_developer();
        let (r, g) = (1500u16, 2200u16);
        // Brighter than the blue seen by the other layers: red and green rise, in proportion.
        let mut px = [r, g, blue_at(&d, 0.6)];
        d.transfer_detail(&mut px, blue_at(&d, 0.4));
        assert!(px[0] > r && px[1] > g);
        let (rise_r, rise_g) = (
            (px[0] as f64 - d.black[0]) / (r as f64 - d.black[0]),
            (px[1] as f64 - d.black[1]) / (g as f64 - d.black[1]),
        );
        assert!((rise_r - rise_g).abs() < 0.01, "{rise_r} vs {rise_g}");
        // Darker: they fall. Blue itself is never touched.
        let mut px = [r, g, blue_at(&d, 0.2)];
        d.transfer_detail(&mut px, blue_at(&d, 0.4));
        assert!(px[0] < r && px[1] < g);
        assert_eq!(px[2], blue_at(&d, 0.2));
    }

    #[test]
    fn detail_transfer_is_limited_and_fades_in_the_dark() {
        let d = detail_developer();
        // A huge jump is capped at the ratio limit.
        let mut px = [1000, 1000, blue_at(&d, 1.0)];
        d.transfer_detail(&mut px, blue_at(&d, 0.1));
        let gain = (px[0] as f64 - d.black[0]) / (1000.0 - d.black[0]);
        assert!(gain <= DETAIL_RATIO.1 + 1e-9, "{gain}");
        // Where blue is almost black the ratio is noise: the change is much smaller than in
        // the bright case for the same ratio.
        let dark = |f: f64| {
            let mut px = [1000, 1000, blue_at(&d, f * 2.0)];
            d.transfer_detail(&mut px, blue_at(&d, f));
            (px[0] as f64 - 1000.0).abs()
        };
        assert!(dark(0.002) < dark(0.3) / 4.0);
        // Blue at or below black: nothing to go by, nothing changes.
        let mut px = [1000, 1000, 50];
        d.transfer_detail(&mut px, 50);
        assert_eq!(px, [1000, 1000, 50]);
    }

    /// The sd Quattro developer applied to one raw pixel (black 100, white 4095), with or without
    /// the red-row correction.
    fn develop_pixel(px: [u16; 3], corrected: bool) -> [u16; 3] {
        // Sunlight white balance, and the exposure the tests were written for.
        let tuning = Tuning {
            white_balance: Some("Sunlight"),
            ..Tuning::default()
        };
        let mut d = Developer::new(&raw(), &with_camera(40.0), None, &tuning).unwrap();
        d.set_exposure_ev([0.0; 3]);
        if !corrected {
            d.red_row = None;
        }
        let mut row = px;
        d.apply(&mut row);
        row
    }

    /// Raw pixel at `level` in green and blue whose red the test's Merrill trim brings to
    /// the same level (black is 100).
    fn neutral(level: u16) -> [u16; 3] {
        [
            100 + ((level as f64 - 100.0) / MERRILL_RED_TRIM).round() as u16,
            level,
            level,
        ]
    }

    #[test]
    fn sd_quattro_red_row_keeps_neutrals_and_leaves_green_and_blue_alone() {
        let red_row_sum: f64 = SD_QUATTRO_RED_ROW.iter().sum();
        assert!((red_row_sum - 1.0).abs() < 1e-12);
        // A raw pixel that comes out neutral still does with the correction.
        // 生の値は整数なので、赤の補正を割り戻した値の丸めで 16bit の数レベルずれる。
        let plain = develop_pixel(neutral(2000), false);
        assert!(plain[0].abs_diff(plain[1]) <= 8, "{plain:?}");
        let gray = develop_pixel(neutral(2000), true);
        assert!(
            gray.iter().zip(plain).all(|(a, b)| a.abs_diff(b) <= 2),
            "{gray:?} vs {plain:?}"
        );
        let green = develop_pixel([800, 2600, 1000], true);
        let plain = develop_pixel([800, 2600, 1000], false);
        assert_eq!(green[1], plain[1]);
        assert_eq!(green[2], plain[2]);
    }

    #[test]
    fn sd_quattro_red_row_moves_greens_away_from_yellow() {
        // Yellow-green: red is high next to green. The correction takes red down.
        let green = develop_pixel([1800, 2600, 800], true);
        let plain = develop_pixel([1800, 2600, 800], false);
        assert!(green[0] < plain[0], "{green:?} vs {plain:?}");
        // A neutral pixel keeps its red, to rounding.
        let flat = develop_pixel(neutral(1500), true);
        let plain = develop_pixel(neutral(1500), false);
        assert!(flat[0].abs_diff(plain[0]) <= 2, "{flat:?} vs {plain:?}");
    }

    #[test]
    fn sd_quattro_auto_exposure_is_fixed_and_other_cameras_are_not() {
        let sd = Developer::new(&raw(), &with_camera(40.0), None, &Tuning::default()).unwrap();
        assert_eq!(sd.fixed_exposure(), Some(SD_QUATTRO_EXPOSURE_EV));
        for camera in [41.0, 42.0, 100.0] {
            let d = Developer::new(&raw(), &with_camera(camera), None, &Tuning::default()).unwrap();
            assert_eq!(d.fixed_exposure(), None, "camera {camera}");
        }
        let none = Developer::new(&raw(), &camf(1.0), None, &Tuning::default()).unwrap();
        assert_eq!(none.fixed_exposure(), None);
    }

    #[test]
    fn dp1_to_dp3_quattro_use_the_dp0_quattro_exposure_and_curve() {
        let dp0 = Developer::new(&raw(), &with_camera(83.0), None, &Tuning::default()).unwrap();
        for camera in [80.0, 81.0, 82.0] {
            let d = Developer::new(&raw(), &with_camera(camera), None, &Tuning::default()).unwrap();
            assert_eq!(
                d.fixed_exposure(),
                Some(DP0_QUATTRO_EXPOSURE_EV),
                "{camera}"
            );
            assert_eq!(d.curve_max, dp0.curve_max, "{camera}");
            assert_eq!(d.curve, dp0.curve, "{camera}");
        }
        // 隣の ID(sd Quattro H など)には広がらない。
        for camera in [79.0, 84.0, 41.0] {
            let d = Developer::new(&raw(), &with_camera(camera), None, &Tuning::default()).unwrap();
            assert_eq!(d.fixed_exposure(), None, "{camera}");
        }
    }

    #[test]
    fn dp0_quattro_auto_exposure_is_fixed_at_zero_ev() {
        let d = Developer::new(&raw(), &with_camera(83.0), None, &Tuning::default()).unwrap();
        assert_eq!(d.fixed_exposure(), Some(DP0_QUATTRO_EXPOSURE_EV));
        assert_eq!(d.curve_max, 2.0);
    }

    fn developed_in(space: ColorSpace, camera: f64, px: [u16; 3]) -> [u16; 3] {
        let tuning = Tuning {
            color_space: space,
            ..Tuning::default()
        };
        let d = Developer::new(&raw(), &with_camera(camera), None, &tuning).unwrap();
        assert_eq!(d.color_space(), space);
        let mut row = px;
        d.apply(&mut row);
        row
    }

    /// 2x3 の補正表を持つ Merrill の CAMF(絞り 2.8 と 8 の 2 段)。
    fn shaded_camf(aperture: f64) -> Camf {
        let table = |name: &str, values: &[f64]| Entry {
            name: name.into(),
            value: Value::Matrix {
                dims: vec![2, 3],
                values: values.to_vec(),
            },
        };
        let mut c = with_camera(77.0);
        c.0.push(m("SpatialGain_Fstop", &[2.8, 8.0]));
        c.0.push(m("CaptureAperture", &[aperture]));
        c.0.push(table(
            "SpatialGainG_0_INF",
            &[90.0, 0.0, 90.0, 90.0, 0.0, 90.0],
        ));
        c.0.push(table(
            "SpatialGainB_0_INF",
            &[180.0, 0.0, 180.0, 180.0, 0.0, 180.0],
        ));
        c.0.push(table("SpatialGainG_1_INF", &[0.0; 6]));
        c.0.push(table("SpatialGainB_1_INF", &[0.0; 6]));
        c
    }

    fn big_raw() -> Raw {
        Raw::Merrill {
            width: 9,
            height: 5,
            rgb: vec![100u16; 9 * 5 * 3],
        }
    }

    #[test]
    fn shading_gain_is_one_in_the_centre_and_follows_the_tables_at_the_edges() {
        let s = Shading::new(&big_raw(), &shaded_camf(2.8)).unwrap();
        let line = s.line(2);
        let centre = s.at(&line, 4);
        assert!(centre.iter().all(|g| (g - 1.0).abs() < 1e-6), "{centre:?}");
        let edge = s.at(&line, 0);
        assert!((edge[1] - (1.0 + 90.0 / SHADING_SCALE)).abs() < 1e-6);
        assert!((edge[2] - (1.0 + 180.0 / SHADING_SCALE)).abs() < 1e-6);
        // 赤は青の表を使う。
        assert!((edge[0] - (1.0 + 180.0 / SHADING_RED_SCALE)).abs() < 1e-6);
        // 格子の間は直線で補間し、範囲の外の座標は端に丸める。
        let between = s.at(&line, 2);
        assert!((between[1] - (1.0 + 45.0 / SHADING_SCALE)).abs() < 1e-6);
        assert_eq!(s.at(&line, 8), s.at(&line, 1000));
        assert_eq!(s.at(&s.line(4), 0), s.at(&s.line(99), 0));
    }

    #[test]
    fn shading_tables_are_blended_between_the_apertures() {
        let gain = |aperture: f64| {
            let s = Shading::new(&big_raw(), &shaded_camf(aperture)).unwrap();
            s.at(&s.line(0), 0)[1]
        };
        let (open, closed) = (1.0 + 90.0 / SHADING_SCALE, 1.0);
        assert!((gain(2.8) - open).abs() < 1e-6);
        assert!((gain(8.0) - closed).abs() < 1e-6);
        // 範囲の外は端の表。
        assert!((gain(1.4) - open).abs() < 1e-6);
        assert!((gain(16.0) - closed).abs() < 1e-6);
        // 間は log2 で混ぜる(2.8 と 8 の幾何平均でちょうど半分)。
        let mid = gain((2.8f64 * 8.0).sqrt());
        assert!((mid - (open + closed) / 2.0).abs() < 1e-4, "{mid}");
    }

    #[test]
    fn shading_is_skipped_when_the_tables_are_missing_or_damaged() {
        // 表が無い機種、Quattro。
        assert!(Shading::new(&big_raw(), &with_camera(77.0)).is_none());
        let quattro = Raw::Quattro {
            low_width: 2,
            low_height: 2,
            low: vec![100; 12],
            top_width: 4,
            top_height: 4,
            top: vec![100; 16],
        };
        assert!(Shading::new(&quattro, &shaded_camf(2.8)).is_none());
        // 絞りが無い・0・非数。
        for bad in [0.0, -1.0, f64::NAN] {
            assert!(
                Shading::new(&big_raw(), &shaded_camf(bad)).is_none(),
                "{bad}"
            );
        }
        // 表の大きさが食い違う、値が範囲外、1 行しかない。
        let damage = |name: &str, dims: Vec<usize>, values: Vec<f64>| {
            let mut c = shaded_camf(2.8);
            let e = c.0.iter_mut().find(|e| e.name == name).unwrap();
            e.value = Value::Matrix { dims, values };
            c
        };
        for c in [
            damage("SpatialGainB_0_INF", vec![3, 2], vec![0.0; 6]),
            damage(
                "SpatialGainG_0_INF",
                vec![2, 3],
                vec![0.0, 0.0, f64::NAN, 0.0, 0.0, 0.0],
            ),
            damage("SpatialGainG_0_INF", vec![2, 3], vec![-5.0; 6]),
            damage("SpatialGainG_0_INF", vec![1, 6], vec![0.0; 6]),
            damage("SpatialGainG_0_INF", vec![2, 4], vec![0.0; 6]),
            damage("SpatialGain_Fstop", vec![2], vec![2.8, 0.0]),
        ] {
            assert!(Shading::new(&big_raw(), &c).is_none());
        }
        // 1 画素幅の画像では格子に割り当てられない。
        let thin = Raw::Merrill {
            width: 1,
            height: 5,
            rgb: vec![100; 15],
        };
        assert!(Shading::new(&thin, &shaded_camf(2.8)).is_none());
    }

    #[test]
    fn apply_at_brightens_the_edges_and_apply_ignores_the_position() {
        let d = Developer::new(&big_raw(), &shaded_camf(2.8), None, &Tuning::default()).unwrap();
        let px = [900u16, 900, 900];
        let mut plain = px;
        d.apply(&mut plain);
        let mut row: Vec<u16> = px.repeat(9);
        d.apply_at(&mut row, 2, |i| i);
        // 中央はゲイン 1 なので位置なしと同じ、端は明るい。
        assert_eq!(&row[4 * 3..5 * 3], &plain);
        assert!(row[1] > plain[1] && row[8 * 3 + 1] > plain[1], "{row:?}");
        // 表の無い機種では apply と同じ。
        let d = Developer::new(&big_raw(), &with_camera(77.0), None, &Tuning::default()).unwrap();
        let (mut a, mut b) = (px, px);
        d.apply(&mut a);
        d.apply_at(&mut b, 0, |_| 0);
        assert_eq!(a, b);
    }

    #[test]
    fn color_space_names_round_trip_and_unknown_names_are_rejected() {
        for space in [ColorSpace::Srgb, ColorSpace::AdobeRgb] {
            assert_eq!(ColorSpace::from_name(space.name()), Some(space));
        }
        assert_eq!(ColorSpace::from_name("ProPhoto"), None);
        assert_eq!(ColorSpace::from_name(""), None);
        assert_eq!(Tuning::default().color_space, ColorSpace::Srgb);
    }

    #[test]
    fn adobe_rgb_output_is_the_same_colour_as_the_srgb_output() {
        // sRGB の色域に収まる色は、どちらの色空間で書いても同じ色(リニア光)を指す。
        // 41(sd Quattro H)は測ったカーブも赤の補正も持たない機種。
        for px in [[400, 500, 600], [900, 700, 300], [1500, 1500, 1500]] {
            let s = developed_in(ColorSpace::Srgb, 41.0, px).map(|v| v as f64 / 65535.0);
            let a = developed_in(ColorSpace::AdobeRgb, 41.0, px).map(|v| v as f64 / 65535.0);
            let tone = |e: f64| e + TONE_STRENGTH * (e * e * (3.0 - 2.0 * e) - e);
            // カーブを通る前のリニア値に戻して比べる(カーブは出力色空間のチャンネルごと)。
            let untone = |y: f64| {
                let (mut lo, mut hi) = (0.0, 1.0);
                for _ in 0..50 {
                    let mid = (lo + hi) / 2.0;
                    if tone(mid) < y {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                srgb_decode((lo + hi) / 2.0)
            };
            let s_lin = s.map(untone);
            let a_lin = a.map(|e| untone(srgb_encode(e.powf(ADOBE_GAMMA))));
            let want = SRGB_TO_ADOBE.map(|m| m[0] * s_lin[0] + m[1] * s_lin[1] + m[2] * s_lin[2]);
            for c in 0..3 {
                assert!(
                    (a_lin[c] - want[c]).abs() < 2e-3,
                    "{px:?} {c}: {a_lin:?} {want:?}"
                );
            }
        }
    }

    #[test]
    fn adobe_rgb_keeps_grey_grey_and_white_white() {
        for camera in [40.0, 77.0, 78.0, 83.0] {
            let grey = developed_in(ColorSpace::AdobeRgb, camera, [1000, 1000, 1000]);
            let srgb = developed_in(ColorSpace::Srgb, camera, [1000, 1000, 1000]);
            // sRGB で灰色になる入力は Adobe RGB でも灰色(白色点が同じ)。機種の補正で色が付く入力は
            // 原色が広いぶん、エンコード値の開きが小さくなる。
            let spread = |p: [u16; 3]| p.iter().max().unwrap() - p.iter().min().unwrap();
            if spread(srgb) < 200 {
                assert!(spread(grey) < 300, "camera {camera}: {grey:?} {srgb:?}");
            } else {
                assert!(
                    spread(grey) < spread(srgb),
                    "camera {camera}: {grey:?} {srgb:?}"
                );
            }
            let white = developed_in(ColorSpace::AdobeRgb, camera, [u16::MAX; 3]);
            assert!(
                white.iter().all(|&v| v > 60000),
                "camera {camera}: {white:?}"
            );
        }
    }

    #[test]
    fn dp2_dp3_merrill_use_the_dp1_merrill_exposure_and_curve() {
        let dp1 = Developer::new(&raw(), &with_camera(77.0), None, &Tuning::default()).unwrap();
        let dp23 = Developer::new(&raw(), &with_camera(78.0), None, &Tuning::default()).unwrap();
        assert_eq!(dp23.fixed_exposure(), Some(DP1_MERRILL_EXPOSURE_EV));
        assert_eq!(dp23.curve_max, dp1.curve_max);
        assert_eq!(dp23.curve, dp1.curve);
    }

    #[test]
    fn dp1_merrill_auto_exposure_is_fixed_and_curve_follows_the_measured_table() {
        let d = Developer::new(&raw(), &with_camera(77.0), None, &Tuning::default()).unwrap();
        assert_eq!(d.fixed_exposure(), Some(DP1_MERRILL_EXPOSURE_EV));
        let m = &DP1_MERRILL_MEASURED;
        assert!((d.curve_max - m.max_linear()).abs() < 1e-12);
        for &(x, y) in m.nodes {
            assert!((m.curve(2f64.powf(x)) * 255.0 - y).abs() < 1e-6, "{x}");
        }
        // 白に届く点より上は白のまま、表は単調。
        assert_eq!(m.curve(m.max_linear()), 1.0);
        assert_eq!(m.curve(4.0), 1.0);
        assert!(m.nodes.windows(2).all(|w| w[0].1 < w[1].1));
        assert_eq!(m.curve(0.0), 0.0);
    }

    #[test]
    fn dp0_quattro_curve_follows_the_measured_table_up_to_twice_linear_white() {
        let m = &DP0_QUATTRO_MEASURED;
        for &(x, y) in m.nodes {
            let got = m.curve(2f64.powf(x)) * 255.0;
            assert!((got - y).abs() < 0.01, "log2 {x}: {got} vs {y}");
        }
        assert_eq!(m.curve(0.0), 0.0);
        // リニア 1.0 ではまだ白に届かず、2.0 で届く。その先は変わらない。
        assert!(m.curve(1.0) < 254.0 / 255.0);
        assert!((m.curve(2.0) - 1.0).abs() < 1e-9);
        assert_eq!(m.encode(5.0, TONE_STRENGTH), m.encode(2.0, TONE_STRENGTH));
        // 強さ 0 は素の sRGB。
        for l in [0.001, 0.18, 0.9] {
            assert!((m.encode(l, 0.0) - srgb_encode(l)).abs() < 1e-12);
        }
    }

    #[test]
    fn dp0_quattro_lookup_table_is_monotone_and_keeps_highlights_above_linear_white() {
        let d = Developer::new(&raw(), &with_camera(83.0), None, &Tuning::default()).unwrap();
        assert!(d.curve.windows(2).all(|w| w[1] >= w[0]));
        assert_eq!(d.curve[0], 0.0);
        // 緑(ゲイン 1)が飽和した画素: リニア 1.0 は白の少し手前に来る。
        let mut row = [100u16, 4095, 100];
        d.apply(&mut row);
        // 飽和画素は白へ寄せるので 3 チャンネルが揃い、かつ 16bit の上限には張り付かない。
        assert!(row[1] > 64000 && row[1] < 65535, "{row:?}");
        // 測ったカーブを持たない機種は、これまでどおりリニア 1.0 が上限。
        let other = Developer::new(&raw(), &with_camera(41.0), None, &Tuning::default()).unwrap();
        assert_eq!(other.curve_max, 1.0);
    }

    #[test]
    fn monotone_cubic_passes_through_its_nodes_and_never_overshoots() {
        let nodes = [(0.0, 0.0), (1.0, 1.0), (2.0, 1.1), (3.0, 5.0)];
        for &(x, y) in &nodes {
            assert!((monotone_cubic(&nodes, x) - y).abs() < 1e-12);
        }
        let mut last = f64::MIN;
        for i in 0..=300 {
            let v = monotone_cubic(&nodes, i as f64 / 100.0);
            assert!(v >= last - 1e-12, "not monotone at {i}");
            assert!((0.0..=5.0).contains(&v));
            last = v;
        }
    }

    #[test]
    fn sd_quattro_curve_follows_the_measured_table() {
        for &(x, y) in &SD_QUATTRO_TONE {
            let got = sd_quattro_curve(2f64.powf(x)) * 255.0;
            assert!((got - y).abs() < 0.01, "log2 {x}: {got} vs {y}");
        }
        assert_eq!(sd_quattro_curve(0.0), 0.0);
        assert!((sd_quattro_curve(1.0) * 255.0 - SD_QUATTRO_WHITE).abs() < 1e-9);
        // Black stays black, and the shadows are darker than plain sRGB's.
        let shadow = 2f64.powf(-5.0);
        assert!(sd_quattro_curve(shadow) < srgb_encode(shadow) * 0.6);
    }

    #[test]
    fn sd_quattro_tone_strength_runs_from_plain_srgb_through_photo_pros_curve() {
        for l in [0.001, 0.02, 0.18, 0.5, 0.9] {
            assert!((encode_sd_quattro(l, 0.0) - srgb_encode(l)).abs() < 1e-12);
            assert!((encode_sd_quattro(l, TONE_STRENGTH) - sd_quattro_curve(l)).abs() < 1e-12);
        }
        // The finished lookup table never goes backwards, even at the strongest setting.
        let tuning = Tuning {
            tone: 1.0,
            ..Tuning::default()
        };
        let d = Developer::new(&raw(), &with_camera(40.0), None, &tuning).unwrap();
        assert!(d.curve.windows(2).all(|w| w[1] >= w[0]));
        assert_eq!(d.curve[0], 0.0);
    }
}
