//! Opus in Ogg (RFC 7845), pure Rust: `opus-rs` for the codec, `ogg` for pages.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use ogg::writing::{PacketWriteEndInfo, PacketWriter};
use opus_rs::{Application, OpusEncoder};

use super::{Encoder, Spec, enc_err};
use crate::Result;

const SERIAL: u32 = 0x7765_6269;
/// Encoder look-ahead at 48 kHz that players skip at the start.
const PRE_SKIP: u16 = 312;

pub struct OggOpus {
    enc: OpusEncoder,
    ogg: PacketWriter<'static, BufWriter<File>>,
    frame: usize,
    channels: usize,
    pending: Vec<f32>,
    packet: Vec<u8>,
    /// Granule position: always counted in 48 kHz samples.
    granule: u64,
    step: u64,
    held: Option<Vec<u8>>,
}

fn head(channels: u8, rate: u32) -> Vec<u8> {
    let mut h = b"OpusHead".to_vec();
    h.extend_from_slice(&[1, channels]);
    h.extend_from_slice(&PRE_SKIP.to_le_bytes());
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&[0, 0, 0]); // output gain, mapping family 0
    h
}

fn tags(title: &str) -> Vec<u8> {
    let vendor = b"webinarip";
    let comment = format!("TITLE={title}");
    let mut t = b"OpusTags".to_vec();
    t.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    t.extend_from_slice(vendor);
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&(comment.len() as u32).to_le_bytes());
    t.extend_from_slice(comment.as_bytes());
    t
}

impl OggOpus {
    pub fn new(spec: Spec, path: &Path, title: &str) -> Result<Self> {
        let app = Application::Audio;
        let mut enc = OpusEncoder::new(spec.rate as i32, spec.channels as usize, app).map_err(enc_err)?;
        enc.bitrate_bps = (spec.kbps * 1000) as i32;
        enc.use_cbr = true;
        let mut ogg = PacketWriter::new(BufWriter::new(File::create(path)?));
        ogg.write_packet(head(spec.channels as u8, spec.rate), SERIAL, PacketWriteEndInfo::EndPage, 0)?;
        ogg.write_packet(tags(title), SERIAL, PacketWriteEndInfo::EndPage, 0)?;
        let frame = spec.rate as usize / 50; // 20 ms
        Ok(Self {
            enc,
            ogg,
            frame,
            channels: spec.channels as usize,
            pending: Vec::new(),
            packet: vec![0; 4000],
            granule: 0,
            step: 960,
            held: None,
        })
    }

    fn encode_frame(&mut self, pcm: &[f32]) -> Result<()> {
        let n = self.enc.encode(pcm, self.frame, &mut self.packet).map_err(enc_err)?;
        // Hold one packet back so the very last one can be flagged as end of stream.
        if let Some(prev) = self.held.replace(self.packet[..n].to_vec()) {
            self.granule += self.step;
            self.ogg
                .write_packet(prev, SERIAL, PacketWriteEndInfo::NormalPacket, self.granule)?;
        }
        Ok(())
    }
}

impl Encoder for OggOpus {
    fn write(&mut self, pcm: &[f32]) -> Result<()> {
        self.pending.extend_from_slice(pcm);
        let size = self.frame * self.channels;
        let mut start = 0;
        while self.pending.len() - start >= size {
            let chunk: Vec<f32> = self.pending[start..start + size].to_vec();
            self.encode_frame(&chunk)?;
            start += size;
        }
        self.pending.drain(..start);
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        let size = self.frame * self.channels;
        if !self.pending.is_empty() {
            let mut last = std::mem::take(&mut self.pending);
            last.resize(size, 0.0);
            self.encode_frame(&last)?;
        }
        if let Some(last) = self.held.take() {
            self.granule += self.step;
            self.ogg.write_packet(last, SERIAL, PacketWriteEndInfo::EndStream, self.granule)?;
        }
        Ok(())
    }
}
