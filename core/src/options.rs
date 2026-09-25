//! What to download and how.

use std::path::PathBuf;

use crate::encode::{Format, Quality};
use crate::{cache, record};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    /// The mixed audio file only.
    Audio,
    /// Participants' videos (WebM with the mixed audio) only.
    Video,
    /// Both.
    Both,
}

/// Without `--tracks`, videos shorter than this are skipped (reconnects, a few seconds each).
pub const MIN_VIDEO_SECS: f64 = 30.0;

#[derive(Debug, Clone)]
pub struct Options {
    pub link: String,
    /// For private recordings. Kept in memory only.
    pub session_id: Option<String>,
    pub api_base: String,
    pub what: What,
    pub format: Format,
    pub quality: Quality,
    pub from: Option<f64>,
    pub to: Option<f64>,
    pub tracks: Option<String>,
    /// Highest video frame height; `None` = best available.
    pub video_height: Option<u32>,
    /// Each participant's own audio in original quality (.m4a, no re-encoding).
    pub separate: bool,
    /// FCPXML + EDL timeline of all the pieces (implies `separate`).
    pub multicam: bool,
    /// Convert videos to H.264/AAC MP4 with the system ffmpeg (slow: re-encodes).
    pub mp4: bool,
    pub out_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub connections: usize,
}

impl Options {
    pub fn new(link: impl Into<String>) -> Self {
        Self {
            link: link.into(),
            session_id: None,
            api_base: record::DEFAULT_API.into(),
            what: What::Audio,
            format: Format::Mp3,
            quality: Quality::Speech,
            from: None,
            to: None,
            tracks: None,
            video_height: None,
            separate: false,
            multicam: false,
            mp4: false,
            out_dir: PathBuf::from("."),
            cache_dir: cache::default_root(),
            connections: crate::HARD_CAP,
        }
    }

    pub fn wants_video(&self) -> bool {
        self.what != What::Audio
    }

    pub fn wants_mix_file(&self) -> bool {
        self.what != What::Video
    }
}
