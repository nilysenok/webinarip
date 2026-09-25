//! Reading MP4 boxes: sizes, types, nested paths, big-endian numbers.

use crate::{Error, Result};

pub(crate) fn bad(what: &str) -> Error {
    Error::Parse(format!("video fragment: {what}"))
}

pub(crate) fn u32_at(b: &[u8], at: usize) -> Result<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes(s.try_into().unwrap()))
        .ok_or_else(|| bad("truncated"))
}

pub(crate) fn u64_at(b: &[u8], at: usize) -> Result<u64> {
    b.get(at..at + 8)
        .map(|s| u64::from_be_bytes(s.try_into().unwrap()))
        .ok_or_else(|| bad("truncated"))
}

/// Child boxes of `buf[start..end]`: (type, box start, payload start, box end).
/// (type, box start, payload start, box end)
pub(crate) type BoxRef = ([u8; 4], usize, usize, usize);

pub(crate) fn boxes(buf: &[u8], start: usize, end: usize) -> Result<Vec<BoxRef>> {
    let (mut out, mut at) = (Vec::new(), start);
    while at + 8 <= end {
        let size = u32_at(buf, at)? as usize;
        let kind: [u8; 4] = buf[at + 4..at + 8].try_into().unwrap();
        let (hdr, size) = match size {
            1 => (16, u64_at(buf, at + 8)? as usize),
            0 => (8, end - at),
            s => (8, s),
        };
        if size < hdr || at + size > end {
            return Err(bad("box size"));
        }
        out.push((kind, at, at + hdr, at + size));
        at += size;
    }
    Ok(out)
}

pub(crate) fn find(buf: &[u8], start: usize, end: usize, path: &[&[u8; 4]]) -> Result<Option<(usize, usize)>> {
    let Some((first, rest)) = path.split_first() else {
        return Ok(Some((start, end)));
    };
    for (kind, _, body, stop) in boxes(buf, start, end)? {
        if &kind == *first {
            return find(buf, body, stop, rest);
        }
    }
    Ok(None)
}
