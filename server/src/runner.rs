//! The job queue: one job at a time (every job already uses up to 256 connections), in the
//! order they were added. A job reuses the download started when its link was pasted.

use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Instant;

use webinarip_core::encode::{Format, Quality};
use webinarip_core::job::Options;
use webinarip_core::paths::{output_path, window};
use webinarip_core::progress::Stage;
use webinarip_core::render::Render;
use webinarip_core::{Error, Result, timefmt};

use crate::app::{App, Entry, Job, Status};

pub async fn run_queue(app: Arc<App>) {
    loop {
        let next = app
            .jobs
            .lock()
            .unwrap()
            .iter()
            .find(|j| *j.status.lock().unwrap() == Status::Queued)
            .cloned();
        let Some(job) = next else {
            app.wake.notified().await;
            continue;
        };
        *job.status.lock().unwrap() = Status::Running;
        *job.started.lock().unwrap() = Some(Instant::now());
        let res = run_job(&app, &job).await;
        *job.took.lock().unwrap() = job.started.lock().unwrap().map(|t| t.elapsed().as_secs_f64());
        let mut status = job.status.lock().unwrap();
        *status = match res {
            Ok(()) => Status::Done,
            Err(_) if job.cancel.load(Relaxed) => Status::Cancelled,
            Err(e) => {
                *job.error.lock().unwrap() = Some(e.to_string());
                Status::Failed
            }
        };
    }
}

fn format_of(s: Option<&str>) -> Format {
    match s {
        Some("opus") => Format::Opus,
        Some("aac") => Format::Aac,
        Some("wav") => Format::Wav,
        _ => Format::Mp3,
    }
}

fn quality_of(s: Option<&str>) -> Quality {
    match s {
        Some("high") => Quality::High,
        Some("low") => Quality::Low,
        _ => Quality::Speech,
    }
}

async fn run_job(app: &Arc<App>, job: &Arc<Job>) -> Result<()> {
    job.render.set_stage(Stage::Meta);
    let engine = app.engine(job.session.as_deref())?;
    let probed = app.probe.lock().unwrap().clone().filter(|(l, _)| *l == job.link).map(|(_, r)| r);
    let rec = match probed {
        Some(r) => r,
        None => engine.record(&job.link).await?,
    };
    *job.record.lock().unwrap() = Some(rec.clone());
    let mut opts = Options::new(&job.link);
    opts.from = job.req.from.as_deref().filter(|s| !s.is_empty()).map(timefmt::parse).transpose()?;
    opts.to = job.req.to.as_deref().filter(|s| !s.is_empty()).map(timefmt::parse).transpose()?;
    let (from, to) = window(&opts, &rec)?;
    let dl = app.download_for(&engine, &rec).await?;
    *job.download.lock().unwrap() = Some(dl.clone());
    let chosen: Vec<u64> = rec
        .tracks
        .iter()
        .filter(|t| job.req.tracks.is_empty() || job.req.tracks.contains(&t.index))
        .map(|t| t.id)
        .collect();
    let slots: Vec<usize> = chosen.iter().filter_map(|id| dl.slot_of(*id)).collect();
    if slots.is_empty() {
        return Err(Error::Usage("none of the chosen tracks has audio".into()));
    }
    dl.restrict(from, to, &slots);
    let (format, quality) = (format_of(job.req.format.as_deref()), quality_of(job.req.quality.as_deref()));
    let range = (opts.from.is_some() || opts.to.is_some()).then_some((from, to));
    let path = output_path(&app.out_dir, &rec, range, format.extension())?;
    *job.path.lock().unwrap() = Some(path.clone());
    *job.streamable.lock().unwrap() = matches!(format, Format::Mp3 | Format::Opus);
    job.render.total_ms.store(((to - from) * 1000.0) as u64, Relaxed);
    job.render.set_stage(Stage::Download);
    let render = Render {
        planned: dl.select(from, to, &slots),
        from,
        to,
        format,
        quality,
    };
    let (prog, gate, cancel, title, p2) = (
        job.render.clone(),
        dl.gate.clone(),
        job.cancel.clone(),
        rec.title.clone(),
        path.clone(),
    );
    let t0 = Instant::now();
    let res = tokio::task::spawn_blocking(move || render.write_file(&p2, &title, &prog, gate, cancel))
        .await
        .map_err(|e| Error::Decode(e.to_string()))?;
    if res.is_err() {
        let _ = std::fs::remove_file(&path);
        return res;
    }
    job.render.set_stage(Stage::Done);
    app.remember(Entry {
        title: rec.title.clone(),
        when: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
        seconds: to - from,
        format: format.extension().to_owned(),
        bytes: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        path: path.display().to_string(),
        took: job
            .started
            .lock()
            .unwrap()
            .map_or(t0.elapsed().as_secs_f64(), |t| t.elapsed().as_secs_f64()),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_default_to_mp3() {
        assert_eq!(format_of(Some("opus")), Format::Opus);
        assert_eq!(format_of(Some("wav")), Format::Wav);
        assert_eq!(format_of(Some("aac")), Format::Aac);
        assert_eq!(format_of(Some("??")), Format::Mp3);
        assert_eq!(format_of(None), Format::Mp3);
    }
}
