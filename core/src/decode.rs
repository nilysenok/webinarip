//! Decoding one track: its cached fMP4 pieces (init + segments) are read as one continuous
//! forward-only stream — `init + fragments` is a valid MP4 — and decoded to 48 kHz stereo f32.
//! Packet timestamps come from the fragments themselves, so a track can start mid-recording
//! (for `--from`) and still land at the exact spot on the timeline.

use std::path::PathBuf;
use std::sync::Arc;

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

pub use crate::stream::{Gate, StreamSource};
use crate::{Error, Result};

pub const RATE: u32 = 48_000;

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
