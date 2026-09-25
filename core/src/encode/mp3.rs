//! MP3 via LAME (compiled from source and linked statically — no runtime dependency).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use mp3lame_encoder::{Bitrate, Builder, FlushNoGap, Id3Tag, InterleavedPcm, MonoPcm, Quality};

use super::{Encoder, Spec, enc_err};
use crate::Result;

pub struct Mp3 {
    enc: mp3lame_encoder::Encoder,
    out: BufWriter<File>,
    buf: Vec<u8>,
    channels: u16,
}

fn bitrate(kbps: u32) -> Bitrate {
    match kbps {
        0..=24 => Bitrate::Kbps24,
        25..=32 => Bitrate::Kbps32,
        33..=64 => Bitrate::Kbps64,
        65..=96 => Bitrate::Kbps96,
        97..=128 => Bitrate::Kbps128,
        _ => Bitrate::Kbps192,
    }
}

impl Mp3 {
    pub fn new(spec: Spec, path: &Path, title: &str) -> Result<Self> {
        let mut b = Builder::new().ok_or_else(|| enc_err("LAME init failed"))?;
        b.set_num_channels(spec.channels as u8).map_err(enc_err)?;
        b.set_sample_rate(spec.rate).map_err(enc_err)?;
        b.set_brate(bitrate(spec.kbps)).map_err(enc_err)?;
        b.set_quality(Quality::Ok).map_err(enc_err)?;
        let tag = Id3Tag {
            title: title.as_bytes(),
            artist: &[],
            album: &[],
            album_art: &[],
            year: &[],
            comment: b"webinarip",
        };
        b.set_id3_tag(tag).map_err(|e| enc_err(format!("{e:?}")))?;
        Ok(Self {
            enc: b.build().map_err(enc_err)?,
            out: BufWriter::new(File::create(path)?),
            buf: Vec::new(),
            channels: spec.channels,
        })
    }
}

impl Encoder for Mp3 {
    fn write(&mut self, pcm: &[f32]) -> Result<()> {
        self.buf.clear();
        let frames = pcm.len() / self.channels as usize;
        self.buf.reserve(mp3lame_encoder::max_required_buffer_size(frames));
        if self.channels == 1 {
            self.enc.encode_to_vec(MonoPcm(pcm), &mut self.buf).map_err(enc_err)?;
        } else {
            self.enc.encode_to_vec(InterleavedPcm(pcm), &mut self.buf).map_err(enc_err)?;
        }
        Ok(self.out.write_all(&self.buf)?)
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        self.buf.clear();
        self.buf.reserve(7200);
        self.enc.flush_to_vec::<FlushNoGap>(&mut self.buf).map_err(enc_err)?;
        self.out.write_all(&self.buf)?;
        Ok(self.out.flush()?)
    }
}
