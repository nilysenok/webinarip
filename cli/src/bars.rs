//! Terminal progress: one bar per stage, and a short summary with segment latency percentiles.

use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use webinarip_core::job::Output;
use webinarip_core::progress::{Progress, Stage};
use webinarip_core::timefmt::hms;

fn bar(mp: &MultiProgress, template: &str) -> ProgressBar {
    let b = mp.add(ProgressBar::new(0));
    b.set_style(
        ProgressStyle::with_template(template)
            .expect("valid template")
            .progress_chars("█▉▊▋▌▍▎▏ "),
    );
    b
}

fn download_line(d: &Progress) -> String {
    let cached = d.seg_cached.load(Relaxed);
    format!(
        "{:.0} MB · {:.1} MB/s · ETA {} · {} conn{}",
        d.bytes.load(Relaxed) as f64 / 1e6,
        d.speed() / 1e6,
        d.eta().map_or("—".into(), hms),
        d.limit.load(Relaxed),
        if cached > 0 {
            format!(" · {cached} cached")
        } else {
            String::new()
        }
    )
}

/// `render`: stage and mixed seconds; `download`: filled in once the download has started.
pub async fn draw(render: Arc<Progress>, download: Arc<OnceLock<Arc<Progress>>>) {
    let mp = MultiProgress::new();
    let meta = mp.add(ProgressBar::new_spinner());
    meta.set_message("reading the recording…");
    meta.enable_steady_tick(Duration::from_millis(100));
    let dl = bar(&mp, "download {bar:32.cyan/blue} {pos}/{len} seg · {msg}");
    let mix = bar(&mp, "mix+enc  {bar:32.green/blue} {msg}");
    let (trace, started, mut next_trace) = (std::env::var_os("WEBINARIP_TRACE").is_some(), Instant::now(), 0);
    loop {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let Some(d) = download.get() else { continue };
        meta.finish_and_clear();
        let (done, total) = (d.seg_done.load(Relaxed), d.seg_total.load(Relaxed));
        let (m, t) = (render.mixed_ms.load(Relaxed), render.total_ms.load(Relaxed).max(1));
        if trace && started.elapsed().as_secs() >= next_trace {
            next_trace += 5;
            eprintln!(
                "t={:>4}s seg {done}/{total} mixed {:.0}s conn {}",
                started.elapsed().as_secs(),
                m as f64 / 1000.0,
                d.limit.load(Relaxed)
            );
        }
        dl.set_length(total as u64);
        dl.set_position(done as u64);
        dl.set_message(download_line(d));
        if render.stage() >= Stage::Mix {
            dl.finish();
        }
        if m > 0 {
            mix.set_length(t);
            mix.set_position(m);
            mix.set_message(format!("{} / {}", hms(m as f64 / 1000.0), hms(t as f64 / 1000.0)));
        }
    }
}

pub fn summary(out: &Output, took: Duration) {
    let p = &out.download;
    let pct = |q| p.percentile(q).map_or("—".into(), |s| format!("{s:.2} s"));
    println!();
    println!("✔ {}", out.path.display());
    println!(
        "  {} of audio from {} tracks · {:.1} MB · total {:.1} s (download {:.1} s, mix+encode {:.1} s)",
        hms(out.seconds),
        out.tracks,
        out.bytes as f64 / 1e6,
        took.as_secs_f64(),
        out.download_secs,
        out.mix_secs
    );
    println!(
        "  segments {} ({} cached) · p50 {} · p99 {} · retries {} · hedged {} · 429: {}",
        p.seg_done.load(Relaxed),
        p.seg_cached.load(Relaxed),
        pct(0.5),
        pct(0.99),
        p.retries.load(Relaxed),
        p.hedges.load(Relaxed),
        p.http429.load(Relaxed)
    );
}
