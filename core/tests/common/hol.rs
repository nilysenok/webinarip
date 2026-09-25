//! Head-of-line blocking: one segment's first request hangs for 10 s. The mixer must not
//! wait for it — the segment it is blocked on is fetched again on a reserved connection.

use super as common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use webinarip_core::encode::{Format, Quality};
use webinarip_core::job::{self, Options};
use webinarip_core::progress::Progress;

/// Runs one job against a mock where `/t1/a/seg1.m4s` hangs for 10 s the first time.
/// Returns (longest mixer stall, median segment latency, total time).
#[allow(dead_code)] // used by the hol and hol_baseline test binaries
pub async fn stalled_run(tag: &str) -> (Duration, Duration, Duration) {
    let cfg = common::Config {
        per_conn_rate: 40_000,
        first_429: 0,
        private: false,
        stall: Some(("/t1/a/seg1.m4s", Duration::from_secs(10))),
    };
    let mock = common::start(cfg).await;
    let dir = std::env::temp_dir().join(format!("webinarip-hol-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut o = Options::new(format!("http://{}/j/1/2/record-new/1", mock.addr));
    (o.api_base, o.format, o.quality) = (format!("http://{}", mock.addr), Format::Wav, Quality::High);
    (o.out_dir, o.cache_dir, o.connections) = (dir.join("out"), dir.join("cache"), 8);
    let prog = Arc::new(Progress::default());
    let t0 = Instant::now();
    let out = job::run(o, prog, |_| {}).await.unwrap();
    let total = t0.elapsed();
    let p50 = Duration::from_secs_f64(out.download.percentile(0.5).unwrap());
    std::fs::remove_dir_all(dir).unwrap();
    (out.mixer_wait, p50, total)
}
