//! End to end against the mock server: throttled connections, 429s, a flaky segment,
//! the connection ceiling, the segment cache and the mixed result.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering::{Relaxed, SeqCst};

use webinarip_core::Error;
use webinarip_core::encode::{Format, Quality};
use webinarip_core::job::{self, Options};
use webinarip_core::progress::Progress;

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("webinarip-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn options(mock: &common::Mock, dir: &std::path::Path, connections: usize) -> Options {
    let mut o = Options::new(format!("http://{}/j/1/2/record-new/1", mock.addr));
    o.api_base = format!("http://{}", mock.addr);
    o.format = Format::Wav;
    o.quality = Quality::High;
    o.out_dir = dir.join("out");
    o.cache_dir = dir.join("cache");
    o.connections = connections;
    o
}

fn rms(samples: &[i16]) -> f64 {
    (samples.iter().map(|&s| (s as f64 / i16::MAX as f64).powi(2)).sum::<f64>() / samples.len().max(1) as f64).sqrt()
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_under_pressure_mixes_and_then_uses_the_cache() {
    let mock = common::start(common::Config {
        per_conn_rate: 40_000,
        first_429: 3,
        private: false,
    })
    .await;
    let dir = temp("e2e");
    let prog = Arc::new(Progress::default());
    let out = job::run(options(&mock, &dir, 4), prog.clone()).await.unwrap();

    // 429s were honoured and the flaky segment was retried.
    assert_eq!(mock.stats.sent_429.load(SeqCst), 3);
    assert_eq!(prog.http429.load(Relaxed), 3);
    assert!(prog.retries.load(Relaxed) >= 4);
    // Never more parallel connections than asked for (plus the metadata/playlist requests).
    assert!(
        mock.stats.conns_max.load(SeqCst) <= 4 + 2,
        "conns_max = {}",
        mock.stats.conns_max.load(SeqCst)
    );

    // Two 3 s tracks, the second starting at 2 s → 5 s of output, sound everywhere.
    let mut wav = hound::WavReader::open(&out.path).unwrap();
    let spec = wav.spec();
    assert_eq!((spec.sample_rate, spec.channels), (48_000, 2));
    let pcm: Vec<i16> = wav.samples::<i16>().map(Result::unwrap).collect();
    assert!(((pcm.len() / 2) as f64 / 48_000.0 - 5.0).abs() < 0.01, "{} frames", pcm.len() / 2);
    let second = |s: usize| &pcm[s * 96_000 + 10_000..(s + 1) * 96_000 - 10_000];
    for s in 0..5 {
        assert!(rms(second(s)) > 0.03, "silence in second {s}: {}", rms(second(s)));
    }
    // Where both tracks play, the sum is louder than either alone (no normalization).
    assert!(rms(second(2)) > rms(second(0)) * 1.2);

    // Same recording again, another format: nothing is downloaded twice.
    let hits = mock.stats.segment_hits.load(SeqCst);
    let mut again = options(&mock, &dir, 4);
    again.format = Format::Opus;
    let prog2 = Arc::new(Progress::default());
    job::run(again, prog2.clone()).await.unwrap();
    assert_eq!(mock.stats.segment_hits.load(SeqCst), hits);
    assert_eq!(prog2.seg_cached.load(Relaxed), prog2.seg_total.load(Relaxed));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn range_and_track_selection() {
    let mock = common::start(common::Config {
        per_conn_rate: 1_000_000,
        first_429: 0,
        private: false,
    })
    .await;
    let dir = temp("range");
    let mut o = options(&mock, &dir, 8);
    o.from = Some(1.0);
    o.to = Some(4.0);
    o.tracks = Some("host".into());
    o.quality = Quality::Low;
    let out = job::run(o, Arc::new(Progress::default())).await.unwrap();
    let mut wav = hound::WavReader::open(&out.path).unwrap();
    assert_eq!((wav.spec().sample_rate, wav.spec().channels), (16_000, 1));
    let pcm: Vec<i16> = wav.samples::<i16>().map(Result::unwrap).collect();
    assert_eq!(pcm.len(), 48_000); // 3 s at 16 kHz
    assert!(rms(&pcm[2_000..14_000]) > 0.03); // 1–2 s: the host speaks
    assert!(rms(&pcm[34_000..46_000]) < 0.005); // 3–4 s: host track is over, guest is not selected
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn private_recording_asks_for_a_session_id() {
    let mock = common::start(common::Config {
        per_conn_rate: 1_000_000,
        first_429: 0,
        private: true,
    })
    .await;
    let dir = temp("private");
    let err = job::run(options(&mock, &dir, 4), Arc::new(Progress::default()))
        .await
        .err()
        .unwrap();
    assert!(matches!(err, Error::Access(403)), "{err}");
    assert!(err.to_string().contains("session"));
    std::fs::remove_dir_all(dir).unwrap();
}
