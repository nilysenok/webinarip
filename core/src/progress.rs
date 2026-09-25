//! Lock-free progress counters shared between the job and whoever draws it (CLI, web UI).

use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Stage {
    Meta = 0,
    Download = 1,
    Mix = 2,
    Done = 3,
}

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

    pub fn cached(&self, bytes: u64) {
        self.seg_cached.fetch_add(1, Relaxed);
        self.seg_done.fetch_add(1, Relaxed);
        self.bytes.fetch_add(bytes, Relaxed);
    }

    pub fn downloaded(&self, bytes: u64, took: Duration) {
        self.seg_done.fetch_add(1, Relaxed);
        self.bytes.fetch_add(bytes, Relaxed);
        let us = took.as_micros() as u64;
        let old = self.ewma_us.load(Relaxed);
        self.ewma_us.store(if old == 0 { us } else { (old * 7 + us) / 8 }, Relaxed);
        self.latencies_ms.lock().unwrap().push(took.as_millis() as u32);
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
        let i = ((v.len() - 1) as f64 * p).round() as usize;
        Some(v[i] as f64 / 1000.0)
    }

    /// Histogram of segment latencies with `bins` equal buckets up to the max.
    pub fn histogram(&self, bins: usize) -> (f64, Vec<usize>) {
        let v = self.latencies_ms.lock().unwrap();
        let max = v.iter().copied().max().unwrap_or(0).max(1) as f64;
        let mut h = vec![0; bins.max(1)];
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
    fn percentiles() {
        let p = Progress::default();
        for ms in [100, 200, 300, 400, 1000] {
            p.downloaded(10, Duration::from_millis(ms));
        }
        assert_eq!(p.percentile(0.5), Some(0.3));
        assert_eq!(p.percentile(0.99), Some(1.0));
        assert_eq!(p.seg_done.load(Relaxed), 5);
    }
}
