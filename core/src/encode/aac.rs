//! AAC in .m4a. There is no pure-Rust AAC encoder: on macOS we use the system AudioToolbox
//! (always present, no extra install); elsewhere we pipe PCM into the system `ffmpeg`.

use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use super::{Encoder, Spec, enc_err};
use crate::{Error, Result};

pub fn create(spec: Spec, path: &Path) -> Result<Box<dyn Encoder>> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(super::aac_mac::AudioToolbox::new(spec, path)?))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(Box::new(Ffmpeg::new(spec, path)?))
    }
}

/// Fallback: `ffmpeg -f f32le … -c:a aac`.
pub struct Ffmpeg {
    child: Child,
}

impl Ffmpeg {
    #[allow(dead_code)]
    pub fn new(spec: Spec, path: &Path) -> Result<Self> {
        let child = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "f32le"])
            .args(["-ar", &spec.rate.to_string(), "-ac", &spec.channels.to_string(), "-i", "pipe:0"])
            .args(["-c:a", "aac", "-b:a", &format!("{}k", spec.kbps)])
            .arg(path)
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|_| {
                Error::Usage("--aac needs ffmpeg on this system (https://ffmpeg.org/download.html); or use the default MP3 / --opus".into())
            })?;
        Ok(Self { child })
    }
}

impl Encoder for Ffmpeg {
    fn write(&mut self, pcm: &[f32]) -> Result<()> {
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        Ok(self.child.stdin.as_mut().expect("piped").write_all(&bytes)?)
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        drop(self.child.stdin.take());
        let status = self.child.wait()?;
        status
            .success()
            .then_some(())
            .ok_or_else(|| enc_err(format!("ffmpeg exited with {status}")))
    }
}
