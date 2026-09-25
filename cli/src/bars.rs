//! Terminal progress: one bar per stage, and a short summary with segment latency percentiles.

use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

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

pub async fn draw(p: Arc<Progress>) {
    let mp = MultiProgress::new();
    let meta = mp.add(ProgressBar::new_spinner());
    meta.set_message("reading the recording…");
    meta.enable_steady_tick(Duration::from_millis(100));
    let dl = bar(&mp, "download {bar:32.cyan/blue} {pos}/{len} seg · {msg}");
    let mix = bar(&mp, "mix+enc  {bar:32.green/blue} {msg}");
    let mut last = (0u64, std::time::Instant::now(), 0.0f64);
    let (trace, started) = (std::env::var_os("WEBINARIP_TRACE").is_some(), std::time::Instant::now());
    let mut next_trace = 0u64;
    loop {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let stage = p.stage();
        if stage != Stage::Meta {
            meta.finish_and_clear();
        }
        let (done, total, bytes) = (p.seg_done.load(Relaxed), p.seg_total.load(Relaxed), p.bytes.load(Relaxed));
        if trace && started.elapsed().as_secs() >= next_trace {
            next_trace += 5;
            eprintln!(
                "t={:>4}s seg {done}/{total} mixed {:.0}s conn {}",
                started.elapsed().as_secs(),
                p.mixed_ms.load(Relaxed) as f64 / 1000.0,
                p.limit.load(Relaxed)
            );
        }
        dl.set_length(total as u64);
        dl.set_position(done as u64);
        let dt = last.1.elapsed().as_secs_f64();
        if dt > 0.5 {
            let rate = (bytes - last.0) as f64 / dt;
            last = (
                bytes,
                std::time::Instant::now(),
                if last.2 == 0.0 { rate } else { last.2 * 0.6 + rate * 0.4 },
            );
        }
        let eta = if last.2 > 0.0 && done > 0 {
            hms((bytes as f64 / done as f64) * (total - done) as f64 / last.2)
        } else {
            "—".into()
        };
        dl.set_message(format!(
            "{:.0} MB · {:.1} MB/s · ETA {eta} · {} conn{}",
            bytes as f64 / 1e6,
            last.2 / 1e6,
            p.limit.load(Relaxed),
            if p.seg_cached.load(Relaxed) > 0 {
                format!(" · {} cached", p.seg_cached.load(Relaxed))
            } else {
                String::new()
            }
        ));
        if stage >= Stage::Mix {
            dl.finish();
        }
        if p.mixed_ms.load(Relaxed) > 0 {
            let (m, t) = (p.mixed_ms.load(Relaxed), p.total_ms.load(Relaxed).max(1));
            mix.set_length(t);
            mix.set_position(m);
            mix.set_message(format!("{} / {}", hms(m as f64 / 1000.0), hms(t as f64 / 1000.0)));
        }
    }
}

pub fn summary(out: &Output, p: &Progress, took: Duration) {
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
