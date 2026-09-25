//! A process-wide engine: one HTTP client and one AIMD limiter shared by every download, so
//! the connection ceiling holds across a whole batch — and downloads that live on their own,
//! independent of what is rendered from them.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use tokio::sync::{Semaphore, watch};

use crate::decode::Gate;
use crate::fetch::{Fetcher, Item};
use crate::http::Http;
use crate::limiter::Limiter;
use crate::plan::{Planned, plan};
use crate::progress::{Progress, Stage};
use crate::record::{self, Record, Track};
use crate::{Error, Result};

pub struct Engine {
    pub http: Arc<Http>,
    /// Regular workers share this AIMD limit…
    pub lim: Arc<Limiter>,
    /// …and this small pool is kept for rushing what the mixer waits for. Together ≤ the ceiling.
    pub rush: Arc<Semaphore>,
    pub api_base: String,
    pub cache_dir: PathBuf,
}

impl Engine {
    pub fn new(session_id: Option<&str>, connections: usize, api_base: &str, cache_dir: PathBuf) -> Result<Arc<Self>> {
        let connections = connections.clamp(2, crate::HARD_CAP);
        let reserve = crate::rush::reserve_for(connections);
        let regular = connections - reserve;
        Ok(Arc::new(Self {
            http: Arc::new(Http::new(session_id, crate::HARD_CAP)?),
            lim: Limiter::new(64.min(regular), regular),
            rush: Arc::new(Semaphore::new(reserve)),
            api_base: api_base.to_owned(),
            cache_dir,
        }))
    }

    pub async fn record(&self, link: &str) -> Result<Record> {
        let id = record::record_id(link)?;
        let body = self.http.get_ok(&record::api_url(&self.api_base, &id)).await?;
        let json = serde_json::from_slice(&body).map_err(|e| Error::Parse(format!("recording JSON: {e}")))?;
        record::parse(&id, &json)
    }

    /// Plans `tracks` within `[from, to)` and starts downloading right away, in the background.
    pub async fn download(self: &Arc<Self>, rec: &Record, tracks: Vec<Track>, from: f64, to: f64) -> Result<Arc<Download>> {
        let planned: Vec<Planned> = plan(&self.http, &self.cache_dir, rec, tracks)
            .await?
            .iter()
            .filter_map(|p| p.window(from, to))
            .collect();
        let prog = Arc::new(Progress::default());
        let totals: Vec<usize> = planned.iter().map(|p| p.pieces.len()).collect();
        let mut slots = vec![0; planned.iter().map(|p| p.slot + 1).max().unwrap_or(0)];
        for (p, t) in planned.iter().zip(&totals) {
            slots[p.slot] = *t;
        }
        prog.set_slots(&slots);
        prog.set_stage(Stage::Download);
        let items = planned
            .iter()
            .flat_map(|p| {
                p.pieces.iter().map(move |x| Item {
                    url: x.url.clone(),
                    path: x.path.clone(),
                    at: x.at.unwrap_or(-1.0),
                    slot: p.slot,
                })
            })
            .collect();
        let (gate, (tx, rx)) = (Arc::new(Gate::default()), watch::channel(None));
        let pools = (self.lim.clone(), self.rush.clone());
        let fetcher = Arc::new(Fetcher::new(self.http.clone(), pools, prog.clone(), gate.clone(), items));
        let dl = Arc::new(Download {
            record: rec.clone(),
            planned,
            prog,
            gate: gate.clone(),
            fetcher: fetcher.clone(),
            result: rx,
        });
        tokio::spawn(async move {
            let res = fetcher.run().await.map_err(|e| e.to_string());
            gate.failed.store(res.is_err(), Relaxed);
            gate.finished.store(true, Relaxed);
            let _ = tx.send(Some(res));
        });
        Ok(dl)
    }
}

pub struct Download {
    pub record: Record,
    pub planned: Vec<Planned>,
    pub prog: Arc<Progress>,
    pub gate: Arc<Gate>,
    fetcher: Arc<Fetcher>,
    result: watch::Receiver<Option<std::result::Result<(), String>>>,
}

impl Download {
    /// Keeps only what `[from, to)` on `slots` needs; the rest is not downloaded at all.
    pub fn restrict(&self, from: f64, to: f64, slots: &[usize]) {
        self.fetcher
            .retain(|it| slots.contains(&it.slot) && (it.at < 0.0 || (it.at < to && it.at + 20.0 > from)));
    }

    /// Track slot of each track id in this download.
    pub fn slot_of(&self, track_id: u64) -> Option<usize> {
        self.planned.iter().find(|p| p.track.id == track_id).map(|p| p.slot)
    }

    pub fn cancel(&self) {
        self.fetcher.cancel();
    }

    pub fn is_finished(&self) -> bool {
        self.gate.finished.load(Relaxed)
    }

    /// Waits until every segment is on disk (or the download failed).
    pub async fn wait(&self) -> Result<()> {
        let mut rx = self.result.clone();
        let res = rx.wait_for(Option::is_some).await.map_err(|e| Error::Net(e.to_string()))?.clone();
        res.expect("checked by wait_for").map_err(Error::Net)
    }

    /// The planned tracks restricted to `slots` and cut to `[from, to)`.
    pub fn select(&self, from: f64, to: f64, slots: &[usize]) -> Vec<Planned> {
        self.planned
            .iter()
            .filter(|p| slots.contains(&p.slot))
            .filter_map(|p| p.window(from, to))
            .collect()
    }
}
