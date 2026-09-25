//! Download webinar recordings that are published as HLS: fetch only the audio rendition of
//! every participant, mix the parallel tracks on one timeline, encode to MP3 / Opus / AAC / WAV.
//!
//! Use it only for recordings you have the rights to.

pub mod cache;
pub mod decode;
pub mod dsp;
pub mod encode;
pub mod error;
pub mod fetch;
pub mod hls;
pub mod http;
pub mod job;
pub mod limiter;
pub mod mix;
pub mod options;
mod paths;
pub mod progress;
pub mod record;
mod render;
pub mod timefmt;

pub use error::{Error, Result};

/// Hard ceiling of simultaneous connections to the media server. Users can go lower, never
/// higher: beyond ~320 connections the server starts dropping them (85–88% errors at 512).
pub const HARD_CAP: usize = 256;
