//! Minimal X3F reader for dp Merrill, dp Quattro and sd Quattro (TRUE-compressed RAW).

use crate::camf::{self, Camf};
use std::fmt;

const FOVB: u32 = 0x6256_4f46;
const SECP: u32 = 0x7043_4553;
const SECI: u32 = 0x6943_4553;
const SECC: u32 = 0x6343_4553;

const IMAGE_HEADER_SIZE: usize = 28;
const CAMF_HEADER_SIZE: usize = 28;
const PROPERTY_HEADER_SIZE: usize = 24;
const TRUE_PLANES: usize = 3;

/// 入力ファイルの大きさの上限(1 GiB)。実ファイルは最大でも 150 MB ほど。読み込む前に確かめる用。
pub const MAX_FILE_BYTES: u64 = 1 << 30;
/// PROP セクション 1 つあたりの件数と、1 つの文字列の長さ(UTF-16 単位)の上限。
const MAX_PROPERTIES: usize = 4096;
const MAX_PROPERTY_CHARS: usize = 4096;
/// 1 つの差分値に使えるビット数の上限。14bit のセンサー値には 15 あれば足りる。
const MAX_DIFF_BITS: u8 = 24;

const TYPE_PREVIEW: u32 = 2;
const FORMAT_MERRILL: u32 = 0x0001_001e;
const FORMAT_QUATTRO: u32 = 0x0001_0023;
const FORMAT_SDQ: u32 = 0x0001_0025;
const FORMAT_SDQH: u32 = 0x0001_0027;

#[derive(Debug)]
pub enum Error {
    NotX3f,
    Truncated,
    NoRawImage,
    Unsupported(u32),
    Corrupt(&'static str),
    /// Calibration data needed for the developed output is missing from the file.
    MissingCalibration(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotX3f => write!(f, "X3F ファイルではありません"),
            Error::Truncated => write!(f, "ファイルが途中で切れています"),
            Error::NoRawImage => write!(f, "RAW 画像が見つかりません"),
            Error::Unsupported(t) => write!(
                f,
                "未対応の機種/形式です (image type 0x{t:08x})。対応: dp Merrill / dp Quattro / sd Quattro"
            ),
            Error::Corrupt(m) => write!(f, "データが壊れています: {m}"),
            Error::MissingCalibration(m) => write!(
                f,
                "現像に必要な校正値がありません ({m})。「生データ」なら変換できます"
            ),
        }
    }
}

impl std::error::Error for Error {}

type Result<T> = std::result::Result<T, Error>;

/// Little-endian cursor over a byte slice.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn at(buf: &'a [u8], pos: usize) -> Self {
        Reader { buf, pos }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(Error::Truncated)?;
        let s = self.buf.get(self.pos..end).ok_or(Error::Truncated)?;
        self.pos = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

struct Entry {
    offset: usize,
    size: usize,
}

pub struct X3f {
    data: Vec<u8>,
    entries: Vec<Entry>,
}

/// Decoded sensor data, 16 bit per sample, no processing applied.
pub enum Raw {
    /// dp Merrill: three full-resolution layers, interleaved (R, G, B order of the layer index).
    Merrill {
        width: usize,
        height: usize,
        rgb: Vec<u16>,
    },
    /// Quattro: two quarter-size layers (interleaved in `low`, channel 2 unused) plus a
    /// full-resolution top layer.
    Quattro {
        low_width: usize,
        low_height: usize,
        low: Vec<u16>,
        top_width: usize,
        top_height: usize,
        top: Vec<u16>,
    },
}

pub struct Properties(pub Vec<(String, String)>);

impl Properties {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

struct ImageHeader {
    format: u32,
    columns: usize,
    rows: usize,
    /// Absolute offset of the first byte after the 28 byte image header.
    body: usize,
    end: usize,
}

impl X3f {
    pub fn parse(data: Vec<u8>) -> Result<X3f> {
        let mut r = Reader::at(&data, 0);
        if r.u32()? != FOVB {
            return Err(Error::NotX3f);
        }
        let tail = data.len().checked_sub(4).ok_or(Error::Truncated)?;
        let dir_pos = Reader::at(&data, tail).u32()? as usize;
        let mut r = Reader::at(&data, dir_pos);
        r.u32()?; // "SECd"
        r.u32()?; // version
        let count = r.u32()? as usize;
        let mut entries = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            let offset = r.u32()? as usize;
            let size = r.u32()? as usize;
            r.u32()?; // type
            if offset.checked_add(size).is_none_or(|e| e > data.len()) {
                return Err(Error::Truncated);
            }
            entries.push(Entry { offset, size });
        }
        Ok(X3f { data, entries })
    }

    fn section_id(&self, e: &Entry) -> Result<u32> {
        Reader::at(&self.data, e.offset).u32()
    }

    fn raw_header(&self) -> Result<ImageHeader> {
        let mut first_other = None;
        for e in &self.entries {
            if self.section_id(e)? != SECI {
                continue;
            }
            let mut r = Reader::at(&self.data, e.offset + 8);
            let ty = r.u32()?;
            let fmt = r.u32()?;
            let columns = r.u32()? as usize;
            let rows = r.u32()? as usize;
            let type_format = (ty << 16) + fmt;
            match type_format {
                FORMAT_MERRILL | FORMAT_QUATTRO | FORMAT_SDQ | FORMAT_SDQH => {
                    return Ok(ImageHeader {
                        format: type_format,
                        columns,
                        rows,
                        body: e.offset + IMAGE_HEADER_SIZE,
                        end: e.offset + e.size,
                    })
                }
                // Thumbnails/previews are not RAW; anything with image type 1 or 3 is.
                _ if ty == 1 || ty == 3 => first_other = first_other.or(Some(type_format)),
                _ => {}
            }
        }
        match first_other {
            Some(t) => Err(Error::Unsupported(t)),
            None => Err(Error::NoRawImage),
        }
    }

    pub fn properties(&self) -> Result<Properties> {
        let mut out = Vec::new();
        for e in &self.entries {
            if self.section_id(e)? != SECP {
                continue;
            }
            let mut r = Reader::at(&self.data, e.offset + 8);
            let num = r.u32()? as usize;
            // 実ファイルは数十件。件数と文字列長に上限を置き、壊れたファイルで時間やメモリを
            // 使い切らないようにする。
            if num > MAX_PROPERTIES {
                return Err(Error::Corrupt("too many properties"));
            }
            let mut r = Reader::at(&self.data, e.offset + PROPERTY_HEADER_SIZE);
            let mut table = Vec::with_capacity(num);
            for _ in 0..num {
                table.push((r.u32()? as usize, r.u32()? as usize));
            }
            // 表がセクションの宣言サイズをはみ出していたら壊れている。
            let rest = (e.offset + e.size)
                .checked_sub(r.pos)
                .ok_or(Error::Truncated)?;
            let strings = r.take(rest)?;
            let utf16 = |off: usize| -> String {
                let units: Vec<u16> = off
                    .checked_mul(2)
                    .and_then(|at| strings.get(at..))
                    .unwrap_or(&[])
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .take_while(|&u| u != 0)
                    .take(MAX_PROPERTY_CHARS)
                    .collect();
                String::from_utf16_lossy(&units)
            };
            for (n, v) in table {
                out.push((utf16(n), utf16(v)));
            }
        }
        Ok(Properties(out))
    }

    /// The white balance name stored in the file header (X3F versions 2.1 up to 3.x only).
    pub fn header_white_balance(&self) -> Option<String> {
        let version = Reader::at(&self.data, 4).u32().ok()?;
        if !(0x0002_0001..0x0004_0000).contains(&version) {
            return None;
        }
        let raw = self.data.get(40..72)?;
        let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
        let s = String::from_utf8_lossy(&raw[..end]).trim().to_owned();
        (!s.is_empty()).then_some(s)
    }

    /// Embedded JPEG previews/thumbnails; the larger ones carry EXIF.
    pub fn preview_jpegs(&self) -> impl Iterator<Item = &[u8]> {
        self.entries.iter().filter_map(|e| {
            if self.section_id(e).ok()? != SECI {
                return None;
            }
            let ty = Reader::at(&self.data, e.offset + 8).u32().ok()?;
            let body = self
                .data
                .get(e.offset + IMAGE_HEADER_SIZE..e.offset + e.size)?;
            (ty == TYPE_PREVIEW && body.starts_with(&[0xff, 0xd8])).then_some(body)
        })
    }

    /// Decodes the camera metadata block, if the file has one.
    pub fn camf(&self) -> Result<Option<Camf>> {
        for e in &self.entries {
            if self.section_id(e)? != SECC {
                continue;
            }
            let mut r = Reader::at(&self.data, e.offset + 8);
            let kind = r.u32()?;
            let params = [r.u32()?, r.u32()?, r.u32()?, r.u32()?];
            let body = self
                .data
                .get(e.offset + CAMF_HEADER_SIZE..e.offset + e.size)
                .ok_or(Error::Truncated)?;
            return camf::decode(kind, params, body).map(Some);
        }
        Ok(None)
    }

    pub fn decode(&self) -> Result<Raw> {
        let h = self.raw_header()?;
        let quattro = h.format != FORMAT_MERRILL;
        let mut r = Reader::at(&self.data, h.body);

        let mut plane_dims = [(0usize, 0usize); TRUE_PLANES];
        if quattro {
            for d in plane_dims.iter_mut() {
                let cols = r.u16()? as usize;
                let rows = r.u16()? as usize;
                *d = (cols, rows);
            }
        }

        let mut seed = [0i32; TRUE_PLANES];
        for s in seed.iter_mut() {
            *s = r.u16()? as i32;
        }
        r.u16()?; // unknown

        let tree = HuffTree::read(&mut r)?;
        if quattro {
            r.u32()?; // unknown
        }
        let mut plane_size = [0usize; TRUE_PLANES];
        for s in plane_size.iter_mut() {
            *s = r.u32()? as usize;
        }

        // The section's declared size can be smaller than the headers we just read.
        let payload = self.data.get(r.pos..h.end).ok_or(Error::Truncated)?;
        let mut planes: [&[u8]; TRUE_PLANES] = [&[]; TRUE_PLANES];
        let mut off = 0usize;
        for i in 0..TRUE_PLANES {
            let end = off.checked_add(plane_size[i]).ok_or(Error::Truncated)?;
            planes[i] = payload
                .get(off..end.min(payload.len()))
                .ok_or(Error::Truncated)?;
            off += plane_size[i].div_ceil(16) * 16;
        }

        if !quattro {
            let (w, h_) = (h.columns, h.rows);
            let len = sample_count(&[(w, h_, 3)])?;
            let mut rgb = vec![0u16; len];
            for c in 0..3 {
                decode_plane(planes[c], &tree, seed[c], w, h_, w, |idx, v| {
                    rgb[idx * 3 + c] = v
                })?;
            }
            return Ok(Raw::Merrill {
                width: w,
                height: h_,
                rgb,
            });
        }

        let (lw, lh) = plane_dims[0];
        let (tw, th) = plane_dims[2];
        if lh == h.rows / 2 {
            // Both low layers are written into one buffer sized by the first plane.
            if plane_dims[1] != plane_dims[0] {
                return Err(Error::Corrupt("mismatched Quattro layer sizes"));
            }
            let low_len = sample_count(&[(lw, lh, 3)])?;
            let top_len = sample_count(&[(tw, th, 1)])?;
            let mut low = vec![0u16; low_len];
            for c in 0..2 {
                let (cols, rows) = plane_dims[c];
                decode_plane(planes[c], &tree, seed[c], cols, rows, lw, |idx, v| {
                    low[idx * 3 + c] = v
                })?;
            }
            let mut top = vec![0u16; top_len];
            decode_plane(planes[2], &tree, seed[2], tw, th, tw, |idx, v| top[idx] = v)?;
            Ok(Raw::Quattro {
                low_width: lw,
                low_height: lh,
                low,
                top_width: tw,
                top_height: th,
                top,
            })
        } else if lh == h.rows {
            // Binned Quattro: all three layers have the same size.
            if plane_dims.iter().any(|&d| d != plane_dims[0]) {
                return Err(Error::Corrupt("mismatched Quattro layer sizes"));
            }
            let mut rgb = vec![0u16; sample_count(&[(lw, lh, 3)])?];
            for c in 0..3 {
                let (cols, rows) = plane_dims[c];
                decode_plane(planes[c], &tree, seed[c], cols, rows, lw, |idx, v| {
                    rgb[idx * 3 + c] = v
                })?;
            }
            Ok(Raw::Merrill {
                width: lw,
                height: lh,
                rgb,
            })
        } else {
            Err(Error::Corrupt("unknown Quattro layer size"))
        }
    }
}

/// Upper bound on the samples of one decoded buffer (512 MiB of u16). Real files stay below a
/// quarter of this (sd Quattro H: about 8000 x 5300 x 3).
const MAX_SAMPLES: usize = 1 << 28;

/// Number of samples for `(width, height, channels)` buffers, rejecting zero sizes and sizes the
/// file could use to exhaust memory or overflow the arithmetic.
fn sample_count(dims: &[(usize, usize, usize)]) -> Result<usize> {
    let mut total = 0usize;
    for &(w, h, c) in dims {
        let n = w
            .checked_mul(h)
            .and_then(|n| n.checked_mul(c))
            .filter(|&n| n > 0 && n <= MAX_SAMPLES)
            .ok_or(Error::Corrupt("invalid image size"))?;
        total += n;
    }
    Ok(total)
}

/// Binary tree over the TRUE Huffman table. Leaf value = number of difference bits.
pub(crate) struct HuffTree {
    nodes: Vec<[u16; 2]>,
    leaf: Vec<u8>,
}

const NONE: u16 = u16::MAX;

impl HuffTree {
    /// Builds the tree from `(code_size, code)` pairs; the leaf value is the pair's index.
    pub(crate) fn from_pairs(pairs: impl Iterator<Item = (u8, u8)>) -> Result<HuffTree> {
        let mut t = HuffTree {
            nodes: vec![[NONE, NONE]],
            leaf: vec![NONE as u8],
        };
        for (i, (size, code)) in pairs.enumerate() {
            let (size, code) = (size as u32, code as u32);
            if size > 8 {
                return Err(Error::Corrupt("huffman code too long"));
            }
            let code = (code >> (8 - size)) & 0xff;
            let mut n = 0usize;
            for b in (0..size).rev() {
                let bit = ((code >> b) & 1) as usize;
                if t.nodes[n][bit] == NONE {
                    t.nodes.push([NONE, NONE]);
                    t.leaf.push(NONE as u8);
                    t.nodes[n][bit] = (t.nodes.len() - 1) as u16;
                }
                n = t.nodes[n][bit] as usize;
            }
            t.leaf[n] = i as u8;
        }
        Ok(t)
    }

    /// Reads the table stored in an image section: pairs up to a zero code size.
    fn read(r: &mut Reader) -> Result<HuffTree> {
        let mut pairs = Vec::new();
        while pairs.len() < 256 {
            let size = r.u8()?;
            let code = r.u8()?;
            if size == 0 {
                break;
            }
            pairs.push((size, code));
        }
        HuffTree::from_pairs(pairs.into_iter())
    }
}

/// MSB 先頭のビットリーダ。終端を越えて読むとエラーにする(途中で切れたストリームを、
/// 黙ってゼロのビットで埋めて画素をでっち上げないため)。
pub(crate) struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0 }
    }

    fn bit(&mut self) -> Result<u8> {
        let byte = *self.data.get(self.pos >> 3).ok_or(Error::Truncated)?;
        let b = (byte >> (7 - (self.pos & 7))) & 1;
        self.pos += 1;
        Ok(b)
    }

    /// One TRUE difference value: Huffman-coded bit count, then that many raw bits.
    pub(crate) fn diff(&mut self, tree: &HuffTree) -> Result<i32> {
        let mut n = 0usize;
        while tree.nodes[n] != [NONE, NONE] {
            n = tree.nodes[n][self.bit()? as usize] as usize;
            if n == NONE as usize {
                return Err(Error::Corrupt("unexpected huffman bit"));
            }
        }
        let nbits = tree.leaf[n];
        if nbits == 0 || nbits == NONE as u8 {
            return Ok(0);
        }
        // 葉の値は表の添字なので、壊れた表では 255 まであり得る。シフトがあふれる前に弾く。
        if nbits > MAX_DIFF_BITS {
            return Err(Error::Corrupt("huffman difference too wide"));
        }
        let first = self.bit()? as i32;
        let mut d = first;
        for _ in 1..nbits {
            d = (d << 1) + self.bit()? as i32;
        }
        if first == 0 {
            d -= (1 << nbits) - 1;
        }
        Ok(d)
    }
}

/// Decode one TRUE plane. `emit(index, value)` receives the pixel index within a row-major
/// image that is `stride` wide; columns beyond `stride` are decoded but discarded.
fn decode_plane(
    data: &[u8],
    tree: &HuffTree,
    seed: i32,
    cols: usize,
    rows: usize,
    stride: usize,
    mut emit: impl FnMut(usize, u16),
) -> Result<()> {
    let mut bits = Bits::new(data);

    let mut row_start = [[seed; 2]; 2];
    for row in 0..rows {
        let odd_row = row & 1;
        let mut acc = [0i32; 2];
        for col in 0..cols {
            let odd_col = col & 1;

            let diff = bits.diff(tree)?;

            let prev = if col < 2 {
                row_start[odd_row][odd_col]
            } else {
                acc[odd_col]
            };
            // 壊れたストリームでは差分の累積が i32 をあふれ得る。値は下位 16 ビットしか使わない。
            let value = prev.wrapping_add(diff);
            acc[odd_col] = value;
            if col < 2 {
                row_start[odd_row][odd_col] = value;
            }
            if col < stride {
                emit(row * stride + col, value as u16);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32s(v: &[u32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    /// An X3F holding one image section: `format`/`columns`/`rows` in its header, `body` after the
    /// 28 byte header, and `declared_size` as the size the directory claims for the section.
    fn x3f_with_image(
        type_format: (u32, u32),
        columns: u32,
        rows: u32,
        body: &[u8],
        declared_size: u32,
    ) -> Vec<u8> {
        let mut f = vec![0u8; 128];
        f[..4].copy_from_slice(b"FOVb");
        f[4..8].copy_from_slice(&0x0002_0300u32.to_le_bytes());
        let off = f.len() as u32;
        f.extend_from_slice(b"SECi");
        f.extend(u32s(&[0, type_format.0, type_format.1, columns, rows]));
        f.extend_from_slice(body);
        f.resize(f.len().max((off + declared_size) as usize), 0);
        let dir = f.len() as u32;
        f.extend_from_slice(b"SECd");
        f.extend(u32s(&[0, 1, off, declared_size, 0, dir]));
        f
    }

    /// Merrill/Quattro image body: seeds, empty Huffman table, plane sizes of zero.
    fn body(quattro: bool, dims: [(u16, u16); 3]) -> Vec<u8> {
        let mut b = Vec::new();
        if quattro {
            for (c, r) in dims {
                b.extend(c.to_le_bytes());
                b.extend(r.to_le_bytes());
            }
        }
        b.extend([0u8; 8]); // three seeds + one unknown u16
        b.extend([0u8; 2]); // table terminator
        if quattro {
            b.extend([0u8; 4]);
        }
        b.extend([0u8; 12]); // plane sizes
        b
    }

    fn decode(data: Vec<u8>) -> Result<Raw> {
        X3f::parse(data)?.decode()
    }

    #[test]
    fn section_smaller_than_its_headers_is_rejected() {
        let f = x3f_with_image((1, 0x23), 100, 100, &body(true, [(1, 1); 3]), 24);
        assert!(matches!(decode(f), Err(Error::Truncated)));
    }

    #[test]
    fn huge_or_zero_image_sizes_are_rejected_without_allocating() {
        let b = body(false, [(0, 0); 3]);
        for (w, h) in [(u32::MAX, u32::MAX), (100_000, 100_000), (0, 10)] {
            let f = x3f_with_image((1, 0x1e), w, h, &b, 128);
            assert!(matches!(decode(f), Err(Error::Corrupt(_))), "{w}x{h}");
        }
        let q = body(true, [(0, 0), (0, 0), (60_000, 60_000)]);
        let f = x3f_with_image((1, 0x23), 100, 0, &q, 128);
        assert!(matches!(decode(f), Err(Error::Corrupt(_))));
    }

    /// 4x4 の Merrill 画像。符号は「1 ビットの 0 = 差分なし」だけなので、1 プレーンに 16 ビット要る。
    fn merrill_4x4(plane_bytes: u32) -> Vec<u8> {
        // `x3f_with_image` が書くヘッダは 24 バイト。28 バイトのヘッダの残り(行サイズ)を先に足す。
        let mut b = vec![0u8; 4];
        b.extend([0u8; 8]); // シード 3 つと不明な u16
        b.extend([1, 0, 0, 0]); // 符号表: (長さ 1, 符号 0)、終端
        b.extend(u32s(&[plane_bytes; 3]));
        b.extend([0u8; 48]); // プレーンは 16 バイト境界に並ぶ
        let size = (IMAGE_HEADER_SIZE - 4 + b.len()) as u32;
        x3f_with_image((1, 0x1e), 4, 4, &b, size)
    }

    #[test]
    fn decode_complete_planes_returns_the_image() {
        let Ok(Raw::Merrill { width, height, rgb }) = decode(merrill_4x4(2)) else {
            panic!("decodes");
        };
        assert_eq!((width, height, rgb.len()), (4, 4, 48));
        assert!(rgb.iter().all(|&v| v == 0));
    }

    #[test]
    fn decode_truncated_plane_returns_error() {
        // 16 ビット要るところに 8 ビットしかない。ゼロで埋めて続けたりしない。
        assert!(matches!(decode(merrill_4x4(1)), Err(Error::Truncated)));
    }

    #[test]
    fn diff_wider_than_the_limit_returns_error() {
        // 葉の値(= 表の添字)が 25 になる符号。差分 25 ビットは上限を超える。
        let tree = HuffTree::from_pairs((0..26u8).map(|i| (8, i))).unwrap();
        let wide = Bits::new(&[25, 0, 0, 0, 0]).diff(&tree);
        assert!(matches!(wide, Err(Error::Corrupt(_))), "{wide:?}");
        // 上限以内なら読める(24 ビットの 1 → 2^24 - 1)。
        let ok = Bits::new(&[24, 0xff, 0xff, 0xff]).diff(&tree);
        assert_eq!(ok.ok(), Some((1 << 24) - 1));
    }

    #[test]
    fn decode_overflowing_differences_does_not_panic() {
        // 毎画素 +(2^24 - 1) を足し続けると i32 をあふれる幅の画像。
        let tree = HuffTree::from_pairs((0..26u8).map(|i| (8, i))).unwrap();
        let data: Vec<u8> = (0..400).flat_map(|_| [24u8, 0xff, 0xff, 0xff]).collect();
        let mut n = 0;
        decode_plane(&data, &tree, i32::MAX - 5, 400, 1, 400, |_, _| n += 1).unwrap();
        assert_eq!(n, 400);
    }

    /// PROP セクション 1 つだけの X3F。`declared_size` はディレクトリが言うセクションの大きさ。
    fn x3f_with_properties(num: u32, table_and_strings: &[u8], declared_size: u32) -> Vec<u8> {
        let mut f = vec![0u8; 128];
        f[..4].copy_from_slice(b"FOVb");
        let off = f.len() as u32;
        f.extend_from_slice(b"SECp");
        f.extend(u32s(&[0, num, 0, 0, 0]));
        f.extend_from_slice(table_and_strings);
        let dir = f.len() as u32;
        f.extend_from_slice(b"SECd");
        f.extend(u32s(&[0, 1, off, declared_size, 0, dir]));
        f
    }

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    }

    #[test]
    fn properties_well_formed_section_returns_the_pairs() {
        let mut body = u32s(&[0, 4]); // 名前は 0 文字目、値は 4 文字目から
        body.extend(utf16("ISO"));
        body.extend(utf16("100"));
        let size = PROPERTY_HEADER_SIZE as u32 + body.len() as u32;
        let x = X3f::parse(x3f_with_properties(1, &body, size)).unwrap();
        assert_eq!(x.properties().unwrap().get("ISO"), Some("100"));
    }

    #[test]
    fn properties_table_beyond_the_section_returns_error() {
        // 表(2 件 = 16 バイト)が、宣言されたセクション(ヘッダ + 8 バイト)をはみ出す。
        let body = u32s(&[0, 0, 0, 0]);
        let f = x3f_with_properties(2, &body, PROPERTY_HEADER_SIZE as u32 + 8);
        let r = X3f::parse(f).unwrap().properties();
        assert!(matches!(r, Err(Error::Truncated)));
    }

    #[test]
    fn properties_absurd_count_returns_error() {
        let f = x3f_with_properties(u32::MAX, &[], PROPERTY_HEADER_SIZE as u32);
        let r = X3f::parse(f).unwrap().properties();
        assert!(matches!(r, Err(Error::Corrupt(_))));
    }

    #[test]
    fn properties_string_without_terminator_is_cut_at_the_limit() {
        let mut body = u32s(&[0, 0]);
        body.extend(std::iter::repeat_n([b'a', 0], MAX_PROPERTY_CHARS + 50).flatten());
        let size = PROPERTY_HEADER_SIZE as u32 + body.len() as u32;
        let x = X3f::parse(x3f_with_properties(1, &body, size)).unwrap();
        assert_eq!(x.properties().unwrap().0[0].0.len(), MAX_PROPERTY_CHARS);
    }

    #[test]
    fn parse_truncated_or_empty_file_returns_error() {
        assert!(matches!(X3f::parse(Vec::new()), Err(Error::Truncated)));
        assert!(matches!(X3f::parse(b"FOV".to_vec()), Err(Error::Truncated)));
        // ディレクトリの位置がファイルの外。
        let mut f = b"FOVb".to_vec();
        f.extend([0u8; 60]);
        f.extend(u32::MAX.to_le_bytes());
        assert!(matches!(X3f::parse(f), Err(Error::Truncated)));
        // どの長さで切っても、パニックせずエラーか成功を返す。
        let whole = merrill_4x4(2);
        for n in 0..whole.len() {
            let _ = X3f::parse(whole[..n].to_vec()).and_then(|x| {
                x.properties()?;
                x.camf()?;
                x.decode()
            });
        }
    }

    #[test]
    fn decode_unsupported_camera_returns_error() {
        // SD14 など旧世代の RAW(type 3, format 6)は対象外。
        let f = x3f_with_image((3, 6), 100, 100, &[0; 64], 128);
        assert!(matches!(decode(f), Err(Error::Unsupported(0x0003_0006))));
    }

    #[test]
    fn quattro_layers_of_different_sizes_are_rejected() {
        let b = body(true, [(10, 50), (10, 60), (20, 100)]);
        let f = x3f_with_image((1, 0x23), 20, 100, &b, 128);
        let r = decode(f);
        assert!(matches!(r, Err(Error::Corrupt(_))), "{:?}", r.err());
    }
}
