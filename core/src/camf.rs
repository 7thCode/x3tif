//! CAMF: the camera metadata block. It is stored compressed with the same difference/Huffman
//! scheme as the image planes, and unpacks into a list of named text and matrix entries.

use crate::x3f::{Bits, Error, HuffTree};

type Result<T> = std::result::Result<T, Error>;

const CMB_P: u32 = 0x5062_4d43;
const CMB_T: u32 = 0x5462_4d43;
const CMB_M: u32 = 0x4d62_4d43;

/// The Huffman table occupies the first 28 bytes, followed by a size word and the bit stream.
const TABLE_BYTES: usize = 28;
const STREAM_OFFSET: usize = 32;

/// Matrices larger than this keep only their dimensions (bad-pixel maps, gain tables, ...).
const MAX_STORED_ELEMENTS: usize = 4096;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Text(String),
    Matrix { dims: Vec<usize>, values: Vec<f64> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub value: Value,
}

#[derive(Debug, Default)]
pub struct Camf(pub Vec<Entry>);

impl Camf {
    pub fn text(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|e| e.name == name)
            .and_then(|e| match &e.value {
                Value::Text(t) => Some(t.as_str()),
                _ => None,
            })
    }

    pub fn matrix(&self, name: &str) -> Option<&[f64]> {
        self.0
            .iter()
            .find(|e| e.name == name)
            .and_then(|e| match &e.value {
                Value::Matrix { values, .. } if !values.is_empty() => Some(values.as_slice()),
                _ => None,
            })
    }

    pub fn number(&self, name: &str) -> Option<f64> {
        self.matrix(name).and_then(|m| m.first().copied())
    }
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    let s = b.get(o..o.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn cstr(b: &[u8], o: usize) -> String {
    let s = b.get(o..).unwrap_or(&[]);
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf8_lossy(&s[..end]).into_owned()
}

/// Decodes a CAMF section body. `kind` and `params` come from the section header
/// (type 4: Merrill, type 5: Quattro).
pub(crate) fn decode(kind: u32, params: [u32; 4], body: &[u8]) -> Result<Camf> {
    let [decoded_size, bias, block_size, block_count] = params;
    if kind != 4 && kind != 5 {
        return Err(Error::Corrupt("unsupported CAMF type"));
    }
    let table = body.get(..TABLE_BYTES).ok_or(Error::Truncated)?;
    let pairs = table
        .chunks_exact(2)
        .take_while(|p| p[0] != 0)
        .map(|p| (p[0], p[1]));
    let tree = HuffTree::from_pairs(pairs)?;
    let stream = body.get(STREAM_OFFSET..).ok_or(Error::Truncated)?;
    let size = decoded_size as usize;
    if size > 1 << 28 {
        return Err(Error::Corrupt("CAMF too large"));
    }
    let data = if kind == 5 {
        unpack_bytes(stream, &tree, bias as i32, size)?
    } else {
        unpack_12bit(
            stream,
            &tree,
            bias as i32,
            block_size as usize,
            block_count as usize,
            size,
        )?
    };
    Ok(Camf(entries(&data)))
}

/// Type 5: one difference-coded byte per output byte.
fn unpack_bytes(stream: &[u8], tree: &HuffTree, bias: i32, size: usize) -> Result<Vec<u8>> {
    let mut bits = Bits::new(stream);
    let mut acc = bias;
    let mut out = Vec::with_capacity(size);
    for _ in 0..size {
        acc += bits.diff(tree)?;
        out.push(acc as u8);
    }
    Ok(out)
}

/// Type 4: 12-bit values (two interleaved difference streams per row) packed into bytes.
fn unpack_12bit(
    stream: &[u8],
    tree: &HuffTree,
    bias: i32,
    cols: usize,
    rows: usize,
    size: usize,
) -> Result<Vec<u8>> {
    let mut bits = Bits::new(stream);
    let mut out = vec![0u8; size];
    let mut at = 0usize;
    let mut odd_dst = false;
    let mut row_start = [[bias; 2]; 2];
    for row in 0..rows {
        let mut acc = [0i32; 2];
        for col in 0..cols {
            let (oc, or) = (col & 1, row & 1);
            let prev = if col < 2 { row_start[or][oc] } else { acc[oc] };
            let v = prev + bits.diff(tree)?;
            acc[oc] = v;
            if col < 2 {
                row_start[or][oc] = v;
            }
            if !odd_dst {
                out[at] = ((v >> 4) & 0xff) as u8;
                at += 1;
                if at >= size {
                    return Ok(out);
                }
                out[at] = ((v << 4) & 0xf0) as u8;
            } else {
                out[at] |= ((v >> 8) & 0x0f) as u8;
                at += 1;
                if at >= size {
                    return Ok(out);
                }
                out[at] = (v & 0xff) as u8;
                at += 1;
                if at >= size {
                    return Ok(out);
                }
            }
            odd_dst = !odd_dst;
        }
    }
    Ok(out)
}

/// Splits the decoded block into named entries. Parsing stops at the first unknown entry.
fn entries(data: &[u8]) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 20 <= data.len() {
        let (Some(id), Some(entry_size), Some(name_off), Some(value_off)) = (
            u32_at(data, p),
            u32_at(data, p + 8),
            u32_at(data, p + 12),
            u32_at(data, p + 16),
        ) else {
            break;
        };
        if !matches!(id, CMB_P | CMB_T | CMB_M) {
            break;
        }
        let entry_size = entry_size as usize;
        let Some(e) = data
            .get(p..p.saturating_add(entry_size))
            .filter(|e| e.len() >= 20)
        else {
            break;
        };
        let name = cstr(e, name_off as usize);
        let value = match id {
            CMB_T => text(e, value_off as usize),
            CMB_M => matrix(e, value_off as usize),
            _ => None, // property lists carry no values worth keeping
        };
        if let Some(value) = value {
            out.push(Entry { name, value });
        }
        p += entry_size.max(20);
    }
    out
}

fn text(e: &[u8], at: usize) -> Option<Value> {
    let len = u32_at(e, at)? as usize;
    let raw = e.get(at + 4..at.checked_add(4)?.checked_add(len)?.min(e.len()))?;
    let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    Some(Value::Text(
        String::from_utf8_lossy(&raw[..end]).into_owned(),
    ))
}

fn matrix(e: &[u8], at: usize) -> Option<Value> {
    let ty = u32_at(e, at)?;
    let ndim = u32_at(e, at + 4)? as usize;
    let data_off = u32_at(e, at + 8)? as usize;
    if ndim == 0 || ndim > 8 {
        return None;
    }
    let mut dims = Vec::with_capacity(ndim);
    let mut total = 1usize;
    for i in 0..ndim {
        let d = u32_at(e, at + 12 + 12 * i)? as usize;
        dims.push(d);
        total = total.checked_mul(d)?;
    }
    let width = match ty {
        5 => 1,
        0 | 6 => 2,
        1..=3 => 4,
        _ => return None,
    };
    if total > MAX_STORED_ELEMENTS {
        return Some(Value::Matrix {
            dims,
            values: Vec::new(),
        });
    }
    let raw = e.get(data_off..data_off.checked_add(total.checked_mul(width)?)?)?;
    let values = raw
        .chunks_exact(width)
        .map(|c| match ty {
            0 => i16::from_le_bytes([c[0], c[1]]) as f64,
            1 | 2 => u32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64,
            3 => f32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64,
            5 => c[0] as f64,
            _ => u16::from_le_bytes([c[0], c[1]]) as f64,
        })
        .collect();
    Some(Value::Matrix { dims, values })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u32, name: &str, value: &[u8]) -> Vec<u8> {
        let name_off = 20u32;
        let value_off = name_off + name.len() as u32 + 1;
        let size = value_off + value.len() as u32;
        let mut e = Vec::new();
        for v in [id, 0, size, name_off, value_off] {
            e.extend_from_slice(&v.to_le_bytes());
        }
        e.extend_from_slice(name.as_bytes());
        e.push(0);
        e.extend_from_slice(value);
        e
    }

    #[test]
    fn parses_text_and_float_matrix_entries() {
        let mut text = 3u32.to_le_bytes().to_vec();
        text.extend_from_slice(b"hi\0");
        // type 3 (f32), 1 dimension. Data offsets count from the entry start: 20 byte entry
        // header + "Gain\0" + 24 byte matrix header.
        let mut m = Vec::new();
        for v in [3u32, 1, 49, 3, 0, 0] {
            m.extend_from_slice(&v.to_le_bytes());
        }
        for f in [1.5f32, 2.0, -0.25] {
            m.extend_from_slice(&f.to_le_bytes());
        }
        let mut data = entry(CMB_T, "Note", &text);
        data.extend(entry(CMB_M, "Gain", &m));
        data.extend_from_slice(&[0xff; 24]); // unknown trailing entry stops parsing

        let camf = Camf(entries(&data));
        assert_eq!(camf.text("Note"), Some("hi"));
        assert_eq!(camf.matrix("Gain"), Some(&[1.5, 2.0, -0.25][..]));
        assert_eq!(camf.number("Gain"), Some(1.5));
        assert_eq!(camf.0.len(), 2);
    }

    #[test]
    fn oversized_matrix_keeps_only_dimensions() {
        let mut m = Vec::new();
        for v in [5u32, 1, 24, (MAX_STORED_ELEMENTS + 1) as u32, 0, 0] {
            m.extend_from_slice(&v.to_le_bytes());
        }
        let camf = Camf(entries(&entry(CMB_M, "Big", &m)));
        assert_eq!(camf.matrix("Big"), None);
        assert_eq!(camf.0.len(), 1);
    }

    #[test]
    fn truncated_entry_does_not_panic() {
        let mut data = entry(CMB_T, "Note", &[200, 0, 0, 0, b'x']);
        data.truncate(data.len() - 2);
        let _ = entries(&data);
    }
}
