//! One download, end to end: metadata → plan → segments → mix → encode → file.
//! Mixing and encoding run *while* segments download (see [`crate::render`]).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Instant;

use crate::decode::Gate;
use crate::fetch::{Item, fetch_all};
use crate::http::Http;
use crate::limiter::Limiter;
pub use crate::options::Options;
use crate::paths::{output_path, window};
use crate::progress::{Progress, Stage};
use crate::record::{self, Record, Track};
use crate::render::Render;
use crate::{Error, Result, cache, hls};

pub struct Output {
    pub path: PathBuf,
    pub bytes: u64,
    pub seconds: f64,
    pub record: Record,
    pub tracks: usize,
    pub download_secs: f64,
    pub mix_secs: f64,
}

pub async fn fetch_record(http: &Http, opts: &Options) -> Result<Record> {
    let id = record::record_id(&opts.link)?;
    let body = http.get_ok(&record::api_url(&opts.api_base, &id)).await?;
    let json = serde_json::from_slice(&body).map_err(|e| Error::Parse(format!("recording JSON: {e}")))?;
    record::parse(&id, &json)
}

pub(crate) struct Planned {
    pub track: Track,
    pub files: Vec<PathBuf>,
    /// (absolute start on the recording timeline, item); init pieces get `-1`.
    items: Vec<(f64, Item)>,
    /// Absolute start of the first downloaded segment, seconds.
    pub first: f64,
}

/// Audio rendition of one track, limited to the segments that overlap `[from, to)`.
async fn plan_track(http: Arc<Http>, root: PathBuf, rec_id: String, track: Track, from: f64, to: f64) -> Result<Option<Planned>> {
    let Some(master_url) = track.hls.clone() else { return Ok(None) };
    let master = hls::parse_master(&master_url, &String::from_utf8_lossy(&http.get_ok(&master_url).await?))?;
    let Some(audio) = master.audio else { return Ok(None) };
    let media = hls::parse_media(&audio, &String::from_utf8_lossy(&http.get_ok(&audio).await?))?;
    let dir = cache::track_dir(&root, &rec_id, track.id, "a");
    let (mut files, mut items, mut first) = (Vec::new(), Vec::new(), None);
    if let Some(init) = media.init {
        let p = cache::init_path(&dir);
        items.push((
            -1.0,
            Item {
                url: init,
                path: p.clone(),
            },
        ));
        files.push(p);
    }
    for (i, seg) in media.segments.iter().enumerate() {
        let (s, e) = (track.start + seg.start, track.start + seg.start + seg.duration);
        if e > from && s < to {
            let p = cache::segment_path(&dir, i);
            items.push((
                s,
                Item {
                    url: seg.url.clone(),
                    path: p.clone(),
                },
            ));
            files.push(p);
            first.get_or_insert(s);
        }
    }
    Ok(first.map(|first| Planned {
        track,
        files,
        items,
        first,
    }))
}

pub async fn run(opts: Options, prog: Arc<Progress>) -> Result<Output> {
    prog.set_stage(Stage::Meta);
    let lim = Limiter::new(64.min(opts.connections), opts.connections);
    let http = Arc::new(Http::new(opts.session_id.as_deref(), crate::HARD_CAP)?);
    let rec = fetch_record(&http, &opts).await?;
    let (from, to) = window(&opts, &rec)?;
    let planned = plan(&http, &opts, &rec, from, to).await?;
    // Timeline order: init pieces first, then segments by their place on the shared timeline,
    // so mixing can follow right behind the download front.
    let mut timed: Vec<(f64, Item)> = planned.iter().flat_map(|p| p.items.clone()).collect();
    timed.sort_by(|a, b| a.0.total_cmp(&b.0));
    let items: Vec<Item> = timed.into_iter().map(|(_, it)| it).collect();
    prog.seg_total.store(items.len(), Relaxed);
    prog.total_ms.store(((to - from) * 1000.0) as u64, Relaxed);
    prog.set_stage(Stage::Download);

    let range = (opts.from.is_some() || opts.to.is_some()).then_some((from, to));
    let path = output_path(&opts.out_dir, &rec, range, opts.format.extension())?;
    let gate = Arc::new(Gate::default());
    let t0 = Instant::now();
    let tracks = planned.len();
    let render = {
        let (prog, gate, path, title) = (prog.clone(), gate.clone(), path.clone(), rec.title.clone());
        let job = Render {
            planned,
            from,
            to,
            format: opts.format,
            quality: opts.quality,
        };
        tokio::task::spawn_blocking(move || job.write_file(&path, &title, &prog, gate))
    };
    let fetched = fetch_all(http, lim, prog.clone(), items).await;
    let download_secs = t0.elapsed().as_secs_f64();
    gate.failed.store(fetched.is_err(), Relaxed);
    gate.finished.store(true, Relaxed);
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
    })
}

async fn plan(http: &Arc<Http>, opts: &Options, rec: &Record, from: f64, to: f64) -> Result<Vec<Planned>> {
    let mut set = tokio::task::JoinSet::new();
    for t in record::select(&rec.tracks, opts.tracks.as_deref())? {
        set.spawn(plan_track(http.clone(), opts.cache_dir.clone(), rec.id.clone(), t, from, to));
    }
    let mut planned = Vec::new();
    while let Some(r) = set.join_next().await {
        planned.extend(r.map_err(|e| Error::Net(e.to_string()))??);
    }
    planned.sort_by_key(|p| p.track.index);
    Ok(planned)
}
