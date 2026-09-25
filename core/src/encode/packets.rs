//! Opus packets kept in memory with their timestamps — the audio track of the WebM videos.

use std::sync::{Arc, Mutex};

use opus_rs::{Application, OpusEncoder};

use super::opus::{PRE_SKIP, head};
use super::{Encoder, enc_err};
use crate::Result;

/// (milliseconds from the start of the mix, packet)
pub type Packet = (i64, Vec<u8>);
pub type Packets = Arc<Mutex<Vec<Packet>>>;

pub struct OpusPackets {
    enc: OpusEncoder,
    channels: usize,
    pending: Vec<f32>,
    buf: Vec<u8>,
    frame_no: i64,
    out: Packets,
}

pub const FRAME: usize = 960; // 20 ms at 48 kHz

impl OpusPackets {
    pub fn new(channels: u8, kbps: u32, out: Packets) -> Result<Self> {
        let mut enc = OpusEncoder::new(48_000, channels as usize, Application::Audio).map_err(enc_err)?;
        enc.bitrate_bps = (kbps * 1000) as i32;
        enc.use_cbr = true;
        Ok(Self {
            enc,
            channels: channels as usize,
            pending: Vec::new(),
            buf: vec![0; 4000],
            frame_no: 0,
            out,
        })
    }

    /// CodecPrivate for a WebM Opus track and the encoder delay in nanoseconds.
    pub fn head(channels: u8) -> (Vec<u8>, u64) {
        (head(channels, 48_000), PRE_SKIP as u64 * 1_000_000_000 / 48_000)
    }

    fn frame(&mut self, pcm: &[f32]) -> Result<()> {
        let n = self.enc.encode(pcm, FRAME, &mut self.buf).map_err(enc_err)?;
        self.out.lock().unwrap().push((self.frame_no * 20, self.buf[..n].to_vec()));
        self.frame_no += 1;
        Ok(())
    }
}

impl Encoder for OpusPackets {
    fn write(&mut self, pcm: &[f32]) -> Result<()> {
        self.pending.extend_from_slice(pcm);
        let size = FRAME * self.channels;
        let mut start = 0;
        while self.pending.len() - start >= size {
            let chunk = self.pending[start..start + size].to_vec();
            self.frame(&chunk)?;
            start += size;
        }
        self.pending.drain(..start);
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        if !self.pending.is_empty() {
            let mut last = std::mem::take(&mut self.pending);
            last.resize(FRAME * self.channels, 0.0);
            self.frame(&last)?;
        }
        Ok(())
    }
}
