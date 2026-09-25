//! Mixing and encoding a planned download into one file, on blocking threads:
//! one decoder thread per track, the mixer, and the encoder — all running concurrently,
//! and concurrently with the download itself.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use crate::decode::{Gate, RATE, TrackDecoder};
use crate::dsp::Downsampler;
use crate::encode::{self, Format, Quality};
use crate::job::Planned;
use crate::mix::{ChunkSource, Input, mix};
use crate::progress::Progress;
use crate::{Error, Result};

pub(crate) struct Render {
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

impl Render {
    pub fn write_file(self, path: &Path, title: &str, prog: &Progress, gate: Arc<Gate>) -> Result<()> {
        let Render {
            planned,
            from,
            to,
            format,
            quality,
        } = self;
        let frame = |secs: f64| ((secs - from) * RATE as f64).round() as i64;
        let inputs = planned
            .into_iter()
            .map(|p| {
                let gate = gate.clone();
                Input {
                    offset: frame(p.track.start),
                    starts_at: frame(p.first) - RATE as i64, // 1 s margin for rounded EXTINF
                    open: Box::new(move || {
                        Ok(Box::new(Decoded {
                            dec: TrackDecoder::open(p.files, gate)?,
                            carry: None,
                        }) as Box<dyn ChunkSource>)
                    }),
                }
            })
            .collect();
        encode_mix(inputs, to - from, format, quality, path, title, prog)
    }
}

fn encode_mix(inputs: Vec<Input>, seconds: f64, format: Format, quality: Quality, path: &Path, title: &str, prog: &Progress) -> Result<()> {
    let mut enc = encode::create(format, format.spec(quality), path, title)?;
    // The encoder runs on its own thread: mixing and encoding overlap instead of adding up.
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(4);
    let encoder = std::thread::spawn(move || -> Result<()> {
        for block in rx {
            enc.write(&block)?;
        }
        enc.finish()
    });
    let mut down = (quality == Quality::Low).then(Downsampler::default);
    let total = (seconds * RATE as f64).round() as u64;
    let send = |v: Vec<f32>| tx.send(v).map_err(|_| Error::Encode("encoder stopped".into()));
    let mixed = mix(
        inputs,
        total,
        &mut |block| match down.as_mut() {
            Some(d) => {
                let mut mono = Vec::with_capacity(block.len() / 6 + 1);
                d.process(block, &mut mono);
                send(mono)
            }
            None => send(block.to_vec()),
        },
        &|frames| prog.mixed_ms.store(frames * 1000 / RATE as u64, Relaxed),
    );

    drop(tx);
    let encoded = encoder.join().map_err(|_| Error::Encode("encoder thread panicked".into()))?;
    mixed.and(encoded)
}
