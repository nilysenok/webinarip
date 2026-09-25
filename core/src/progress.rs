//! Progress counters shared between the job and whoever draws it (CLI, web UI).
//! Speed and ETA are computed here so every front end shows the same numbers.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Stage {
    Meta = 0,
    Download = 1,
    Mix = 2,
    Done = 3,
}

const RATE_WINDOW: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct Progress {
    stage: AtomicU8,
    pub seg_total: AtomicUsize,
    pub seg_done: AtomicUsize,
    pub seg_cached: AtomicUsize,
    pub bytes: AtomicU64,
    pub retries: AtomicUsize,
    pub hedges: AtomicUsize,
    pub http429: AtomicUsize,
    pub limit: AtomicUsize,
    /// Output seconds already mixed and encoded, in milliseconds.
    pub mixed_ms: AtomicU64,
    pub total_ms: AtomicU64,
    ewma_us: AtomicU64,
    latencies_ms: Mutex<Vec<u32>>,
    /// (done, total) per track slot.
    slots: Mutex<Vec<(usize, usize)>>,
    /// (when, downloaded bytes so far) over the last few seconds.
    samples: Mutex<VecDeque<(Instant, u64)>>,
}

impl Progress {
    pub fn stage(&self) -> Stage {
        match self.stage.load(Relaxed) {
            0 => Stage::Meta,
            1 => Stage::Download,
            2 => Stage::Mix,
            _ => Stage::Done,
        }
    }

    pub fn set_stage(&self, s: Stage) {
        self.stage.store(s as u8, Relaxed);
    }

    pub fn set_slots(&self, totals: &[usize]) {
        *self.slots.lock().unwrap() = totals.iter().map(|&t| (0, t)).collect();
        self.seg_total.store(totals.iter().sum(), Relaxed);
    }

    /// (done, total) segments per track slot.
    pub fn slots(&self) -> Vec<(usize, usize)> {
        self.slots.lock().unwrap().clone()
    }

    /// A planned segment was dropped from the download (outside the chosen range).
    pub fn unplan(&self, slot: usize) {
        if let Some(s) = self.slots.lock().unwrap().get_mut(slot) {
            s.1 = s.1.saturating_sub(1);
        }
        self.seg_total.fetch_sub(1, Relaxed);
    }

    fn slot_done(&self, slot: usize) {
        if let Some(s) = self.slots.lock().unwrap().get_mut(slot) {
            s.0 += 1;
        }
    }

    pub fn cached(&self, bytes: u64, slot: usize) {
        self.seg_cached.fetch_add(1, Relaxed);
        self.seg_done.fetch_add(1, Relaxed);
        self.bytes.fetch_add(bytes, Relaxed);
        self.slot_done(slot);
    }

    pub fn downloaded(&self, bytes: u64, took: Duration, slot: usize) {
        self.seg_done.fetch_add(1, Relaxed);
        let total = self.bytes.fetch_add(bytes, Relaxed) + bytes;
        self.slot_done(slot);
        let us = took.as_micros() as u64;
        let old = self.ewma_us.load(Relaxed);
        self.ewma_us.store(if old == 0 { us } else { (old * 7 + us) / 8 }, Relaxed);
        self.latencies_ms.lock().unwrap().push(took.as_millis() as u32);
        let mut s = self.samples.lock().unwrap();
        s.push_back((Instant::now(), total));
        while s.front().is_some_and(|(t, _)| t.elapsed() > RATE_WINDOW) {
            s.pop_front();
        }
    }

    /// Download speed over the last few seconds, bytes per second.
    pub fn speed(&self) -> f64 {
        let s = self.samples.lock().unwrap();
        let (Some(first), Some(last)) = (s.front(), s.back()) else {
            return 0.0;
        };
        if first.0.elapsed() > RATE_WINDOW * 2 {
            return 0.0; // nothing arrived lately
        }
        (last.1 - first.1) as f64 / first.0.elapsed().as_secs_f64().max(1.0)
    }

    /// Seconds until every planned segment is on disk, if the speed holds.
    pub fn eta(&self) -> Option<f64> {
        let (done, total, bytes) = (self.seg_done.load(Relaxed), self.seg_total.load(Relaxed), self.bytes.load(Relaxed));
        let speed = self.speed();
        (done > 0 && speed > 0.0).then(|| (bytes as f64 / done as f64) * total.saturating_sub(done) as f64 / speed)
    }

    /// Smoothed segment latency — basis for the hedging delay.
    pub fn typical_latency(&self) -> Duration {
        Duration::from_micros(self.ewma_us.load(Relaxed))
    }

    /// Latency percentile of downloaded (not cached) segments, seconds.
    pub fn percentile(&self, p: f64) -> Option<f64> {
        let mut v = self.latencies_ms.lock().unwrap().clone();
        if v.is_empty() {
            return None;
        }
        v.sort_unstable();
        Some(v[((v.len() - 1) as f64 * p).round() as usize] as f64 / 1000.0)
    }

    /// Histogram of segment latencies: `bins` equal buckets from 0 to the max; returns (max s, counts).
    pub fn histogram(&self, bins: usize) -> (f64, Vec<usize>) {
        let v = self.latencies_ms.lock().unwrap();
        let bins = bins.max(1);
        let max = v.iter().copied().max().unwrap_or(0).max(1) as f64;
        let mut h = vec![0; bins];
        for &x in v.iter() {
            h[((x as f64 / max) * (bins as f64 - 1.0)).round() as usize] += 1;
        }
        (max / 1000.0, h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_slots_and_histogram() {
        let p = Progress::default();
        p.set_slots(&[3, 2]);
        for (i, ms) in [100, 200, 300, 400, 1000].into_iter().enumerate() {
            p.downloaded(10, Duration::from_millis(ms), i % 2);
        }
        assert_eq!(p.percentile(0.5), Some(0.3));
        assert_eq!(p.percentile(0.99), Some(1.0));
        assert_eq!(p.slots(), vec![(3, 3), (2, 2)]);
        let (max, h) = p.histogram(4);
        assert_eq!((max, h.iter().sum::<usize>()), (1.0, 5));
        assert!(p.speed() > 0.0);
    }
}
