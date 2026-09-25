//! Extra outputs next to the mix: participants' videos (WebM, frames copied), their own audio
//! tracks in original quality (.m4a, bytes copied) and, on request, MP4 via the system ffmpeg.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::encode::packets::{OpusPackets, Packet};
use crate::paths::sanitize;
use crate::plan::Planned;
use crate::timefmt::hms;
use crate::video::{remux, webm::Audio};
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Made {
    Mix,
    Video { width: u16, height: u16 },
    Track,
}

/// One file of the result, placed on the output timeline (seconds from the range start).
#[derive(Debug, Clone)]
pub struct Piece {
    pub kind: Made,
    pub path: PathBuf,
    pub name: String,
    pub offset: f64,
    pub duration: f64,
}

fn file_name(p: &Planned, from: f64, ext: &str) -> String {
    let at = hms(p.first().max(from)).replace(':', "-");
    sanitize(&format!("{:02} {} {at}.{ext}", p.track.index, p.track.name))
}

/// One WebM per video track, each with the matching stretch of the mixed audio.
pub fn videos(planned: &[Planned], dir: &Path, (from, to): (f64, f64), audio: Option<(u8, &[Packet])>) -> Result<Vec<Piece>> {
    std::fs::create_dir_all(dir)?;
    let track_audio = audio.map(|(channels, _)| {
        let (head, delay_ns) = OpusPackets::head(channels);
        Audio { channels, head, delay_ns }
    });
    let mut out = Vec::new();
    for p in planned {
        let path = dir.join(file_name(p, from, "webm"));
        let files = p.files();
        // The video's own start on the timeline decides which audio packets belong to it.
        let slice = |first: f64, dur: f64| -> Vec<(i64, Vec<u8>)> {
            let Some((_, packets)) = audio else { return vec![] };
            let start_ms = ((p.track.start + first - from) * 1000.0).round() as i64;
            let end_ms = start_ms + (dur * 1000.0).round() as i64;
            packets
                .iter()
                .filter(|(t, _)| *t >= start_ms && *t < end_ms)
                .map(|(t, d)| (t - start_ms, d.clone()))
                .collect()
        };
        let window = (from - p.track.start, to - p.track.start);
        let r = remux(
            &files,
            &path,
            window,
            track_audio
                .as_ref()
                .map(|a| (a, &slice as &dyn Fn(f64, f64) -> Vec<(i64, Vec<u8>)>)),
        )?;
        let (width, height) = (r.width, r.height);
        out.push(Piece {
            kind: Made::Video { width, height },
            path,
            name: p.track.name.clone(),
            offset: p.track.start + r.first - from,
            duration: r.duration,
        });
    }
    Ok(out)
}

/// Each participant's own audio, bytes as they came from the server (AAC in MP4).
pub fn tracks(planned: &[Planned], dir: &Path, from: f64) -> Result<Vec<Piece>> {
    std::fs::create_dir_all(dir)?;
    let mut out = Vec::new();
    for p in planned {
        let path = dir.join(file_name(p, from, "m4a"));
        rebased_copy(&p.files(), &path)?;
        let duration = p.pieces.last().map_or(0.0, |x| x.end) - p.first();
        out.push(Piece {
            kind: Made::Track,
            path,
            name: p.track.name.clone(),
            offset: p.first() - from,
            duration,
        });
    }
    Ok(out)
}

/// Copies init + segments into one file whose times start at zero and whose header has the
/// real length — two passes, one segment in memory at a time. No sample is touched.
fn rebased_copy(files: &[PathBuf], out: &Path) -> Result<()> {
    use crate::video::fmp4;
    let mut init = std::fs::read(&files[0])?;
    let header = fmp4::parse_init(&init)?;
    let (mut first, mut total) = (i64::MAX, 0u64);
    for f in &files[1..] {
        let samples = fmp4::parse_fragment(&header, &std::fs::read(f)?)?;
        first = first.min(samples.iter().map(|s| s.pts).min().unwrap_or(i64::MAX));
        total += samples.iter().map(|s| s.duration as u64).sum::<u64>();
    }
    crate::video::patch::set_length(&mut init, total)?;
    let mut w = std::io::BufWriter::new(std::fs::File::create(out)?);
    std::io::Write::write_all(&mut w, &init)?;
    for f in &files[1..] {
        let mut seg = std::fs::read(f)?;
        crate::video::patch::shift(&mut seg, first.max(0) as u64)?;
        std::io::Write::write_all(&mut w, &seg)?;
    }
    std::io::Write::flush(&mut w)?;
    Ok(())
}

/// `--mp4`: H.264 + AAC for editors and QuickTime. Re-encodes the video — slow, and only via
/// the system ffmpeg; the WebM is replaced on success.
pub fn to_mp4(piece: &mut Piece) -> Result<()> {
    let out = piece.path.with_extension("mp4");
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(&piece.path)
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "23",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-movflags",
            "+faststart",
        ])
        .arg(&out)
        .status()
        .map_err(|_| Error::Usage("--mp4 needs ffmpeg on this system (https://ffmpeg.org/download.html); WebM needs nothing".into()))?;
    if !status.success() {
        return Err(Error::Encode(format!("ffmpeg exited with {status}")));
    }
    std::fs::remove_file(&piece.path)?;
    piece.path = out;
    Ok(())
}
