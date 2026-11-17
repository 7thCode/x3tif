//! 検証用 CLI: 2 つの 16bit RGB TIFF を画素単位で比べる。
//!
//!   tiffcmp <reference.tif> <candidate.tif> [--grid N] [--bands]
//!
//! `--bands` は位置ずれ(高域の相互相関)と、輝度・色差の帯域別の振幅・相関・ゲインも出す。
//!
//! reference は SIGMA Photo Pro の出力、candidate は x3tif の出力を想定している。
//! 画素値の差(一致率・最大誤差・RMSE・PSNR など)を主に報告し、TIFF タグの違いと、
//! reference の MakerNote に残っている Photo Pro の現像設定は別枠で一覧にする。
//! ICC プロファイルが違う場合(Adobe RGB と sRGB)は、candidate を reference の色空間へ
//! 測色的に変換してから比べる(その旨を出力に明記する)。
use std::{collections::BTreeMap, env, fs, process::ExitCode};

type Res<T> = Result<T, String>;

/// 読み込む画像の上限(サンプル数)。壊れたヘッダで巨大確保しないため。
const MAX_SAMPLES: usize = 1 << 29;

struct Tiff<'a> {
    b: &'a [u8],
    le: bool,
}

#[derive(Clone)]
struct Entry {
    ty: u16,
    count: usize,
    /// 値の置き場所(ファイル先頭からのオフセット)。
    at: usize,
}

fn type_size(ty: u16) -> usize {
    match ty {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 => 8,
        _ => 1,
    }
}

impl<'a> Tiff<'a> {
    fn new(b: &'a [u8]) -> Res<Tiff<'a>> {
        let le = match b.get(..4) {
            Some([b'I', b'I', 42, 0]) => true,
            Some([b'M', b'M', 0, 42]) => false,
            _ => return Err("TIFF ではありません".into()),
        };
        Ok(Tiff { b, le })
    }

    fn u16(&self, o: usize) -> Res<u16> {
        let s = o
            .checked_add(2)
            .and_then(|e| self.b.get(o..e))
            .ok_or("TIFF が途中で切れています")?;
        Ok(if self.le {
            u16::from_le_bytes([s[0], s[1]])
        } else {
            u16::from_be_bytes([s[0], s[1]])
        })
    }

    fn u32(&self, o: usize) -> Res<u32> {
        let s = o
            .checked_add(4)
            .and_then(|e| self.b.get(o..e))
            .ok_or("TIFF が途中で切れています")?;
        let a = [s[0], s[1], s[2], s[3]];
        Ok(if self.le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        })
    }

    /// `at` にある IFD のタグ一覧と、次の IFD のオフセット。
    fn ifd(&self, at: usize) -> Res<(BTreeMap<u16, Entry>, usize)> {
        let n = self.u16(at)? as usize;
        let mut out = BTreeMap::new();
        for i in 0..n {
            let e = at + 2 + i * 12;
            let ty = self.u16(e + 2)?;
            let count = self.u32(e + 4)? as usize;
            let size = count.saturating_mul(type_size(ty));
            let at = if size <= 4 {
                e + 8
            } else {
                self.u32(e + 8)? as usize
            };
            out.insert(self.u16(e)?, Entry { ty, count, at });
        }
        Ok((out, self.u32(at + 2 + n * 12)? as usize))
    }

    fn bytes(&self, e: &Entry) -> Res<&'a [u8]> {
        let len = e.count.saturating_mul(type_size(e.ty));
        e.at.checked_add(len)
            .and_then(|end| self.b.get(e.at..end))
            .ok_or_else(|| "タグの値がファイルの外を指しています".into())
    }

    /// SHORT / LONG の値を数値の並びとして読む。
    fn ints(&self, e: &Entry) -> Res<Vec<usize>> {
        self.bytes(e)?;
        (0..e.count)
            .map(|i| match e.ty {
                3 => self.u16(e.at + i * 2).map(usize::from),
                4 => self.u32(e.at + i * 4).map(|v| v as usize),
                _ => Err("数値でないタグです".into()),
            })
            .collect()
    }

    /// 人が読める形のタグ値(長いものは先頭だけ)。
    fn show(&self, e: &Entry) -> String {
        let Ok(raw) = self.bytes(e) else {
            return "(範囲外)".into();
        };
        let rat = |i: usize, signed: bool| -> String {
            let (n, d) = (
                self.u32(e.at + i * 8).unwrap_or(0),
                self.u32(e.at + i * 8 + 4).unwrap_or(0),
            );
            if signed {
                format!("{}/{}", n as i32, d as i32)
            } else {
                format!("{n}/{d}")
            }
        };
        let list = |f: &dyn Fn(usize) -> String| -> String {
            let shown: Vec<String> = (0..e.count.min(8)).map(f).collect();
            let more = if e.count > 8 { " …" } else { "" };
            format!("{}{more}", shown.join(" "))
        };
        match e.ty {
            2 => {
                let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
                format!("\"{}\"", String::from_utf8_lossy(&raw[..end]).trim_end())
            }
            3 => list(&|i| self.u16(e.at + i * 2).map_or("?".into(), |v| v.to_string())),
            4 => list(&|i| self.u32(e.at + i * 4).map_or("?".into(), |v| v.to_string())),
            5 => list(&|i| rat(i, false)),
            10 => list(&|i| rat(i, true)),
            _ => format!("({} バイト)", raw.len()),
        }
    }
}

struct Image {
    width: usize,
    height: usize,
    /// R, G, B の順に並んだ 16bit サンプル。
    px: Vec<u16>,
    ifd0: BTreeMap<u16, Entry>,
    exif: BTreeMap<u16, Entry>,
    /// ICC プロファイルから読み取った色空間名("sRGB" / "Adobe RGB (1998)" / なし)。
    color_space: Option<&'static str>,
}

fn find(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn load<'a>(t: &Tiff<'a>) -> Res<Image> {
    let (ifd0, _) = t.ifd(t.u32(4)? as usize)?;
    let one = |tag: u16| -> Res<usize> {
        let e = ifd0.get(&tag).ok_or(format!("タグ {tag} がありません"))?;
        t.ints(e)?
            .first()
            .copied()
            .ok_or(format!("タグ {tag} が空です"))
    };
    let (width, height) = (one(256)?, one(257)?);
    let bits = t.ints(ifd0.get(&258).ok_or("BitsPerSample がありません")?)?;
    let samples = one(277)?;
    let compression = ifd0.get(&259).map_or(Ok(1), |_| one(259))?;
    let planar = ifd0.get(&284).map_or(Ok(1), |_| one(284))?;
    let predictor = ifd0.get(&317).map_or(Ok(1), |_| one(317))?;
    if bits != [16, 16, 16] || samples != 3 || planar != 1 {
        return Err(format!(
            "16bit RGB(chunky)のみ対応です(bits {bits:?}, samples {samples}, planar {planar})"
        ));
    }
    if !matches!(compression, 1 | 8 | 32946) || !matches!(predictor, 1 | 2) {
        return Err(format!(
            "未対応の圧縮です(compression {compression}, predictor {predictor})"
        ));
    }
    let total = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(3))
        .filter(|&n| n > 0 && n <= MAX_SAMPLES)
        .ok_or("画像サイズが不正です")?;
    let offsets = t.ints(ifd0.get(&273).ok_or("StripOffsets がありません")?)?;
    let counts = t.ints(ifd0.get(&279).ok_or("StripByteCounts がありません")?)?;
    if offsets.len() != counts.len() {
        return Err("ストリップの数が合いません".into());
    }
    let mut bytes: Vec<u8> = Vec::with_capacity(total * 2);
    for (&o, &c) in offsets.iter().zip(&counts) {
        let strip = o
            .checked_add(c)
            .and_then(|e| t.b.get(o..e))
            .ok_or("ストリップがファイルの外を指しています")?;
        if compression == 1 {
            bytes.extend_from_slice(strip);
        } else {
            let limit = total * 2 - bytes.len().min(total * 2);
            let raw = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(strip, limit)
                .map_err(|_| "Deflate の展開に失敗しました")?;
            bytes.extend_from_slice(&raw);
        }
    }
    if bytes.len() < total * 2 {
        return Err("画素データが足りません".into());
    }
    let mut px: Vec<u16> = bytes[..total * 2]
        .chunks_exact(2)
        .map(|c| {
            if t.le {
                u16::from_le_bytes([c[0], c[1]])
            } else {
                u16::from_be_bytes([c[0], c[1]])
            }
        })
        .collect();
    if predictor == 2 {
        for row in px.chunks_exact_mut(width * 3) {
            for i in 3..row.len() {
                row[i] = row[i].wrapping_add(row[i - 3]);
            }
        }
    }
    let exif = match ifd0.get(&34665) {
        Some(e) => t.ifd(t.u32(e.at)? as usize)?.0,
        None => BTreeMap::new(),
    };
    let color_space = ifd0.get(&34675).and_then(|e| {
        let icc = t.bytes(e).ok()?;
        if find(icc, b"Adobe RGB (1998)") {
            Some("Adobe RGB (1998)")
        } else if find(icc, b"sRGB") {
            Some("sRGB")
        } else {
            None
        }
    });
    Ok(Image {
        width,
        height,
        px,
        ifd0,
        exif,
        color_space,
    })
}

/// 正立後の座標から元の座標への対応。
type Mapping = Box<dyn Fn(usize, usize) -> (usize, usize)>;

/// Orientation タグ(3 / 6 / 8)に従って画素を正立させる。Photo Pro は縦位置の写真を画素ごと
/// 回して書き出し、x3tif は横のままタグで向きを示すので、比べる前に揃える。
fn upright(img: &mut Image, orientation: usize) {
    let (w, h) = (img.width, img.height);
    // 正立後の (X, Y) に来る、元の (x, y)。
    let (nw, nh, from): (usize, usize, Mapping) = match orientation {
        3 => (w, h, Box::new(move |x, y| (w - 1 - x, h - 1 - y))),
        6 => (h, w, Box::new(move |x, y| (y, h - 1 - x))),
        8 => (h, w, Box::new(move |x, y| (w - 1 - y, x))),
        _ => return,
    };
    let mut px = vec![0u16; img.px.len()];
    for y in 0..nh {
        for x in 0..nw {
            let (sx, sy) = from(x, y);
            px[(y * nw + x) * 3..][..3].copy_from_slice(&img.px[(sy * w + sx) * 3..][..3]);
        }
    }
    img.px = px;
    img.width = nw;
    img.height = nh;
}

type M3 = [[f64; 3]; 3];

fn mul(a: &M3, b: &M3) -> M3 {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

fn invert(m: &M3) -> M3 {
    let c = |i: usize, j: usize| {
        let (a, b) = ((i + 1) % 3, (i + 2) % 3);
        let (p, q) = ((j + 1) % 3, (j + 2) % 3);
        m[a][p] * m[b][q] - m[a][q] * m[b][p]
    };
    let det: f64 = (0..3).map(|j| m[0][j] * c(0, j)).sum();
    std::array::from_fn(|i| std::array::from_fn(|j| c(j, i) / det))
}

/// 原色の色度と D65 白色点から、リニア RGB → XYZ の行列を作る。
fn rgb_to_xyz(primaries: [(f64, f64); 3]) -> M3 {
    let xyz = |(x, y): (f64, f64)| [x / y, 1.0, (1.0 - x - y) / y];
    let p = primaries.map(xyz);
    let m: M3 = std::array::from_fn(|i| std::array::from_fn(|j| p[j][i]));
    let w = xyz((0.3127, 0.3290));
    let inv = invert(&m);
    let s: [f64; 3] = std::array::from_fn(|i| (0..3).map(|k| inv[i][k] * w[k]).sum());
    std::array::from_fn(|i| std::array::from_fn(|j| m[i][j] * s[j]))
}

const SRGB_PRIMARIES: [(f64, f64); 3] = [(0.64, 0.33), (0.30, 0.60), (0.15, 0.06)];
const ADOBE_PRIMARIES: [(f64, f64); 3] = [(0.64, 0.33), (0.21, 0.71), (0.15, 0.06)];
const ADOBE_GAMMA: f64 = 2.199_218_75;

/// sRGB の画素値を Adobe RGB (1998) の画素値へ変換する(sRGB は Adobe RGB の色域に収まる)。
fn srgb_to_adobe(px: &mut [u16]) {
    let m = mul(
        &invert(&rgb_to_xyz(ADOBE_PRIMARIES)),
        &rgb_to_xyz(SRGB_PRIMARIES),
    );
    let decode: Vec<f64> = (0..=u16::MAX)
        .map(|v| {
            let e = v as f64 / 65535.0;
            if e <= 0.04045 {
                e / 12.92
            } else {
                ((e + 0.055) / 1.055).powf(2.4)
            }
        })
        .collect();
    for p in px.chunks_exact_mut(3) {
        let l = [0, 1, 2].map(|c| decode[p[c] as usize]);
        for c in 0..3 {
            let v = (m[c][0] * l[0] + m[c][1] * l[1] + m[c][2] * l[2]).clamp(0.0, 1.0);
            p[c] = (v.powf(1.0 / ADOBE_GAMMA) * 65535.0).round() as u16;
        }
    }
}

#[derive(Default, Clone)]
struct Stats {
    n: u64,
    exact: u64,
    within: [u64; 3],
    max: u32,
    sum: f64,
    sum_abs: f64,
    sum_sq: f64,
}

/// 「±1 以内」などの判定に使う幅(16bit の値)。257 が 8bit の 1 段にあたる。
const WITHIN: [u32; 3] = [1, 257, 4 * 257];

impl Stats {
    fn add(&mut self, reference: u16, candidate: u16) {
        let d = candidate as i32 - reference as i32;
        let a = d.unsigned_abs();
        self.n += 1;
        self.exact += (a == 0) as u64;
        for (w, &limit) in self.within.iter_mut().zip(&WITHIN) {
            *w += (a <= limit) as u64;
        }
        self.max = self.max.max(a);
        self.sum += d as f64;
        self.sum_abs += a as f64;
        self.sum_sq += (d as f64) * (d as f64);
    }

    fn merge(&mut self, o: &Stats) {
        self.n += o.n;
        self.exact += o.exact;
        for (a, b) in self.within.iter_mut().zip(o.within) {
            *a += b;
        }
        self.max = self.max.max(o.max);
        self.sum += o.sum;
        self.sum_abs += o.sum_abs;
        self.sum_sq += o.sum_sq;
    }

    fn rmse(&self) -> f64 {
        (self.sum_sq / self.n.max(1) as f64).sqrt()
    }

    fn line(&self, name: &str) -> String {
        let n = self.n.max(1) as f64;
        let pct = |v: u64| 100.0 * v as f64 / n;
        let rmse = self.rmse();
        let psnr = if rmse > 0.0 {
            format!("{:6.2} dB", 20.0 * (65535.0 / rmse).log10())
        } else {
            "    inf".into()
        };
        format!(
            "{name:<4} 完全一致 {:7.3}%  ±1 {:7.3}%  ±1/255 {:7.3}%  ±4/255 {:7.3}%  最大 {:5} ({:6.2}/255)  平均差 {:+8.1} ({:+6.2}/255)  MAE {:7.1}  RMSE {:7.1} ({:5.2}/255)  PSNR {psnr}",
            pct(self.exact),
            pct(self.within[0]),
            pct(self.within[1]),
            pct(self.within[2]),
            self.max,
            self.max as f64 / 257.0,
            self.sum / n,
            self.sum / n / 257.0,
            self.sum_abs / n,
            rmse,
            rmse / 257.0,
        )
    }
}

/// `block` 画素四方の平均(チャンネルごと)。端の半端なブロックは捨てる。
fn block_means(img: &Image, block: usize) -> (usize, usize, Vec<f64>) {
    let (bw, bh) = (img.width / block, img.height / block);
    let mut out = vec![0f64; bw * bh * 3];
    for by in 0..bh {
        for y in by * block..(by + 1) * block {
            let row = &img.px[y * img.width * 3..(y + 1) * img.width * 3];
            for bx in 0..bw {
                for p in row[bx * block * 3..(bx + 1) * block * 3].chunks_exact(3) {
                    for c in 0..3 {
                        out[(by * bw + bx) * 3 + c] += p[c] as f64;
                    }
                }
            }
        }
    }
    let n = (block * block) as f64;
    out.iter_mut().for_each(|v| *v /= n);
    (bw, bh, out)
}

fn tag_name(tag: u16) -> &'static str {
    match tag {
        254 => "NewSubfileType",
        256 => "ImageWidth",
        257 => "ImageLength",
        258 => "BitsPerSample",
        259 => "Compression",
        262 => "Photometric",
        270 => "ImageDescription",
        271 => "Make",
        272 => "Model",
        273 => "StripOffsets",
        274 => "Orientation",
        277 => "SamplesPerPixel",
        278 => "RowsPerStrip",
        279 => "StripByteCounts",
        282 => "XResolution",
        283 => "YResolution",
        284 => "PlanarConfig",
        296 => "ResolutionUnit",
        305 => "Software",
        306 => "DateTime",
        315 => "Artist",
        317 => "Predictor",
        700 => "XMP",
        33432 => "Copyright",
        34665 => "ExifIFD",
        34675 => "ICCProfile",
        33434 => "ExposureTime",
        33437 => "FNumber",
        34850 => "ExposureProgram",
        34855 => "ISO",
        36864 => "ExifVersion",
        36867 => "DateTimeOriginal",
        36868 => "DateTimeDigitized",
        37377 => "ShutterSpeedValue",
        37378 => "ApertureValue",
        37379 => "BrightnessValue",
        37380 => "ExposureBias",
        37381 => "MaxApertureValue",
        37382 => "SubjectDistance",
        37383 => "MeteringMode",
        37384 => "LightSource",
        37385 => "Flash",
        37386 => "FocalLength",
        37500 => "MakerNote",
        37510 => "UserComment",
        40961 => "ColorSpace",
        40962 => "PixelXDimension",
        40963 => "PixelYDimension",
        41985 => "CustomRendered",
        41986 => "ExposureMode",
        41987 => "WhiteBalance",
        41989 => "FocalLengthIn35mm",
        41992 => "Contrast",
        41993 => "Saturation",
        41994 => "Sharpness",
        42016 => "ImageUniqueID",
        42033 => "BodySerialNumber",
        42035 => "LensMake",
        42036 => "LensModel",
        42240 => "Gamma",
        _ => "",
    }
}

/// 画素の置き場所だけを表すタグ。圧縮方式が違えば当然違うので、差分の一覧からは外す。
const LAYOUT_TAGS: [u16; 6] = [259, 273, 278, 279, 317, 34665];

fn tag_diff(title: &str, a: (&Tiff, &BTreeMap<u16, Entry>), b: (&Tiff, &BTreeMap<u16, Entry>)) {
    println!("  [{title}]");
    let tags: std::collections::BTreeSet<u16> = a.1.keys().chain(b.1.keys()).copied().collect();
    for tag in tags {
        if LAYOUT_TAGS.contains(&tag) {
            continue;
        }
        let show = |(t, m): (&Tiff, &BTreeMap<u16, Entry>)| m.get(&tag).map(|e| t.show(e));
        let (va, vb) = (show(a), show(b));
        if va == vb {
            continue;
        }
        println!(
            "    {tag:5} {:<18} reference: {:<34} candidate: {}",
            tag_name(tag),
            va.unwrap_or_else(|| "(なし)".into()),
            vb.unwrap_or_else(|| "(なし)".into())
        );
    }
}

/// SIGMA の MakerNote に Photo Pro が残した現像設定。名前は ExifTool の Sigma タグ表によるもので、
/// 表に無いタグは番号のまま出す。
fn maker_note_settings(t: &Tiff, exif: &BTreeMap<u16, Entry>) {
    let Some(note) = exif.get(&37500) else {
        println!("  MakerNote がありません(現像設定は記録されていません)");
        return;
    };
    let ok = t.bytes(note).is_ok_and(|b| b.starts_with(b"SIGMA\0"));
    let Some((entries, _)) = ok.then(|| t.ifd(note.at + 10).ok()).flatten() else {
        println!("  SIGMA の MakerNote として読めません");
        return;
    };
    let name = |tag: u16| match tag {
        0x0007 => "WhiteBalance",
        0x000b => "ColorSpace",
        0x000c => "ExposureAdjust",
        0x000d => "Contrast",
        0x000e => "Shadow",
        0x000f => "Highlight",
        0x0010 => "Saturation",
        0x0011 => "Sharpness",
        0x0012 => "X3FillLight",
        0x0014 => "ColorAdjustment",
        0x0015 => "AdjustmentMode",
        0x0016 => "Quality",
        0x0017 => "Firmware",
        0x0018 => "Software",
        0x001a => "ChrominanceNoiseReduction",
        0x001b => "LuminanceNoiseReduction",
        0x003d => "ColorMode",
        _ => "",
    };
    for (tag, e) in &entries {
        // 撮影時の情報(シリアル、AF モードなど)は現像設定ではないので省く。
        if *tag < 0x0007 || matches!(tag, 0x0008..=0x000a) {
            continue;
        }
        println!("    0x{tag:04x} {:<26} {}", name(*tag), t.show(e));
    }
}

/// 横・縦の順にガウスぼかしをかける(端は繰り返す)。`sigma` が 0 以下なら複製を返す。
fn gauss_blur(src: &[f32], w: usize, h: usize, sigma: f64) -> Vec<f32> {
    if sigma <= 0.0 {
        return src.to_vec();
    }
    let r = (3.0 * sigma).ceil() as usize;
    let mut k: Vec<f32> = (0..=2 * r)
        .map(|i| {
            let d = i as f64 - r as f64;
            (-d * d / (2.0 * sigma * sigma)).exp() as f32
        })
        .collect();
    let sum: f32 = k.iter().sum();
    k.iter_mut().for_each(|v| *v /= sum);
    let mut tmp = vec![0f32; w * h];
    let mut padded = Vec::with_capacity(w + 2 * r);
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        padded.clear();
        padded.extend(std::iter::repeat_n(row[0], r));
        padded.extend_from_slice(row);
        padded.extend(std::iter::repeat_n(row[w - 1], r));
        let out = &mut tmp[y * w..(y + 1) * w];
        for (j, &kj) in k.iter().enumerate() {
            for (d, s) in out.iter_mut().zip(&padded[j..]) {
                *d += kj * s;
            }
        }
    }
    let mut dst = vec![0f32; w * h];
    for y in 0..h {
        let out = &mut dst[y * w..(y + 1) * w];
        for (j, &kj) in k.iter().enumerate() {
            let ys = (y as isize + j as isize - r as isize).clamp(0, h as isize - 1) as usize;
            for (d, s) in out.iter_mut().zip(&tmp[ys * w..(ys + 1) * w]) {
                *d += kj * s;
            }
        }
    }
    dst
}

/// 画像の 1 成分を /255 の単位で取り出す。0: 輝度 (R+G+B)/3、1: R−G、2: B−G、3/4/5: R/G/B。
fn component(img: &Image, which: usize) -> Vec<f32> {
    img.px
        .chunks_exact(3)
        .map(|p| {
            let (r, g, b) = (p[0] as f32, p[1] as f32, p[2] as f32);
            let v = match which {
                0 => (r + g + b) / 3.0,
                1 => r - g,
                2 => b - g,
                c => p[c - 3] as f32,
            };
            v / 257.0
        })
        .collect()
}

/// 位置ずれの測定。高域(元 − σ2.8 のぼかし)の正規化相互相関で、画像内 3x3 か所・R/G/B/輝度ごとに
/// candidate の像が reference よりどれだけ右下へずれているかを ±3 画素・サブピクセルで求める。
fn shift_report(reference: &Image, cand: &Image) {
    const T: usize = 320;
    const M: isize = 3;
    let (w, h) = (reference.width, reference.height);
    if w < T + 2 * M as usize || h < T + 2 * M as usize {
        println!("\n画像が小さいので位置ずれは測りません");
        return;
    }
    let highpass = |img: &Image, which: usize| {
        let p = component(img, which);
        let b = gauss_blur(&p, w, h, 2.8);
        p.iter().zip(&b).map(|(a, b)| a - b).collect::<Vec<f32>>()
    };
    let names = ["輝度", "R", "G", "B"];
    let which = [0usize, 3, 4, 5];
    let rp: Vec<Vec<f32>> = which.iter().map(|&c| highpass(reference, c)).collect();
    let cp: Vec<Vec<f32>> = which.iter().map(|&c| highpass(cand, c)).collect();
    println!("\n== 位置ずれ(candidate − reference、画素。+x は右、+y は下。高域の相互相関 ±3 画素、括弧内は相関のピーク) ==");
    for (fy, fx, place) in [
        (0.15, 0.15, "左上"),
        (0.15, 0.5, "上"),
        (0.15, 0.85, "右上"),
        (0.5, 0.15, "左"),
        (0.5, 0.5, "中央"),
        (0.5, 0.85, "右"),
        (0.85, 0.15, "左下"),
        (0.85, 0.5, "下"),
        (0.85, 0.85, "右下"),
    ] {
        let x0 = ((w as f64 * fx) as usize).clamp(T / 2 + 3, w - T / 2 - 3) - T / 2;
        let y0 = ((h as f64 * fy) as usize).clamp(T / 2 + 3, h - T / 2 - 3) - T / 2;
        let mut cells = Vec::new();
        for (name, (a, b)) in names.iter().zip(rp.iter().zip(&cp)) {
            // 相関 c[dy][dx](dx, dy は -3..=3)。
            let mut c = [[0f64; 7]; 7];
            let (mut aa, mut bb) = (0f64, 0f64);
            for y in y0..y0 + T {
                for x in x0..x0 + T {
                    aa += (a[y * w + x] as f64).powi(2);
                    bb += (b[y * w + x] as f64).powi(2);
                }
            }
            for (iy, row) in c.iter_mut().enumerate() {
                for (ix, v) in row.iter_mut().enumerate() {
                    let (dx, dy) = (ix as isize - M, iy as isize - M);
                    let mut s = 0f64;
                    for y in y0 + M as usize..y0 + T - M as usize {
                        let by = (y as isize + dy) as usize;
                        for x in x0 + M as usize..x0 + T - M as usize {
                            s +=
                                a[y * w + x] as f64 * b[by * w + (x as isize + dx) as usize] as f64;
                        }
                    }
                    *v = s / (aa * bb).sqrt().max(1e-12);
                }
            }
            let (mut best, mut bx, mut by) = (f64::MIN, 0usize, 0usize);
            for (iy, row) in c.iter().enumerate() {
                for (ix, &v) in row.iter().enumerate() {
                    if v > best {
                        (best, bx, by) = (v, ix, iy);
                    }
                }
            }
            let sub = |m: f64, z: f64, p: f64| {
                let d = m - 2.0 * z + p;
                if d.abs() < 1e-12 {
                    0.0
                } else {
                    (m - p) / (2.0 * d)
                }
            };
            let fxs = if (1..6).contains(&bx) {
                sub(c[by][bx - 1], c[by][bx], c[by][bx + 1])
            } else {
                0.0
            };
            let fys = if (1..6).contains(&by) {
                sub(c[by - 1][bx], c[by][bx], c[by + 1][bx])
            } else {
                0.0
            };
            cells.push(format!(
                "{name} ({:+.2},{:+.2}) [{:.2}]",
                bx as f64 - M as f64 + fxs,
                by as f64 - M as f64 + fys,
                best
            ));
        }
        println!("  {place:<4} {}", cells.join("  "));
    }
}

/// 帯域解析。輝度・R−G・B−G を差分ガウスの 4 帯域(σ0.7 / 1.4 / 2.8 / 5.6 の差)に分け、
/// reference の輝度(σ5.6 のぼかし)で 3 つの明るさに分けて、振幅・相関・最小二乗ゲインを出す。
/// ゲインは「reference ≈ ゲイン × candidate」の係数で、1 でなければその帯域の強さが違う。
/// 「ゲイン後」は帯域・明るさごとにゲインを掛けたあとの残差で、線形の調整で届く下限を表す。
fn band_report(reference: &Image, cand: &Image) {
    const SIGMAS: [f64; 4] = [0.7, 1.4, 2.8, 5.6];
    const EDGES: [f32; 2] = [48.0, 128.0];
    let (w, h) = (reference.width, reference.height);
    #[derive(Default, Clone, Copy)]
    struct S {
        n: f64,
        rr: f64,
        cc: f64,
        rc: f64,
    }
    let mut class: Vec<u8> = Vec::new();
    println!("\n== 帯域別の比較(/255。帯域は差分ガウス、明るさは reference の輝度で 暗 <48 / 中 <128 / 明) ==");
    for (which, name) in [(0usize, "輝度"), (1, "R−G"), (2, "B−G")] {
        let blurs = |img: &Image| {
            let p = component(img, which);
            let mut v = vec![p.clone()];
            for s in SIGMAS {
                v.push(gauss_blur(&p, w, h, s));
            }
            v
        };
        let (rl, cl) = (blurs(reference), blurs(cand));
        if which == 0 {
            // 輝度のとき、σ5.6 のぼかしを reference の明るさとして保存する。
            class = rl[4]
                .iter()
                .map(|&v| EDGES.iter().filter(|&&e| v >= e).count() as u8)
                .collect();
        }
        // stats[明るさ][帯域 0..4 が高→低の 4 帯域、4 が低周波の残り]。
        let mut stats = [[S::default(); 5]; 3];
        for i in 0..w * h {
            let k = class[i] as usize;
            for b in 0..5 {
                let (r, c) = if b < 4 {
                    (rl[b][i] - rl[b + 1][i], cl[b][i] - cl[b + 1][i])
                } else {
                    (rl[4][i], cl[4][i])
                };
                let s = &mut stats[k][b];
                s.n += 1.0;
                s.rr += (r as f64).powi(2);
                s.cc += (c as f64).powi(2);
                s.rc += r as f64 * c as f64;
            }
        }
        println!("\n[{name}]  帯域(σ)  明るさ    割合  ref の RMS  cand の RMS   相関   ゲイン");
        let total: f64 = stats.iter().map(|s| s[0].n).sum();
        let (mut actual, mut linear) = (0f64, 0f64);
        for b in 0..5 {
            let label = match b {
                0 => "0〜0.7 ".to_string(),
                4 => "5.6〜 ".to_string(),
                _ => format!("{}〜{}", SIGMAS[b - 1], SIGMAS[b]),
            };
            for (k, kn) in ["暗", "中", "明"].iter().enumerate() {
                let s = stats[k][b];
                if s.n < 1.0 {
                    continue;
                }
                actual += s.rr + s.cc - 2.0 * s.rc;
                // 低周波(b == 4)にはゲインを掛けず、現状の差のまま足す。
                linear += if b < 4 {
                    (s.rr - s.rc * s.rc / s.cc.max(1e-12)).max(0.0)
                } else {
                    s.rr + s.cc - 2.0 * s.rc
                };
                if b < 4 {
                    println!(
                        "         {label:<8} {kn}   {:5.1}%  {:9.2}  {:11.2}   {:.3}  {:6.3}",
                        100.0 * s.n / total,
                        (s.rr / s.n).sqrt(),
                        (s.cc / s.n).sqrt(),
                        s.rc / (s.rr * s.cc).sqrt().max(1e-12),
                        s.rc / s.cc.max(1e-12)
                    );
                }
            }
        }
        println!(
            "         RMSE(帯域の合計): 現状 {:.2} / 帯域・明るさごとのゲイン後 {:.2}(低周波 5.6〜 は現状のまま)",
            (actual / total).sqrt(),
            (linear / total).sqrt()
        );
    }
}

fn run(args: &[String]) -> Res<()> {
    let mut paths = Vec::new();
    let mut grid = 8usize;
    let mut bands = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--bands" {
            bands = true;
        } else if a == "--grid" {
            grid = it
                .next()
                .and_then(|v| v.parse().ok())
                .filter(|g| (1..=64).contains(g))
                .ok_or("--grid には 1〜64 を指定します")?;
        } else {
            paths.push(a);
        }
    }
    let [ref_path, cand_path] = paths[..] else {
        return Err("usage: tiffcmp <reference.tif> <candidate.tif> [--grid N] [--bands]".into());
    };
    let read = |p: &String| fs::read(p).map_err(|e| format!("{p}: {e}"));
    let (ref_bytes, cand_bytes) = (read(ref_path)?, read(cand_path)?);
    let (rt, ct) = (Tiff::new(&ref_bytes)?, Tiff::new(&cand_bytes)?);
    let reference = load(&rt).map_err(|e| format!("{ref_path}: {e}"))?;
    let mut cand = load(&ct).map_err(|e| format!("{cand_path}: {e}"))?;

    println!("reference: {ref_path}");
    println!("candidate: {cand_path}");
    println!(
        "サイズ: reference {}x{} / candidate {}x{}",
        reference.width, reference.height, cand.width, cand.height
    );
    println!(
        "色空間(ICC): reference {} / candidate {}",
        reference.color_space.unwrap_or("不明"),
        cand.color_space.unwrap_or("不明")
    );

    println!("\n== Photo Pro の現像設定(reference の MakerNote) ==");
    maker_note_settings(&rt, &reference.exif);

    println!("\n== メタデータの違い(画素とは別枠) ==");
    tag_diff("IFD0", (&rt, &reference.ifd0), (&ct, &cand.ifd0));
    tag_diff("EXIF", (&rt, &reference.exif), (&ct, &cand.exif));

    let orientation = |t: &Tiff, img: &Image| {
        img.ifd0
            .get(&274)
            .and_then(|e| t.ints(e).ok())
            .and_then(|v| v.first().copied())
            .unwrap_or(1)
    };
    let (ro, co) = (orientation(&rt, &reference), orientation(&ct, &cand));
    if ro != 1 || co != 1 {
        println!(
            "\n注意: Orientation タグ(reference {ro} / candidate {co})に従って画素を正立させてから比べます"
        );
    }
    let mut reference = reference;
    upright(&mut reference, ro);
    upright(&mut cand, co);
    if (reference.width, reference.height) != (cand.width, cand.height) {
        return Err("画像サイズが違うので画素は比べられません".into());
    }
    match (reference.color_space, cand.color_space) {
        (a, b) if a == b && a.is_some() => {}
        (Some("Adobe RGB (1998)"), Some("sRGB")) => {
            println!("\n注意: candidate (sRGB) を Adobe RGB (1998) へ変換してから比べます");
            srgb_to_adobe(&mut cand.px);
        }
        _ => println!("\n注意: 色空間が揃っていません。画素値をそのまま比べます"),
    }

    // 画素単位の統計。
    let mut chans: [Stats; 3] = Default::default();
    let (mut rg, mut bg) = (0f64, 0f64);
    let (tw, th) = (
        reference.width.div_ceil(grid),
        reference.height.div_ceil(grid),
    );
    let mut tiles = vec![Stats::default(); grid * grid];
    // reference の明るさ(3 チャンネルの平均)で 8 段に分けた、チャンネルごとの平均差。
    let mut levels = vec![[(0f64, 0u64); 3]; 8];
    for (i, (r, c)) in reference
        .px
        .chunks_exact(3)
        .zip(cand.px.chunks_exact(3))
        .enumerate()
    {
        let (x, y) = (i % reference.width, i / reference.width);
        let tile = &mut tiles[(y / th) * grid + x / tw];
        let level = (r.iter().map(|&v| v as usize).sum::<usize>() / 3) >> 13;
        for ch in 0..3 {
            chans[ch].add(r[ch], c[ch]);
            tile.add(r[ch], c[ch]);
            let slot = &mut levels[level][ch];
            slot.0 += c[ch] as f64 - r[ch] as f64;
            slot.1 += 1;
        }
        rg += (c[0] as f64 - c[1] as f64) - (r[0] as f64 - r[1] as f64);
        bg += (c[2] as f64 - c[1] as f64) - (r[2] as f64 - r[1] as f64);
    }
    let mut all = Stats::default();
    chans.iter().for_each(|s| all.merge(s));
    let pixels = (reference.width * reference.height) as f64;

    println!("\n== 画素の差(candidate − reference、16bit 値) ==");
    for (s, name) in chans.iter().zip(["R", "G", "B"]) {
        println!("{}", s.line(name));
    }
    println!("{}", all.line("全体"));
    println!(
        "色ずれ: R−G {:+.2}/255  B−G {:+.2}/255",
        rg / pixels / 257.0,
        bg / pixels / 257.0
    );

    // 16 画素四方の平均どうしの差 = 色・トーン・露出など低周波の差。残りが細部・ノイズ・鮮鋭化の差。
    const BLOCK: usize = 16;
    let (_, _, ra) = block_means(&reference, BLOCK);
    let (_, _, ca) = block_means(&cand, BLOCK);
    let low_sq: f64 = ra
        .iter()
        .zip(&ca)
        .map(|(a, b)| (b - a) * (b - a))
        .sum::<f64>()
        / ra.len().max(1) as f64;
    let total_sq = all.sum_sq / all.n.max(1) as f64;
    println!(
        "RMSE の内訳: 低周波(16px 平均。色・トーン・露出) {:.2}/255 / 高周波(細部・ノイズ・鮮鋭化) {:.2}/255",
        low_sq.sqrt() / 257.0,
        (total_sq - low_sq).max(0.0).sqrt() / 257.0
    );

    println!("\n== 明るさ別の平均差(/255。reference の明るさで 8 段) ==");
    println!("  明るさ      画素の割合      R       G       B");
    for (i, l) in levels.iter().enumerate() {
        if l[0].1 == 0 {
            continue;
        }
        let m = |c: usize| l[c].0 / l[c].1 as f64 / 257.0;
        println!(
            "  {:3}-{:3}   {:9.2}%   {:+7.2} {:+7.2} {:+7.2}",
            i * 32,
            i * 32 + 31,
            100.0 * l[0].1 as f64 / pixels,
            m(0),
            m(1),
            m(2)
        );
    }

    println!("\n== 場所別の RMSE(/255、{grid}x{grid} 分割。上が画像の上端) ==");
    for row in tiles.chunks(grid) {
        let cells: Vec<String> = row
            .iter()
            .map(|s| format!("{:5.1}", s.rmse() / 257.0))
            .collect();
        println!("  {}", cells.join(" "));
    }
    if bands {
        shift_report(&reference, &cand);
        band_report(&reference, &cand);
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
