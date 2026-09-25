//! Output encoders. All take interleaved f32 PCM in the rate/channels of their [`Spec`].

use std::path::Path;

use crate::Result;

mod aac;
#[cfg(target_os = "macos")]
mod aac_mac;
mod mp3;
mod opus;
mod wav;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Default: plays everywhere. LAME, statically linked.
    Mp3,
    /// Smallest files: Opus in Ogg, pure Rust.
    Opus,
    /// AAC in .m4a: AudioToolbox on macOS, system ffmpeg elsewhere.
    Aac,
    /// 16-bit PCM, for transcription tools.
    Wav,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    /// 48 kHz stereo.
    High,
    /// 16 kHz mono, low bitrate — speech and transcription.
    Low,
}

#[derive(Debug, Clone, Copy)]
pub struct Spec {
    pub rate: u32,
    pub channels: u16,
    pub kbps: u32,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Format::Mp3 => "mp3",
            Format::Opus => "opus",
            Format::Aac => "m4a",
            Format::Wav => "wav",
        }
    }

    pub fn spec(self, q: Quality) -> Spec {
        let (rate, channels) = match q {
            Quality::High => (48_000, 2),
            Quality::Low => (16_000, 1),
        };
        let kbps = match (self, q) {
            (Format::Mp3, Quality::High) => 128,
            (Format::Mp3, Quality::Low) => 32,
            (Format::Opus, Quality::High) => 96,
            (Format::Opus, Quality::Low) => 24,
            (Format::Aac, Quality::High) => 128,
            (Format::Aac, Quality::Low) => 32,
            (Format::Wav, _) => 0,
        };
        Spec { rate, channels, kbps }
    }
}

pub trait Encoder: Send {
    fn write(&mut self, pcm: &[f32]) -> Result<()>;
    fn finish(self: Box<Self>) -> Result<()>;
}

pub fn create(format: Format, spec: Spec, path: &Path, title: &str) -> Result<Box<dyn Encoder>> {
    Ok(match format {
        Format::Mp3 => Box::new(mp3::Mp3::new(spec, path, title)?),
        Format::Opus => Box::new(opus::OggOpus::new(spec, path, title)?),
        Format::Wav => Box::new(wav::Wav::new(spec, path)?),
        Format::Aac => aac::create(spec, path)?,
    })
}

pub(crate) fn enc_err(e: impl std::fmt::Display) -> crate::Error {
    crate::Error::Encode(e.to_string())
}
