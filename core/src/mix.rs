//! Streaming mixer: every track is decoded on its own thread and summed onto one timeline
//! at its `relativeTime`, one-second block at a time, so memory stays flat for any length.
//!
//! The sum is plain addition — like ffmpeg's `amix` with `normalize=0`. Normalizing would
//! divide every voice by the number of tracks and make the result quiet. Peaks are handled
//! afterwards by a look-ahead limiter.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, sync_channel};

use crate::dsp::{PeakLimiter, add_into};
use crate::{Error, Result};

pub const BLOCK_FRAMES: usize = 48_000;

/// Decoded audio: (position in 48 kHz frames relative to the track start, interleaved stereo).
pub trait ChunkSource: Send {
    fn next_chunk(&mut self) -> Result<Option<(i64, Vec<f32>)>>;
}

pub struct Input {
    /// Where frame 0 of this track lands on the output timeline (may be negative).
    pub offset: i64,
    /// Output frame before which this input surely has no audio: the mixer does not wait
    /// for it until then (its data may still be downloading).
    pub starts_at: i64,
    pub open: Box<dyn FnOnce() -> Result<Box<dyn ChunkSource>> + Send>,
}

type Chunk = Result<(i64, Vec<f32>)>;

struct Lane {
    rx: Receiver<Chunk>,
    starts_at: i64,
    pending: VecDeque<(i64, Vec<f32>)>,
    done: bool,
}

impl Lane {
    fn end(&self) -> i64 {
        self.pending.back().map_or(i64::MIN, |(s, d)| s + (d.len() / 2) as i64)
    }

    /// Pulls chunks until the lane covers `until` or runs dry.
    fn fill(&mut self, until: i64) -> Result<()> {
        while !self.done && self.end() < until {
            match self.rx.recv() {
                Ok(chunk) => self.pending.push_back(chunk?),
                Err(_) => self.done = true,
            }
        }
        Ok(())
    }
}

fn spawn(input: Input) -> Lane {
    let (tx, rx) = sync_channel::<Chunk>(4);
    let starts_at = input.starts_at;
    std::thread::spawn(move || {
        let mut src = match (input.open)() {
            Ok(s) => s,
            Err(e) => return drop(tx.send(Err(e))),
        };
        loop {
            match src.next_chunk() {
                Ok(Some((at, pcm))) => {
                    if tx.send(Ok((input.offset + at, pcm))).is_err() {
                        return;
                    }
                }
                Ok(None) => return,
                Err(e) => return drop(tx.send(Err(e))),
            }
        }
    });
    Lane {
        rx,
        starts_at,
        pending: VecDeque::new(),
        done: false,
    }
}

/// Mixes `total_frames` of output and hands limited stereo blocks to `sink`.
pub fn mix(inputs: Vec<Input>, total_frames: u64, sink: &mut dyn FnMut(&[f32]) -> Result<()>, progress: &dyn Fn(u64)) -> Result<()> {
    if inputs.is_empty() {
        return Err(Error::Usage("nothing to mix: no audio tracks in the selected range".into()));
    }
    let mut lanes: Vec<Lane> = inputs.into_iter().map(spawn).collect();
    let mut limiter = PeakLimiter::new(0.95);
    let (mut block, mut limited) = (Vec::with_capacity(BLOCK_FRAMES * 2), Vec::with_capacity(BLOCK_FRAMES * 2));
    let mut b0 = 0i64;
    while (b0 as u64) < total_frames {
        let b1 = (b0 + BLOCK_FRAMES as i64).min(total_frames as i64);
        block.clear();
        block.resize(((b1 - b0) * 2) as usize, 0.0);
        for lane in lanes.iter_mut().filter(|l| b1 > l.starts_at) {
            lane.fill(b1)?;
            for (cs, data) in &lane.pending {
                let ce = cs + (data.len() / 2) as i64;
                let (s, e) = ((*cs).max(b0), ce.min(b1));
                if s < e {
                    let dst = &mut block[((s - b0) * 2) as usize..((e - b0) * 2) as usize];
                    add_into(dst, &data[((s - cs) * 2) as usize..]);
                }
            }
            while lane.pending.front().is_some_and(|(cs, d)| cs + (d.len() / 2) as i64 <= b1) {
                lane.pending.pop_front();
            }
        }
        limited.clear();
        limiter.process(&block, &mut limited);
        sink(&limited)?;
        progress(b1 as u64);
        b0 = b1;
    }
    limited.clear();
    limiter.flush(&mut limited);
    sink(&limited)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tone(Vec<(i64, Vec<f32>)>);
    impl ChunkSource for Tone {
        fn next_chunk(&mut self) -> Result<Option<(i64, Vec<f32>)>> {
            Ok((!self.0.is_empty()).then(|| self.0.remove(0)))
        }
    }

    fn input(offset: i64, frames: usize, level: f32) -> Input {
        let chunks = (0..frames)
            .step_by(1000)
            .map(|at| (at as i64, vec![level; 2 * 1000.min(frames - at)]))
            .collect();
        Input {
            offset,
            starts_at: offset,
            open: Box::new(move || Ok(Box::new(Tone(chunks)) as Box<dyn ChunkSource>)),
        }
    }

    fn run(inputs: Vec<Input>, total: u64) -> Vec<f32> {
        let mut out = Vec::new();
        mix(
            inputs,
            total,
            &mut |b| {
                out.extend_from_slice(b);
                Ok(())
            },
            &|_| (),
        )
        .unwrap();
        out
    }

    #[test]
    fn places_tracks_at_their_offsets_and_sums_without_normalizing() {
        let out = run(vec![input(0, 60_000, 0.2), input(50_000, 30_000, 0.3)], 100_000);
        assert_eq!(out.len(), 200_000);
        let at = |f: usize| out[f * 2];
        assert!((at(10_000) - 0.2).abs() < 1e-6);
        assert!((at(55_000) - 0.5).abs() < 1e-6); // overlap: 0.2 + 0.3, not their average
        assert!((at(70_000) - 0.3).abs() < 1e-6);
        assert_eq!(at(90_000), 0.0);
    }

    #[test]
    fn limits_peaks_and_negative_offsets_are_cut() {
        let out = run(vec![input(-5_000, 50_000, 0.8), input(0, 20_000, 0.8)], 50_000);
        assert_eq!(out.len(), 100_000);
        assert!(out.iter().all(|s| s.abs() <= 0.95 + 1e-6));
        assert!((out[2 * 1_000] - 0.95).abs() < 1e-3); // 0.8 + 0.8 held at the ceiling
        assert!((out[2 * 44_000] - 0.8).abs() < 0.01); // gain has recovered after the overlap
    }
}
