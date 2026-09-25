//! One download, end to end, for the command line: metadata → plan → segments → mix →
//! encode → files. Mixing and encoding run *while* segments download (see [`crate::render`]);
//! videos, participants' tracks and the multicam timeline are assembled after.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::deliver::{self, Made, Piece};
use crate::encode::Quality;
use crate::encode::packets::Packets;
use crate::engine::{Download, Engine};
use crate::http::Http;
pub use crate::options::{MIN_VIDEO_SECS, Options, What};
use crate::paths::{output_path, window};
use crate::plan::VideoPick;
use crate::progress::{Progress, Stage};
use crate::record::{self, Record};
use crate::render::{Render, Target};
use crate::{Error, Result, export};

pub struct Output {
    /// The mixed audio file, or the result folder when there is no mix file.
    pub path: PathBuf,
    pub dir: PathBuf,
    /// Every file produced, on the output timeline.
    pub pieces: Vec<Piece>,
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

/// Videos for the chosen tracks: all of them with `--tracks`, otherwise the longer ones.
pub fn video_pick(opts: &Options, tracks: &[record::Track]) -> Option<VideoPick> {
    let explicit = opts.tracks.is_some();
    let ids = tracks
        .iter()
        .filter(|t| explicit || t.duration >= MIN_VIDEO_SECS)
        .map(|t| t.id)
        .collect();
    opts.wants_video().then_some(VideoPick {
        tracks: ids,
        max_height: opts.video_height,
    })
}

/// Runs a whole job. `prog` receives render progress (`mixed_ms`, stage); the download's
/// own counters are handed to `on_download` as soon as it starts, and returned in the output.
pub async fn run(opts: Options, prog: Arc<Progress>, on_download: impl FnOnce(Arc<Progress>)) -> Result<Output> {
    prog.set_stage(Stage::Meta);
    let engine = Engine::new(opts.session_id.as_deref(), opts.connections, &opts.api_base, opts.cache_dir.clone())?;
    let rec = engine.record(&opts.link).await?;
    let (from, to) = window(&opts, &rec)?;
    let tracks = record::select(&rec.tracks, opts.tracks.as_deref())?;
    let pick = video_pick(&opts, &tracks);
    let dl = engine.download(&rec, tracks.clone(), (from, to), pick.as_ref()).await?;
    on_download(dl.prog.clone());
    produce(dl, &opts, (&rec, &tracks), prog, Arc::new(AtomicBool::new(false)), |_| {}).await
}

/// Everything after the download has started: one mix feeding the audio file and the videos'
/// soundtrack, then videos, participants' tracks and the timeline. Shared with `serve`.
/// `on_start` gets the output path (the mix file, or the folder) as soon as it exists.
pub async fn produce(
    dl: Arc<Download>,
    opts: &Options,
    (rec, tracks): (&Record, &[record::Track]),
    prog: Arc<Progress>,
    cancel: Arc<AtomicBool>,
    on_start: impl FnOnce(&Path),
) -> Result<Output> {
    let (from, to) = window(opts, rec)?;
    let ids: Vec<u64> = tracks.iter().map(|t| t.id).collect();
    prog.total_ms.store(((to - from) * 1000.0) as u64, Relaxed);
    prog.set_stage(Stage::Download);
    let range = (opts.from.is_some() || opts.to.is_some()).then_some((from, to));
    let mix_path = output_path(&opts.out_dir, rec, range, opts.format.extension())?;
    let dir = mix_path.parent().expect("inside the run folder").to_path_buf();
    on_start(if opts.wants_mix_file() { &mix_path } else { &dir });
    let packets: Packets = Arc::new(Mutex::new(Vec::new()));
    let mut targets = Vec::new();
    if opts.wants_mix_file() {
        targets.push(Target::File {
            path: mix_path.clone(),
            title: rec.title.clone(),
            format: opts.format,
            quality: opts.quality,
        });
    }
    if opts.wants_video() {
        targets.push(Target::Packets {
            quality: opts.quality,
            out: packets.clone(),
        });
    }
    let t0 = Instant::now();
    let planned = dl.of_kind(from, to, &ids, false);
    let n_tracks = planned.len();
    let render = {
        let (prog, gate) = (prog.clone(), dl.gate.clone());
        let job = Render {
            planned,
            from,
            to,
            format: opts.format,
            quality: opts.quality,
        };
        tokio::task::spawn_blocking(move || job.write(targets, &prog, gate, cancel))
    };
    let fetched = dl.wait().await;
    let download_secs = t0.elapsed().as_secs_f64();
    prog.set_stage(Stage::Mix);
    let rendered = render.await.map_err(|e| Error::Decode(e.to_string()))?;
    fetched?;
    rendered?;
    let mut pieces = Vec::new();
    if opts.wants_mix_file() {
        pieces.push(Piece {
            kind: Made::Mix,
            path: mix_path.clone(),
            name: rec.title.clone(),
            offset: 0.0,
            duration: to - from,
        });
    }
    let (dl2, opts2, rec2, dir2, mix) = (dl.clone(), opts.clone(), rec.clone(), dir.clone(), pieces.first().cloned());
    let extra = tokio::task::spawn_blocking(move || extras(&dl2, &opts2, (&rec2, mix), &dir2, (from, to), &ids, &packets));
    pieces.extend(extra.await.map_err(|e| Error::Encode(e.to_string()))??);
    prog.set_stage(Stage::Done);
    let bytes = pieces.iter().filter_map(|p| std::fs::metadata(&p.path).ok()).map(|m| m.len()).sum();
    let mix_secs = t0.elapsed().as_secs_f64() - download_secs;
    let path = if opts.wants_mix_file() { mix_path } else { dir.clone() };
    let (download, mixer_wait, record) = (dl.prog.clone(), dl.gate.longest_wait(), rec.clone());
    Ok(Output {
        path,
        dir,
        pieces,
        bytes,
        seconds: to - from,
        record,
        tracks: n_tracks,
        download_secs,
        mix_secs,
        download,
        mixer_wait,
    })
}

/// Videos, participants' tracks, MP4 conversion and the multicam timeline.
fn extras(
    dl: &Download,
    opts: &Options,
    (rec, mix): (&Record, Option<Piece>),
    dir: &Path,
    (from, to): (f64, f64),
    ids: &[u64],
    packets: &Packets,
) -> Result<Vec<Piece>> {
    let mut out = Vec::new();
    if opts.wants_video() {
        let channels = if opts.quality == Quality::High { 2 } else { 1 };
        let audio = packets.lock().unwrap();
        let mut videos = deliver::videos(
            &dl.of_kind(from, to, ids, true),
            &dir.join("video"),
            (from, to),
            Some((channels, &audio)),
        )?;
        if opts.mp4 {
            videos.iter_mut().try_for_each(deliver::to_mp4)?;
        }
        out.extend(videos);
    }
    if opts.separate || opts.multicam {
        out.extend(deliver::tracks(&dl.of_kind(from, to, ids, false), &dir.join("tracks"), from)?);
    }
    if opts.multicam {
        let all: Vec<Piece> = mix.into_iter().chain(out.iter().cloned()).collect();
        let base = crate::paths::sanitize(&rec.title);
        std::fs::write(dir.join(format!("{base}.fcpxml")), export::fcpxml(&rec.title, to - from, &all))?;
        std::fs::write(dir.join(format!("{base}.edl")), export::edl(&rec.title, &all))?;
    }
    Ok(out)
}
