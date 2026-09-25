//! Mixing and encoding a planned download into one file, on blocking threads:
//! one decoder thread per track, the mixer, and the encoder — all running concurrently,
//! and concurrently with the download itself.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use crate::decode::{Gate, RATE, TrackDecoder};
use crate::dsp::Downsampler;
use crate::encode::packets::{OpusPackets, Packets};
use crate::encode::{self, Format, Quality};
use crate::mix::{ChunkSource, Input, mix};
use crate::plan::Planned;
use crate::progress::Progress;
use crate::{Error, Result};

/// What to render: planned tracks already cut to the range, and the output format.
pub struct Render {
    pub planned: Vec<Planned>,
    pub from: f64,
    pub to: f64,
    pub format: Format,
    pub quality: Quality,
}

/// Glues decoder frames (21 ms each) into ~0.5 s chunks so each track's thread can run
/// ahead of the mixer instead of handing it one tiny frame at a time.
struct Decoded {
    dec: TrackDecoder,
    carry: Option<(i64, Vec<f32>)>,
}

const CHUNK_FRAMES: usize = 24_000;

impl ChunkSource for Decoded {
    fn next_chunk(&mut self) -> Result<Option<(i64, Vec<f32>)>> {
        let (start, mut buf) = match self.carry.take() {
            Some(c) => c,
            None => match self.dec.next_chunk()? {
                Some((at, pcm)) => (at, pcm.to_vec()),
                None => return Ok(None),
            },
        };
        while buf.len() < CHUNK_FRAMES * 2 {
            let Some((at, pcm)) = self.dec.next_chunk()? else { break };
            if at != start + (buf.len() / 2) as i64 {
                self.carry = Some((at, pcm.to_vec())); // a gap: start a new chunk there
                break;
            }
            buf.extend_from_slice(pcm);
        }
        Ok(Some((start, buf)))
    }
}

/// Where a mix goes: a file in a format, or Opus packets in memory for the videos.
pub enum Target {
    File {
        path: PathBuf,
        title: String,
        format: Format,
        quality: Quality,
    },
    Packets {
        quality: Quality,
        out: Packets,
    },
}

impl Render {
    /// Mixes once and feeds every target. Blocks until done; `cancel` stops it with an error.
    pub fn write(self, targets: Vec<Target>, prog: &Progress, gate: Arc<Gate>, cancel: Arc<AtomicBool>) -> Result<()> {
        let Render { planned, from, to, .. } = self;
        let frame = |secs: f64| ((secs - from) * RATE as f64).round() as i64;
        let inputs = planned
            .into_iter()
            .map(|p| {
                let gate = gate.clone();
                Input {
                    offset: frame(p.track.start),
                    starts_at: frame(p.first()) - RATE as i64, // 1 s margin for rounded EXTINF
                    open: Box::new(move || {
                        Ok(Box::new(Decoded {
                            dec: TrackDecoder::open(p.files(), gate)?,
                            carry: None,
                        }) as Box<dyn ChunkSource>)
                    }),
                }
            })
            .collect();
        encode_mix(inputs, to - from, targets, prog, &cancel)
    }

    /// One output file (the command-line path).
    pub fn write_file(self, path: &Path, title: &str, prog: &Progress, gate: Arc<Gate>, cancel: Arc<AtomicBool>) -> Result<()> {
        let (format, quality) = (self.format, self.quality);
        let target = Target::File {
            path: path.to_path_buf(),
            title: title.to_owned(),
            format,
            quality,
        };
        self.write(vec![target], prog, gate, cancel)
    }
}

/// Per-target channel layout: 48 kHz stereo as mixed, 48 kHz mono, or 16 kHz mono.
enum Shape {
    Stereo,
    Mono,
    Speech16(Downsampler),
}

impl Shape {
    fn of(q: Quality) -> Self {
        match q {
            Quality::High => Shape::Stereo,
            Quality::Speech => Shape::Mono,
            Quality::Low => Shape::Speech16(Downsampler::default()),
        }
    }

    fn apply(&mut self, block: &[f32]) -> Vec<f32> {
        match self {
            Shape::Stereo => block.to_vec(),
            Shape::Mono => block.chunks_exact(2).map(|f| (f[0] + f[1]) * 0.5).collect(),
            Shape::Speech16(d) => {
                let mut out = Vec::with_capacity(block.len() / 6 + 1);
                d.process(block, &mut out);
                out
            }
        }
    }
}

type Lane = (std::sync::mpsc::SyncSender<Vec<f32>>, Shape, std::thread::JoinHandle<Result<()>>);

fn open(target: Target) -> Result<Lane> {
    let (mut enc, shape): (Box<dyn encode::Encoder>, Shape) = match target {
        Target::File {
            path,
            title,
            format,
            quality,
        } => (encode::create(format, format.spec(quality), &path, &title)?, Shape::of(quality)),
        Target::Packets { quality, out } => {
            // Videos always carry 48 kHz audio: the transcription shape falls back to mono speech.
            let q = if quality == Quality::High { Quality::High } else { Quality::Speech };
            let (channels, kbps) = if q == Quality::High { (2, 96) } else { (1, 48) };
            (Box::new(OpusPackets::new(channels, kbps, out)?), Shape::of(q))
        }
    };
    // Each encoder runs on its own thread: mixing and encoding overlap instead of adding up.
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(4);
    let handle = std::thread::spawn(move || -> Result<()> {
        for block in rx {
            enc.write(&block)?;
        }
        enc.finish()
    });
    Ok((tx, shape, handle))
}

fn encode_mix(inputs: Vec<Input>, seconds: f64, targets: Vec<Target>, prog: &Progress, cancel: &AtomicBool) -> Result<()> {
    let mut lanes = targets.into_iter().map(open).collect::<Result<Vec<_>>>()?;
    let total = (seconds * RATE as f64).round() as u64;
    let mixed = mix(
        inputs,
        total,
        &mut |block| {
            if cancel.load(Relaxed) {
                return Err(Error::Usage("cancelled".into()));
            }
            for (tx, shape, _) in lanes.iter_mut() {
                tx.send(shape.apply(block)).map_err(|_| Error::Encode("encoder stopped".into()))?;
            }
            Ok(())
        },
        &|frames| prog.mixed_ms.store(frames * 1000 / RATE as u64, Relaxed),
    );
    let mut encoded = Ok(());
    for (tx, _, handle) in lanes {
        drop(tx);
        let r = handle.join().map_err(|_| Error::Encode("encoder thread panicked".into()))?;
        encoded = encoded.and(r);
    }
    mixed.and(encoded)
}
