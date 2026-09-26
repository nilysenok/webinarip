//! What the interface polls: one JSON snapshot of the probe, the active job, the queue and
//! the history. Everything the UI shows is computed here, not in JavaScript.

use std::sync::atomic::Ordering::Relaxed;

use serde::Serialize;

use crate::app::{App, Entry, Job, Status};
pub use crate::records::{RecordView, record_view};

#[derive(Serialize)]
pub struct JobView {
    pub id: u64,
    pub status: Status,
    /// meta | download | mix | done
    pub stage: &'static str,
    pub record: Option<RecordView>,
    pub percent: f64,
    pub speed: f64,
    pub eta: Option<f64>,
    pub mixed: f64,
    pub total: f64,
    pub bytes: u64,
    pub segments: (usize, usize),
    pub connections: usize,
    pub p50: Option<f64>,
    pub p99: Option<f64>,
    pub hist_max: f64,
    pub hist: Vec<usize>,
    pub retries: usize,
    /// Longest time the mixer waited for one segment, seconds.
    pub mixer_wait: f64,
    pub http429: usize,
    pub audio: bool,
    /// Mixing speed: seconds of output per second of wall time.
    pub realtime: f64,
    /// Size of the output file so far; once done, of all files.
    pub out_bytes: u64,
    /// Number of files produced (once done).
    pub files: usize,
    pub path: Option<String>,
    pub error: Option<String>,
    pub elapsed: f64,
}

/// While downloading, progress follows the segments on disk (the mixer can briefly wait
/// for one slow segment and would make the number jump); the last 3% are the final mixing.
fn percent(status: Status, stage: &str, d: &webinarip_core::progress::Progress, mixed: f64, total: f64) -> f64 {
    let (done, all) = (d.seg_done.load(Relaxed) as f64, d.seg_total.load(Relaxed).max(1) as f64);
    match (status, stage) {
        (Status::Done, _) => 100.0,
        (_, "download") => 97.0 * done / all,
        (_, "mix") => 97.0 + 3.0 * (mixed / total.max(1.0)).min(1.0),
        _ => 0.0,
    }
}

fn job_view(job: &Job) -> JobView {
    let status = *job.status.lock().unwrap();
    let dl = job.download.lock().unwrap().clone();
    let rec = job.record.lock().unwrap().clone();
    let (mixed, total) = (
        job.render.mixed_ms.load(Relaxed) as f64 / 1000.0,
        job.render.total_ms.load(Relaxed) as f64 / 1000.0,
    );
    let stage = match (status, &dl) {
        (Status::Done, _) => "done",
        (_, None) => "meta",
        (_, Some(d)) if d.is_finished() => "mix",
        _ => "download",
    };
    let d = dl.as_ref().map(|d| d.prog.clone()).unwrap_or_default();
    let (hist_max, hist) = d.histogram(24);
    // One lock per statement: a guard lives until the end of its statement, and holding two
    // at once (or the same one twice) is how this view once deadlocked.
    let took = *job.took.lock().unwrap();
    let started = *job.started.lock().unwrap();
    let result = *job.result.lock().unwrap();
    let streamable = *job.streamable.lock().unwrap();
    let error = job.error.lock().unwrap().clone();
    let elapsed = took.or_else(|| started.map(|t| t.elapsed().as_secs_f64())).unwrap_or(0.0);
    let realtime = if elapsed > 0.5 { mixed / elapsed } else { 0.0 };
    let path = job.path.lock().unwrap().clone();
    JobView {
        id: job.id,
        status,
        stage,
        record: rec.as_ref().map(|r| record_view(r, dl.as_deref())),
        percent: percent(status, stage, &d, mixed, total),
        speed: d.speed(),
        eta: match stage {
            "download" => d.eta(),
            "mix" if realtime > 0.0 => Some((total - mixed).max(0.0) / realtime),
            _ => None,
        },
        mixed,
        total,
        bytes: d.bytes.load(Relaxed),
        segments: (d.seg_done.load(Relaxed), d.seg_total.load(Relaxed)),
        connections: d.limit.load(Relaxed),
        p50: d.percentile(0.5),
        p99: d.percentile(0.99),
        hist_max,
        hist,
        retries: d.retries.load(Relaxed),
        mixer_wait: dl.as_ref().map_or(0.0, |d| d.gate.longest_wait().as_secs_f64()),
        http429: d.http429.load(Relaxed),
        audio: streamable && mixed >= 2.0 || status == Status::Done,
        realtime,
        out_bytes: result.map_or_else(
            || path.as_ref().and_then(|p| std::fs::metadata(p).ok()).map_or(0, |m| m.len()),
            |r| r.1,
        ),
        files: result.map_or(1, |r| r.0),
        path: path.as_ref().map(|p| p.display().to_string()),
        error,
        elapsed,
    }
}

#[derive(Serialize)]
pub struct StateView {
    pub probe: Option<RecordView>,
    /// The running job, or the last one this session (so its result stays on screen).
    pub job: Option<JobView>,
    pub queued: usize,
    pub history: Vec<Entry>,
}

pub fn state(app: &App) -> StateView {
    let jobs = app.jobs.lock().unwrap().clone();
    let current = jobs
        .iter()
        .find(|j| *j.status.lock().unwrap() == Status::Running)
        .or_else(|| jobs.iter().rev().find(|j| *j.status.lock().unwrap() != Status::Queued));
    let probe = app.probe.lock().unwrap().clone().map(|(_, rec)| {
        let dl = app.downloads.lock().unwrap().get(&rec.id).cloned();
        record_view(&rec, dl.as_deref())
    });
    StateView {
        probe,
        job: current.map(|j| job_view(j)),
        queued: jobs.iter().filter(|j| *j.status.lock().unwrap() == Status::Queued).count(),
        history: app.history.lock().unwrap().iter().take(8).cloned().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use webinarip_core::progress::Progress;

    #[test]
    fn state_does_not_deadlock_with_a_job() {
        let dir = std::env::temp_dir().join(format!("webinarip-view-{}", std::process::id()));
        let app = App::new(dir.clone(), dir.clone(), "http://127.0.0.1:9".into());
        let id = app.enqueue("https://x/record-new/1".into(), Default::default(), None);
        let job = app.job(id).unwrap();
        *job.status.lock().unwrap() = Status::Done;
        *job.result.lock().unwrap() = Some((3, 10));
        *job.took.lock().unwrap() = Some(1.5);
        let (tx, rx) = std::sync::mpsc::channel();
        let a = app.clone();
        std::thread::spawn(move || {
            let first = state(&a).job.map(|j| (j.files, j.out_bytes, j.elapsed));
            let _second = state(&a);
            tx.send(first).unwrap();
        });
        let got = rx.recv_timeout(std::time::Duration::from_secs(2)).expect("state() deadlocked");
        assert_eq!(got, Some((3, 10, 1.5)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn percent_follows_segments_then_mixing() {
        let p = Progress::default();
        p.set_slots(&[10]);
        for _ in 0..5 {
            p.cached(1, 0);
        }
        assert_eq!(percent(Status::Running, "download", &p, 0.0, 100.0), 48.5);
        assert_eq!(percent(Status::Running, "mix", &p, 50.0, 100.0), 98.5);
        assert_eq!(percent(Status::Done, "done", &p, 0.0, 100.0), 100.0);
        assert_eq!(percent(Status::Running, "meta", &p, 0.0, 0.0), 0.0);
    }
}
