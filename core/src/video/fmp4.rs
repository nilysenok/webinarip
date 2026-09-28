//! A small fragmented-MP4 reader for the video renditions: the init piece gives the timescale
//! and frame size, each fragment (`moof` + `mdat`) gives samples with exact timestamps and the
//! key-frame flag. Frames are passed through untouched — nothing is re-encoded.

use super::boxes::bad;
pub(crate) use super::boxes::{boxes, find, u32_at, u64_at};
use crate::Result;

#[derive(Debug, Clone, Default)]
pub struct Init {
    pub timescale: u32,
    pub width: u16,
    pub height: u16,
    /// Four-character code of the sample entry, e.g. `vp09`.
    pub codec: String,
    default_duration: u32,
    default_size: u32,
    default_flags: u32,
}

#[derive(Debug, Clone)]
pub struct Sample {
    /// Presentation time in `timescale` units.
    pub pts: i64,
    pub duration: u32,
    pub key: bool,
    /// Where the frame's bytes are in the segment buffer.
    pub offset: usize,
    pub len: usize,
}

impl Sample {
    pub fn data<'a>(&self, segment: &'a [u8]) -> &'a [u8] {
        &segment[self.offset..self.offset + self.len]
    }
}

/// Whether the init piece holds a video track (`hdlr` = `vide`). MTS Link labels camera-off
/// sessions 640x480 in the master playlist, yet behind the label there is only AAC.
pub fn is_video(init: &[u8]) -> bool {
    let hdlr = find(init, 0, init.len(), &[b"moov", b"trak", b"mdia", b"hdlr"]);
    // Full box: version and flags, pre-defined, then the handler type.
    matches!(hdlr, Ok(Some((s, _))) if init.get(s + 8..s + 12) == Some(b"vide".as_slice()))
}

pub fn parse_init(buf: &[u8]) -> Result<Init> {
    let mut init = Init::default();
    let (s, e) = find(buf, 0, buf.len(), &[b"moov", b"trak", b"mdia", b"mdhd"])?.ok_or_else(|| bad("no mdhd"))?;
    init.timescale = if buf[s] == 0 { u32_at(buf, s + 12)? } else { u32_at(buf, s + 20)? };
    let _ = e;
    let (s, e) = find(buf, 0, buf.len(), &[b"moov", b"trak", b"mdia", b"minf", b"stbl", b"stsd"])?.ok_or_else(|| bad("no stsd"))?;
    if let Some((kind, _, body, _)) = boxes(buf, s + 8, e)?.first().copied() {
        init.codec = String::from_utf8_lossy(&kind).into_owned();
        // VisualSampleEntry: 6 reserved + 2 index + 16 pre-defined, then width, height.
        init.width = u16::from_be_bytes([buf[body + 24], buf[body + 25]]);
        init.height = u16::from_be_bytes([buf[body + 26], buf[body + 27]]);
    }
    // The sample entry may carry the encoder's first (smaller) size; the track header has the
    // display size the frames actually use (seen: 320×180 vs 640×360).
    if let Some((s, _)) = find(buf, 0, buf.len(), &[b"moov", b"trak", b"tkhd"])? {
        let at = s + 4 + if buf[s] == 1 { 32 } else { 20 } + 8 + 8 + 36;
        let (w, h) = (u32_at(buf, at)? >> 16, u32_at(buf, at + 4)? >> 16);
        if w > 0 && h > 0 {
            (init.width, init.height) = (w as u16, h as u16);
        }
    }
    if let Some((s, _)) = find(buf, 0, buf.len(), &[b"moov", b"mvex", b"trex"])? {
        (init.default_duration, init.default_size, init.default_flags) = (u32_at(buf, s + 12)?, u32_at(buf, s + 16)?, u32_at(buf, s + 20)?);
    }
    if init.timescale == 0 {
        return Err(bad("zero timescale"));
    }
    Ok(init)
}

const NON_SYNC: u32 = 0x0001_0000;

/// All samples of every `moof`/`mdat` pair in `buf` (one media segment); data stays in `buf`.
pub fn parse_fragment(init: &Init, buf: &[u8]) -> Result<Vec<Sample>> {
    let mut samples = Vec::new();
    for (kind, moof, body, end) in boxes(buf, 0, buf.len())? {
        if &kind != b"moof" {
            continue;
        }
        let traf = find(buf, body, end, &[b"traf"])?.ok_or_else(|| bad("no traf"))?;
        let (mut base, mut dur, mut size, mut flags) = (moof as u64, init.default_duration, init.default_size, init.default_flags);
        let mut t = 0i64;
        for (k, _, b, _) in boxes(buf, traf.0, traf.1)? {
            match &k {
                b"tfhd" => {
                    let f = u32_at(buf, b)? & 0xff_ffff;
                    let mut at = b + 8;
                    if f & 0x1 != 0 {
                        base = u64_at(buf, at)?;
                        at += 8;
                    }
                    at += if f & 0x2 != 0 { 4 } else { 0 };
                    if f & 0x8 != 0 {
                        dur = u32_at(buf, at)?;
                        at += 4;
                    }
                    if f & 0x10 != 0 {
                        size = u32_at(buf, at)?;
                        at += 4;
                    }
                    if f & 0x20 != 0 {
                        flags = u32_at(buf, at)?;
                    }
                }
                b"tfdt" => {
                    t = if buf[b] == 1 {
                        u64_at(buf, b + 4)? as i64
                    } else {
                        u32_at(buf, b + 4)? as i64
                    }
                }
                b"trun" => trun(buf, b, (base, dur, size, flags), &mut t, &mut samples)?,
                _ => {}
            }
        }
    }
    Ok(samples)
}

fn trun(buf: &[u8], b: usize, (base, dur, size, flags): (u64, u32, u32, u32), t: &mut i64, out: &mut Vec<Sample>) -> Result<()> {
    let f = u32_at(buf, b)? & 0xff_ffff;
    let count = u32_at(buf, b + 4)?;
    let mut at = b + 8;
    let mut data = base as usize;
    if f & 0x1 != 0 {
        data = (base as i64 + u32_at(buf, at)? as i32 as i64) as usize;
        at += 4;
    }
    let first_flags = if f & 0x4 != 0 {
        at += 4;
        Some(u32_at(buf, at - 4)?)
    } else {
        None
    };
    for i in 0..count {
        let mut next = |on: bool, default: u32| -> Result<u32> {
            if !on {
                return Ok(default);
            }
            at += 4;
            u32_at(buf, at - 4)
        };
        let d = next(f & 0x100 != 0, dur)?;
        let s = next(f & 0x200 != 0, size)?;
        let fl = next(f & 0x400 != 0, if i == 0 { first_flags.unwrap_or(flags) } else { flags })?;
        let cts = next(f & 0x800 != 0, 0)? as i32 as i64;
        buf.get(data..data + s as usize).ok_or_else(|| bad("sample outside mdat"))?;
        out.push(Sample {
            pts: *t + cts,
            duration: d,
            key: fl & NON_SYNC == 0,
            offset: data,
            len: s as usize,
        });
        data += s as usize;
        *t += d as i64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_is_told_by_the_handler_not_the_playlist() {
        let video = include_bytes!("../../tests/fixtures/t1/v/init.mp4");
        let audio = include_bytes!("../../tests/fixtures/t1/a/init.mp4");
        assert!(is_video(video));
        assert!(!is_video(audio), "AAC behind a 640x480 label is not a camera");
        assert!(!is_video(b"not an mp4"));
    }
}
