//! Video: participants' VP9 renditions rewrapped from fragmented MP4 into WebM with the mixed
//! audio as Opus — frames are copied, never re-encoded.

mod boxes;
pub mod ebml;
pub mod fmp4;
pub mod patch;
pub mod webm;

use std::path::{Path, PathBuf};

use crate::Result;
use webm::{AUDIO, Audio, VIDEO, WebmWriter};

#[derive(Debug, Clone)]
pub struct Remuxed {
    /// Presentation time of the first frame inside the track, seconds.
    pub first: f64,
    pub duration: f64,
    pub width: u16,
    pub height: u16,
    pub frames: usize,
}

/// Gives the audio for a video once its first frame (seconds into the track) and duration are
/// known: Opus packets as (milliseconds from the first frame, packet).
pub type AudioFor<'a> = &'a dyn Fn(f64, f64) -> Vec<(i64, Vec<u8>)>;

/// `files`: init piece first, then media segments in order.
/// `window`: seconds of the track to keep; the cut starts at the first key frame inside it.
/// Two passes over the segment files — timestamps first, then frames — so memory holds one
/// segment at a time, not the whole video.
pub fn remux(files: &[PathBuf], out: &Path, window: (f64, f64), audio: Option<(&Audio, AudioFor)>) -> Result<Remuxed> {
    let init = std::fs::read(&files[0])?;
    if !fmp4::is_video(&init) {
        return Err(crate::Error::Parse(format!("{}: no video track", files[0].display())));
    }
    let init = fmp4::parse_init(&init)?;
    let scale = init.timescale as f64;
    let (lo, hi) = ((window.0 * scale) as i64, (window.1 * scale) as i64);
    // Pass 1: which frames to keep — from the first key frame at or after `lo`, before `hi`.
    let mut meta: Vec<(usize, fmp4::Sample)> = Vec::new();
    for (i, f) in files[1..].iter().enumerate() {
        meta.extend(fmp4::parse_fragment(&init, &std::fs::read(f)?)?.into_iter().map(|s| (i, s)));
    }
    let start = meta.iter().position(|(_, s)| s.key && s.pts >= lo).unwrap_or(meta.len());
    meta.drain(..start);
    meta.retain(|(_, s)| s.pts < hi);
    let first = meta.iter().map(|(_, s)| s.pts).min().unwrap_or(0);
    let end = meta.iter().map(|(_, s)| s.pts + s.duration as i64).max().unwrap_or(first);
    let (first_s, duration) = (first as f64 / scale, (end - first) as f64 / scale);
    let ts = |pts: i64| (pts - first) * 1000 / init.timescale as i64;
    // Pass 2: write frames segment by segment, interleaved with the audio packets.
    let mut w = WebmWriter::create(out, (init.width, init.height), audio.map(|a| a.0))?;
    let packets = audio.map_or_else(Vec::new, |a| (a.1)(first_s, duration));
    let mut a = packets.iter().peekable();
    let mut last_video = i64::MIN; // WebM has millisecond timestamps: keep frames strictly increasing
    let mut segment: (usize, Vec<u8>) = (usize::MAX, Vec::new());
    for (i, s) in &meta {
        if segment.0 != *i {
            segment = (*i, std::fs::read(&files[1 + i])?);
        }
        let t = ts(s.pts).max(last_video + 1);
        while let Some((at, p)) = a.next_if(|(at, _)| *at < t) {
            w.block(AUDIO, *at, true, p)?;
        }
        w.block(VIDEO, t, s.key, s.data(&segment.1))?;
        last_video = t;
    }
    for (at, p) in a {
        w.block(AUDIO, *at, true, p)?;
    }
    w.finish(duration * 1000.0)?;
    Ok(Remuxed {
        first: first_s,
        duration,
        width: init.width,
        height: init.height,
        frames: meta.len(),
    })
}
