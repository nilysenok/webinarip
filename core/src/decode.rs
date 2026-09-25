//! Decoding one track: its cached fMP4 pieces (init + segments) are read as one continuous
//! forward-only stream — `init + fragments` is a valid MP4 — and decoded to 48 kHz stereo f32.
//! Packet timestamps come from the fragments themselves, so a track can start mid-recording
//! (for `--from`) and still land at the exact spot on the timeline.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Duration;

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;

use crate::{Error, Result};

pub const RATE: u32 = 48_000;

/// Download state shared with decoders that read files while they are still arriving.
#[derive(Default)]
pub struct Gate {
    pub finished: AtomicBool,
    pub failed: AtomicBool,
}

/// The track's files read back to back as one forward-only stream. A file that is not on
/// disk yet is waited for: segments are written atomically, so once a file is visible it is
/// complete. This lets decoding and mixing run while the download is still going.
pub struct StreamSource {
    files: Vec<PathBuf>,
    next: usize,
    open: Option<File>,
    gate: Arc<Gate>,
}

impl StreamSource {
    pub fn new(files: Vec<PathBuf>, gate: Arc<Gate>) -> Self {
        Self {
            files,
            next: 0,
            open: None,
            gate,
        }
    }

    fn open_next(&mut self) -> std::io::Result<bool> {
        let Some(path) = self.files.get(self.next) else { return Ok(false) };
        loop {
            match File::open(path) {
                Ok(f) => {
                    self.open = Some(f);
                    self.next += 1;
                    return Ok(true);
                }
                Err(_) if self.gate.failed.load(Relaxed) => return Err(std::io::Error::other("download failed")),
                Err(e) if self.gate.finished.load(Relaxed) => return Err(e),
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }
}

impl Read for StreamSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if let Some(f) = &mut self.open {
                let n = f.read(buf)?;
                if n > 0 || buf.is_empty() {
                    return Ok(n);
                }
                self.open = None;
            }
            if !self.open_next()? {
                return Ok(0);
            }
        }
    }
}

impl Seek for StreamSource {
    fn seek(&mut self, _: SeekFrom) -> std::io::Result<u64> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "forward-only stream"))
    }
}

impl MediaSource for StreamSource {
    fn is_seekable(&self) -> bool {
        false
    }
    fn byte_len(&self) -> Option<u64> {
        None
    }
}

pub struct TrackDecoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track: u32,
    /// Timestamp units → 48 kHz frames.
    tb: (u64, u64),
    buf: Vec<f32>,
    out: Vec<f32>,
}

fn dec_err(e: impl std::fmt::Display) -> Error {
    Error::Decode(e.to_string())
}

impl TrackDecoder {
    pub fn open(paths: Vec<PathBuf>, gate: Arc<Gate>) -> Result<Self> {
        let mss = MediaSourceStream::new(Box::new(StreamSource::new(paths, gate)), Default::default());
        let mut hint = Hint::new();
        hint.with_extension("mp4");
        let format = symphonia::default::get_probe()
            .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
            .map_err(dec_err)?;
        let track = format.default_track(TrackType::Audio).ok_or_else(|| dec_err("no audio track"))?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| dec_err("not audio"))?;
        if params.sample_rate.is_some_and(|r| r != RATE) {
            return Err(dec_err(format!(
                "sample rate {:?} Hz, only {RATE} Hz is supported",
                params.sample_rate
            )));
        }
        let tb = track
            .time_base
            .map(|t| (t.numer.get() as u64, t.denom.get() as u64))
            .unwrap_or((1, RATE as u64));
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(dec_err)?;
        let id = track.id;
        Ok(Self {
            format,
            decoder,
            track: id,
            tb,
            buf: Vec::new(),
            out: Vec::new(),
        })
    }

    /// Next decoded chunk: (position in 48 kHz frames from the track start, interleaved stereo).
    pub fn next_chunk(&mut self) -> Result<Option<(i64, &[f32])>> {
        loop {
            let Some(pkt) = self.format.next_packet().map_err(dec_err)? else {
                return Ok(None);
            };
            if pkt.track_id != self.track {
                continue;
            }
            let at = pkt.pts.get() * (self.tb.0 * RATE as u64) as i64 / self.tb.1 as i64;
            let decoded = match self.decoder.decode(&pkt) {
                Ok(d) => d,
                Err(symphonia::core::errors::Error::DecodeError(_)) => continue, // one bad frame ≠ a dead track
                Err(e) => return Err(dec_err(e)),
            };
            let ch = decoded.spec().channels().count().max(1);
            self.buf.resize(decoded.samples_interleaved(), 0.0);
            decoded.copy_to_slice_interleaved(&mut self.buf);
            self.out.clear();
            match ch {
                1 => self.out.extend(self.buf.iter().flat_map(|&s| [s, s])),
                2 => self.out.extend_from_slice(&self.buf),
                _ => self.out.extend(self.buf.chunks_exact(ch).flat_map(|f| [f[0], f[1]])),
            }
            return Ok(Some((at, &self.out)));
        }
    }
}
