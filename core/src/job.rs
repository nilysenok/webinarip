//! One download, end to end, for the command line: metadata → plan → segments → mix →
//! encode → file. Mixing and encoding run *while* segments download (see [`crate::render`]).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Instant;

use crate::engine::Engine;
use crate::http::Http;
pub use crate::options::Options;
use crate::paths::{output_path, window};
use crate::progress::{Progress, Stage};
use crate::record::{self, Record};
use crate::render::Render;
use crate::{Error, Result};

pub struct Output {
    pub path: PathBuf,
    pub bytes: u64,
    pub seconds: f64,
    pub record: Record,
    pub tracks: usize,
    pub download_secs: f64,
    pub mix_secs: f64,
    /// Download counters (segments, latency percentiles, retries…).
    pub download: Arc<Progress>,
    /// Longest time the mixer waited for one segment.
    pub mixer_wait: std::time::Duration,
}

pub async fn fetch_record(http: &Http, opts: &Options) -> Result<Record> {
    let id = record::record_id(&opts.link)?;
    let body = http.get_ok(&record::api_url(&opts.api_base, &id)).await?;
    let json = serde_json::from_slice(&body).map_err(|e| Error::Parse(format!("recording JSON: {e}")))?;
    record::parse(&id, &json)
}

/// Runs a whole job. `prog` receives render progress (`mixed_ms`, stage); the download's
/// own counters are handed to `on_download` as soon as it starts, and returned in the output.
pub async fn run(opts: Options, prog: Arc<Progress>, on_download: impl FnOnce(Arc<Progress>)) -> Result<Output> {
    prog.set_stage(Stage::Meta);
    let engine = Engine::new(opts.session_id.as_deref(), opts.connections, &opts.api_base, opts.cache_dir.clone())?;
    let rec = engine.record(&opts.link).await?;
    let (from, to) = window(&opts, &rec)?;
    let tracks = record::select(&rec.tracks, opts.tracks.as_deref())?;
    let dl = engine.download(&rec, tracks, from, to).await?;
    on_download(dl.prog.clone());
    prog.total_ms.store(((to - from) * 1000.0) as u64, Relaxed);
    prog.set_stage(Stage::Download);

    let range = (opts.from.is_some() || opts.to.is_some()).then_some((from, to));
    let path = output_path(&opts.out_dir, &rec, range, opts.format.extension())?;
    let t0 = Instant::now();
    let planned = dl.planned.clone();
    let tracks = planned.len();
    let render = {
        let (prog, gate, path, title) = (prog.clone(), dl.gate.clone(), path.clone(), rec.title.clone());
        let job = Render {
            planned,
            from,
            to,
            format: opts.format,
            quality: opts.quality,
        };
        tokio::task::spawn_blocking(move || job.write_file(&path, &title, &prog, gate, Arc::new(AtomicBool::new(false))))
    };
    let fetched = dl.wait().await;
    let download_secs = t0.elapsed().as_secs_f64();
    prog.set_stage(Stage::Mix);
    let rendered = render.await.map_err(|e| Error::Decode(e.to_string()))?;
    fetched?;
    rendered?;
    prog.set_stage(Stage::Done);
    let bytes = std::fs::metadata(&path)?.len();
    let mix_secs = t0.elapsed().as_secs_f64() - download_secs;
    Ok(Output {
        path,
        bytes,
        seconds: to - from,
        record: rec,
        tracks,
        download_secs,
        mix_secs,
        download: dl.prog.clone(),
        mixer_wait: dl.gate.longest_wait(),
    })
}
