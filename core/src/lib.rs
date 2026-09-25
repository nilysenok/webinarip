//! Download webinar recordings that are published as HLS: fetch only the audio rendition of
//! every participant, mix the parallel tracks on one timeline, encode to MP3 / Opus / AAC / WAV.
//!
//! Use it only for recordings you have the rights to.

pub mod cache;
pub mod decode;
pub mod deliver;
pub mod dsp;
pub mod encode;
pub mod engine;
pub mod error;
pub mod export;
pub mod fetch;
mod hedge;
pub mod hls;
pub mod http;
pub mod job;
pub mod limiter;
pub mod mix;
pub mod options;
pub mod paths;
pub mod plan;
pub mod progress;
pub mod record;
pub mod render;
mod rush;
pub mod stream;
pub mod timefmt;
pub mod video;
mod worker;

pub use error::{Error, Result};

/// Hard ceiling of simultaneous connections to the media server. Users can go lower, never
/// higher: beyond ~320 connections the server starts dropping them (85–88% errors at 512).
pub const HARD_CAP: usize = 256;
