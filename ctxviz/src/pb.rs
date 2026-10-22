//! Schema-less protobuf wire decoder (port of `scripts/pb.py`).
//!
//! Every function borrows from the input buffer and never panics: truncated or
//! malformed input is an `Err`. `next_field` and `find_first` let a fast path
//! pull a single field out of a message without allocating.
//!
//! Input is untrusted: group nesting is capped at [`MAX_DEPTH`] (deeper input
//! is an `Err`, not a stack overflow) and a single message or group holds at
//! most [`MAX_FIELDS`] fields.

use anyhow::{bail, Result};
use std::collections::BTreeMap;

pub const WT_VARINT: u8 = 0;
pub const WT_FIXED64: u8 = 1;
pub const WT_BYTES: u8 = 2;
pub const WT_START_GROUP: u8 = 3;
pub const WT_END_GROUP: u8 = 4;
pub const WT_FIXED32: u8 = 5;

/// Deepest allowed group nesting; deeper input is an error.
pub const MAX_DEPTH: u32 = 32;

/// Most fields decoded from one message or group before giving up.
pub const MAX_FIELDS: usize = 8 * 1024 * 1024;

/// A decoded field value. Byte payloads borrow from the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value<'a> {
    Varint(u64),
    Fixed64(u64),
    /// Length-delimited: string, bytes, or an embedded message.
    Bytes(&'a [u8]),
    /// Deprecated group (wire types 3..4); the raw bytes between the markers.
    Group(&'a [u8]),
    Fixed32(u32),
}

impl<'a> Value<'a> {
    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Value::Varint(v) | Value::Fixed64(v) => Some(v),
            Value::Fixed32(v) => Some(v as u64),
            _ => None,
        }
    }
    pub fn as_bytes(&self) -> Option<&'a [u8]> {
        match *self {
            Value::Bytes(b) | Value::Group(b) => Some(b),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&'a str> {
        self.as_bytes().and_then(|b| std::str::from_utf8(b).ok())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field<'a> {
    pub number: u32,
    pub wire_type: u8,
    pub value: Value<'a>,
}

/// Read a base-128 varint at `i`; returns (value, next index).
pub fn varint(buf: &[u8], mut i: usize) -> Result<(u64, usize)> {
    let mut result: u64 = 0;
    for shift in (0..70).step_by(7) {
        let Some(&b) = buf.get(i) else { bail!("truncated varint at {i}") };
        i += 1;
        if shift == 63 && b > 1 {
            bail!("varint overflows u64 at {}", i - 1);
        }
        result |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok((result, i));
        }
    }
    bail!("varint longer than 10 bytes at {i}")
}

fn take(buf: &[u8], i: usize, n: usize) -> Result<&[u8]> {
    match i.checked_add(n) {
        Some(end) if end <= buf.len() => Ok(&buf[i..end]),
        _ => bail!("truncated field: need {n} bytes at {i}, have {}", buf.len().saturating_sub(i)),
    }
}

/// Decode the field starting at `i`. Returns `None` at end of buffer.
/// This is the lazy primitive: callers that only want one field skip the rest
/// by calling it in a loop and ignoring the value.
pub fn next_field(buf: &[u8], i: usize) -> Result<Option<(Field<'_>, usize)>> {
    next_field_at(buf, i, 0)
}

/// [`next_field`] at group nesting `depth`.
fn next_field_at(buf: &[u8], i: usize, depth: u32) -> Result<Option<(Field<'_>, usize)>> {
    if i >= buf.len() {
        return Ok(None);
    }
    let (key, mut i) = varint(buf, i)?;
    let number = (key >> 3) as u32;
    let wire_type = (key & 7) as u8;
    if number == 0 {
        bail!("field number 0 is invalid");
    }
    let value = match wire_type {
        WT_VARINT => {
            let (v, j) = varint(buf, i)?;
            i = j;
            Value::Varint(v)
        }
        WT_FIXED64 => {
            let b = take(buf, i, 8)?;
            i += 8;
            Value::Fixed64(u64::from_le_bytes(b.try_into().unwrap()))
        }
        WT_BYTES => {
            let (n, j) = varint(buf, i)?;
            let n = usize::try_from(n)?;
            let b = take(buf, j, n)?;
            i = j + n;
            Value::Bytes(b)
        }
        WT_FIXED32 => {
            let b = take(buf, i, 4)?;
            i += 4;
            Value::Fixed32(u32::from_le_bytes(b.try_into().unwrap()))
        }
        WT_START_GROUP => {
            if depth >= MAX_DEPTH {
                bail!("groups nested deeper than {MAX_DEPTH} at {i}");
            }
            let start = i;
            let mut count = 0usize;
            loop {
                count += 1;
                if count > MAX_FIELDS {
                    bail!("group {number} has more than {MAX_FIELDS} fields");
                }
                let Some((f, j)) = next_field_inner(buf, i, number, depth + 1)? else {
                    bail!("unterminated group {number}");
                };
                if let Some(end) = f {
                    i = j;
                    break Value::Group(&buf[start..end]);
                }
                i = j;
            }
        }
        _ => bail!("unsupported wire type {wire_type} for field {number}"),
    };
    Ok(Some((Field { number, wire_type, value }, i)))
}

/// Group helper: advance one field; `Some(Some(end_offset))` when the matching
/// end-group marker is reached.
fn next_field_inner(buf: &[u8], i: usize, group: u32, depth: u32) -> Result<Option<(Option<usize>, usize)>> {
    if i >= buf.len() {
        return Ok(None);
    }
    let (key, j) = varint(buf, i)?;
    if (key & 7) as u8 == WT_END_GROUP {
        if (key >> 3) as u32 != group {
            bail!("mismatched end group");
        }
        return Ok(Some((Some(i), j)));
    }
    match next_field_at(buf, i, depth)? {
        Some((_, j)) => Ok(Some((None, j))),
        None => Ok(None),
    }
}

/// Iterator over top-level fields. Yields one `Err` and stops on bad input.
pub struct Fields<'a> {
    buf: &'a [u8],
    pos: usize,
    count: usize,
    done: bool,
}

impl<'a> Iterator for Fields<'a> {
    type Item = Result<Field<'a>>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        self.count += 1;
        if self.count > MAX_FIELDS {
            self.done = true;
            return Some(Err(anyhow::anyhow!("message has more than {MAX_FIELDS} fields")));
        }
        match next_field(self.buf, self.pos) {
            Ok(Some((f, j))) => {
                self.pos = j;
                Some(Ok(f))
            }
            Ok(None) => {
                self.done = true;
                None
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}

pub fn fields(buf: &[u8]) -> Fields<'_> {
    Fields { buf, pos: 0, count: 0, done: false }
}

/// Decode every top-level field.
pub fn decode(buf: &[u8]) -> Result<Vec<Field<'_>>> {
    fields(buf).collect()
}

/// `{field_number: [values...]}` like `pb.group`.
pub fn group(buf: &[u8]) -> Result<BTreeMap<u32, Vec<Value<'_>>>> {
    let mut out: BTreeMap<u32, Vec<Value<'_>>> = BTreeMap::new();
    for f in fields(buf) {
        let f = f?;
        out.entry(f.number).or_default().push(f.value);
    }
    Ok(out)
}

/// First value of field `number`, skipping others without allocating.
/// Stops at the first match, so a malformed tail after it is not detected.
pub fn find_first(buf: &[u8], number: u32) -> Result<Option<Value<'_>>> {
    for f in fields(buf) {
        let f = f?;
        if f.number == number {
            return Ok(Some(f.value));
        }
    }
    Ok(None)
}

/// All values of field `number`, validating the whole buffer.
pub fn find_all(buf: &[u8], number: u32) -> Result<Vec<Value<'_>>> {
    let mut out = Vec::new();
    for f in fields(buf) {
        let f = f?;
        if f.number == number {
            out.push(f.value);
        }
    }
    Ok(out)
}

/// Encoding helpers, used by tests and fixtures.
pub mod encode {
    pub fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }
    pub fn key(number: u32, wt: u8, out: &mut Vec<u8>) {
        varint(((number as u64) << 3) | wt as u64, out);
    }
    pub fn uint(number: u32, v: u64, out: &mut Vec<u8>) {
        key(number, 0, out);
        varint(v, out);
    }
    pub fn bytes(number: u32, b: &[u8], out: &mut Vec<u8>) {
        key(number, 2, out);
        varint(b.len() as u64, out);
        out.extend_from_slice(b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_edges() {
        assert_eq!(varint(&[0x00], 0).unwrap(), (0, 1));
        assert_eq!(varint(&[0x7f], 0).unwrap(), (127, 1));
        assert_eq!(varint(&[0x80, 0x01], 0).unwrap(), (128, 2));
        assert_eq!(varint(&[0xac, 0x02], 0).unwrap(), (300, 2));
        let max = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01];
        assert_eq!(varint(&max, 0).unwrap(), (u64::MAX, 10));
        // Overflow and over-long encodings.
        let over = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02];
        assert!(varint(&over, 0).is_err());
        assert!(varint(&[0x80; 11], 0).is_err());
        // Truncated.
        assert!(varint(&[0x80], 0).is_err());
        assert!(varint(&[], 0).is_err());
        // Round trip.
        for v in [0u64, 1, 127, 128, 16383, 16384, 1 << 35, u64::MAX] {
            let mut b = Vec::new();
            encode::varint(v, &mut b);
            assert_eq!(varint(&b, 0).unwrap(), (v, b.len()));
        }
    }

    #[test]
    fn all_wire_types() {
        let mut b = Vec::new();
        encode::uint(1, 150, &mut b);
        encode::key(2, WT_FIXED64, &mut b);
        b.extend_from_slice(&7u64.to_le_bytes());
        encode::bytes(3, b"hi", &mut b);
        encode::key(4, WT_FIXED32, &mut b);
        b.extend_from_slice(&9u32.to_le_bytes());
        encode::key(5, WT_START_GROUP, &mut b);
        encode::uint(1, 1, &mut b);
        encode::key(5, WT_END_GROUP, &mut b);
        let f = decode(&b).unwrap();
        assert_eq!(f.len(), 5);
        assert_eq!(f[0], Field { number: 1, wire_type: 0, value: Value::Varint(150) });
        assert_eq!(f[1].value, Value::Fixed64(7));
        assert_eq!(f[2].value.as_str(), Some("hi"));
        assert_eq!(f[3].value, Value::Fixed32(9));
        assert_eq!(f[4].value, Value::Group(&[0x08, 0x01]));
    }

    #[test]
    fn nested_message() {
        let mut inner = Vec::new();
        encode::bytes(1, b"tools", &mut inner);
        encode::bytes(2, b"Tool definitions", &mut inner);
        encode::uint(3, 1234, &mut inner);
        let mut outer = Vec::new();
        encode::bytes(1, &[0xaa; 32], &mut outer);
        encode::bytes(1, &[0xbb; 32], &mut outer);
        encode::bytes(5, &inner, &mut outer);
        encode::uint(26, 1_700_000_000_000, &mut outer);

        let g = group(&outer).unwrap();
        assert_eq!(g[&1].len(), 2);
        let td = find_first(&outer, 5).unwrap().unwrap().as_bytes().unwrap();
        let g2 = group(td).unwrap();
        assert_eq!(g2[&1][0].as_str(), Some("tools"));
        assert_eq!(g2[&3][0].as_u64(), Some(1234));
        assert_eq!(find_first(&outer, 26).unwrap(), Some(Value::Varint(1_700_000_000_000)));
        assert_eq!(find_first(&outer, 99).unwrap(), None);
        assert_eq!(find_all(&outer, 1).unwrap().len(), 2);
    }

    #[test]
    fn truncated_is_err() {
        let mut b = Vec::new();
        encode::bytes(1, b"hello world", &mut b);
        for cut in 1..b.len() {
            assert!(decode(&b[..cut]).is_err(), "cut at {cut}");
        }
        // Length prefix far past the end.
        assert!(decode(&[0x0a, 0xff, 0xff, 0xff, 0xff, 0x0f]).is_err());
        // Fixed widths truncated.
        assert!(decode(&[0x09, 1, 2, 3]).is_err());
        assert!(decode(&[0x0d, 1, 2]).is_err());
        // Unknown wire types, field 0, unterminated group.
        assert!(decode(&[0x0e]).is_err());
        assert!(decode(&[0x00, 0x00]).is_err());
        assert!(decode(&[0x0b, 0x08, 0x01]).is_err());
        // The iterator yields one error then stops.
        let mut it = fields(&[0x08, 0x01, 0x0a, 0x05]);
        assert!(it.next().unwrap().is_ok());
        assert!(it.next().unwrap().is_err());
        assert!(it.next().is_none());
        assert_eq!(decode(&[]).unwrap(), vec![]);
    }

    /// Nest `depth` groups of field 1 around a varint.
    fn nested_groups(depth: usize) -> Vec<u8> {
        let mut b = Vec::new();
        for _ in 0..depth {
            encode::key(1, WT_START_GROUP, &mut b);
        }
        encode::uint(2, 1, &mut b);
        for _ in 0..depth {
            encode::key(1, WT_END_GROUP, &mut b);
        }
        b
    }

    #[test]
    fn group_depth_is_capped() {
        assert!(decode(&nested_groups(MAX_DEPTH as usize)).is_ok());
        assert!(decode(&nested_groups(MAX_DEPTH as usize + 1)).is_err());
        let start = std::time::Instant::now();
        assert!(decode(&vec![0x0b; 300_000]).is_err());
        assert!(find_first(&vec![0x0b; 300_000], 1).is_err());
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
    }
}
