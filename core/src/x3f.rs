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
            let mut r = Reader::at(&self.data, e.offset + PROPERTY_HEADER_SIZE);
            let mut table = Vec::with_capacity(num.min(4096));
            for _ in 0..num {
                table.push((r.u32()? as usize, r.u32()? as usize));
            }
            let strings = r.take(e.offset + e.size - r.pos)?;
            let utf16 = |off: usize| -> String {
                let units: Vec<u16> = strings
                    .get(off * 2..)
                    .unwrap_or(&[])
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .take_while(|&u| u != 0)
                    .collect();
                String::from_utf16_lossy(&units)
            };
            for (n, v) in table {
                out.push((utf16(n), utf16(v)));
            }
        }
        Ok(Properties(out))
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

        let payload = &self.data[r.pos..h.end];
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
            let mut rgb = vec![0u16; w * h_ * 3];
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
            let mut low = vec![0u16; lw * lh * 3];
            for c in 0..2 {
                let (cols, rows) = plane_dims[c];
                decode_plane(planes[c], &tree, seed[c], cols, rows, lw, |idx, v| {
                    low[idx * 3 + c] = v
                })?;
            }
            let mut top = vec![0u16; tw * th];
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
            let mut rgb = vec![0u16; lw * lh * 3];
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

/// MSB-first bit reader; reads past the end yield zero bits.
pub(crate) struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0 }
    }

    fn bit(&mut self) -> u8 {
        let byte = self.data.get(self.pos >> 3).copied().unwrap_or(0);
        let b = (byte >> (7 - (self.pos & 7))) & 1;
        self.pos += 1;
        b
    }

    /// One TRUE difference value: Huffman-coded bit count, then that many raw bits.
    pub(crate) fn diff(&mut self, tree: &HuffTree) -> Result<i32> {
        let mut n = 0usize;
        while tree.nodes[n] != [NONE, NONE] {
            n = tree.nodes[n][self.bit() as usize] as usize;
            if n == NONE as usize {
                return Err(Error::Corrupt("unexpected huffman bit"));
            }
        }
        let nbits = tree.leaf[n];
        if nbits == 0 || nbits == NONE as u8 {
            return Ok(0);
        }
        let first = self.bit() as i32;
        let mut d = first;
        for _ in 1..nbits {
            d = (d << 1) + self.bit() as i32;
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
            let value = prev + diff;
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
