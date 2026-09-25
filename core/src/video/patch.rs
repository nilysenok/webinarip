//! In-place fixes of fixed-size MP4 header fields — for fragments copied out of a longer
//! recording. No sample data is touched.

use super::boxes::{boxes, find, u32_at, u64_at};
use super::fmp4::parse_init;
use crate::Result;

fn put(buf: &mut [u8], at: usize, v1: bool, value: u64) {
    if v1 {
        buf[at..at + 8].copy_from_slice(&value.to_be_bytes());
    } else {
        buf[at..at + 4].copy_from_slice(&(value.min(u32::MAX as u64) as u32).to_be_bytes());
    }
}

/// Moves every fragment of a segment `by` media units earlier (fixed-size `tfdt` fields).
pub fn shift(seg: &mut [u8], by: u64) -> Result<()> {
    for (kind, _, body, end) in boxes(seg, 0, seg.len())? {
        if &kind != b"moof" {
            continue;
        }
        if let Some((s, _)) = find(seg, body, end, &[b"traf", b"tfdt"])? {
            let v1 = seg[s] == 1;
            let t = if v1 { u64_at(seg, s + 4)? } else { u32_at(seg, s + 4)? as u64 };
            put(seg, s + 4, v1, t.saturating_sub(by));
        }
    }
    Ok(())
}

/// Tells the movie header the real length (`media` units of the track's timescale).
pub fn set_length(init: &mut [u8], media: u64) -> Result<()> {
    let header = parse_init(init)?;
    let movie_scale = match find(init, 0, init.len(), &[b"moov", b"mvhd"])? {
        Some((s, _)) if init[s] == 1 => u32_at(init, s + 20)?,
        Some((s, _)) => u32_at(init, s + 12)?,
        None => header.timescale,
    };
    let length = media * movie_scale as u64 / header.timescale as u64;
    if let Some((s, _)) = find(init, 0, init.len(), &[b"moov", b"mvex", b"mehd"])? {
        let v1 = init[s] == 1;
        put(init, s + 4, v1, length);
    }
    if let Some((s, _)) = find(init, 0, init.len(), &[b"moov", b"mvhd"])? {
        let v1 = init[s] == 1;
        put(init, s + if v1 { 24 } else { 16 }, v1, length);
    }
    Ok(())
}
