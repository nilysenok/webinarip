//! 16-bit PCM WAV.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use hound::{SampleFormat, WavSpec, WavWriter};

use super::{Encoder, Spec, enc_err};
use crate::Result;

pub struct Wav(WavWriter<BufWriter<File>>);

impl Wav {
    pub fn new(spec: Spec, path: &Path) -> Result<Self> {
        let s = WavSpec {
            channels: spec.channels,
            sample_rate: spec.rate,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        Ok(Self(WavWriter::create(path, s).map_err(enc_err)?))
    }
}

impl Encoder for Wav {
    fn write(&mut self, pcm: &[f32]) -> Result<()> {
        for &s in pcm {
            self.0
                .write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                .map_err(enc_err)?;
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<()> {
        self.0.finalize().map_err(enc_err)
    }
}
