//! The job queue: one job at a time (every job already uses up to 256 connections), in the
//! order they were added. Audio jobs reuse the download started when the link was pasted;
//! jobs with video start one that also fetches video (audio already on disk is not refetched).

use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Instant;

use webinarip_core::encode::{Format, Quality};
use webinarip_core::job::{self, Options, What};
use webinarip_core::paths::window;
use webinarip_core::progress::Stage;
use webinarip_core::{Result, record, timefmt};

use crate::app::{App, Entry, Job, JobReq, Status};

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
        let started = *job.started.lock().unwrap();
        *job.took.lock().unwrap() = started.map(|t| t.elapsed().as_secs_f64());
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

fn what_of(s: Option<&str>) -> What {
    match s {
        Some("video") => What::Video,
        Some("both") => What::Both,
        _ => What::Audio,
    }
}

fn options(app: &App, link: &str, r: &JobReq) -> Result<Options> {
    let mut o = Options::new(link);
    let time = |s: &Option<String>| s.as_deref().filter(|s| !s.trim().is_empty()).map(timefmt::parse).transpose();
    (o.from, o.to) = (time(&r.from)?, time(&r.to)?);
    (o.what, o.format, o.quality) = (
        what_of(r.what.as_deref()),
        format_of(r.format.as_deref()),
        quality_of(r.quality.as_deref()),
    );
    o.tracks = (!r.tracks.is_empty()).then(|| r.tracks.iter().map(usize::to_string).collect::<Vec<_>>().join(","));
    (o.separate, o.multicam, o.mp4, o.video_height) = (r.separate, r.multicam, r.mp4 && o.what != What::Audio, r.video_height);
    (o.out_dir, o.cache_dir, o.api_base) = (app.out_dir.clone(), app.cache_dir.clone(), app.api_base.clone());
    Ok(o)
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
    let opts = options(app, &job.link, &job.req)?;
    let (from, to) = window(&opts, &rec)?;
    let tracks = record::select(&rec.tracks, opts.tracks.as_deref())?;
    let ids: Vec<u64> = tracks.iter().map(|t| t.id).collect();
    let spec = app.download_for(&engine, &rec).await?;
    let dl = match job::video_pick(&opts, &tracks) {
        None => spec,
        Some(pick) => {
            spec.restrict(0.0, 0.0, &[]); // the speculative audio-only download stops here
            let d = engine.download(&rec, tracks.clone(), (from, to), Some(&pick)).await?;
            app.downloads.lock().unwrap().insert(rec.id.clone(), d.clone());
            d
        }
    };
    dl.restrict(from, to, &dl.slots_of(&ids));
    *job.download.lock().unwrap() = Some(dl.clone());
    *job.streamable.lock().unwrap() = opts.wants_mix_file() && matches!(opts.format, Format::Mp3 | Format::Opus);
    let path_slot = job.clone();
    let res = job::produce(dl, &opts, (&rec, &tracks), job.render.clone(), job.cancel.clone(), move |p| {
        *path_slot.path.lock().unwrap() = Some(p.to_path_buf());
    })
    .await;
    let out = match res {
        Ok(o) => o,
        Err(e) => {
            if let Some(p) = job.path.lock().unwrap().as_ref().filter(|p| p.is_file()) {
                let _ = std::fs::remove_file(p);
            }
            return Err(e);
        }
    };
    *job.result.lock().unwrap() = Some((out.pieces.len(), out.bytes));
    let label = if opts.wants_video() {
        if opts.mp4 { "mp4" } else { "webm" }
    } else {
        opts.format.extension()
    };
    app.remember(Entry {
        title: rec.title.clone(),
        when: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
        seconds: to - from,
        format: label.to_owned(),
        bytes: out.bytes,
        path: out.path.display().to_string(),
        took: job.started.lock().unwrap().map_or(0.0, |t| t.elapsed().as_secs_f64()),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_values_default_sensibly() {
        assert_eq!(format_of(Some("opus")), Format::Opus);
        assert_eq!(format_of(Some("??")), Format::Mp3);
        assert_eq!(quality_of(None), Quality::Speech);
        assert_eq!(what_of(Some("both")), What::Both);
        assert_eq!(what_of(None), What::Audio);
    }
}
