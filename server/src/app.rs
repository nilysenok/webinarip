//! Server state: engines (one per session id, in memory only), downloads by recording,
//! the job queue and the history of finished jobs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use webinarip_core::engine::{Download, Engine};
use webinarip_core::progress::Progress;
use webinarip_core::record::Record;
use webinarip_core::{HARD_CAP, Result};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct JobReq {
    pub format: Option<String>,
    pub quality: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    /// 1-based track numbers; empty = all.
    #[serde(default)]
    pub tracks: Vec<usize>,
    /// audio | video | both
    pub what: Option<String>,
    #[serde(default)]
    pub separate: bool,
    #[serde(default)]
    pub multicam: bool,
    #[serde(default)]
    pub mp4: bool,
    pub video_height: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

pub struct Job {
    pub id: u64,
    pub link: String,
    pub req: JobReq,
    /// Never serialized, never written to disk.
    pub session: Option<String>,
    pub status: Mutex<Status>,
    pub record: Mutex<Option<Record>>,
    pub download: Mutex<Option<Arc<Download>>>,
    pub render: Arc<Progress>,
    pub path: Mutex<Option<PathBuf>>,
    pub error: Mutex<Option<String>>,
    pub cancel: Arc<AtomicBool>,
    pub started: Mutex<Option<Instant>>,
    /// Wall time of the finished job, seconds.
    pub took: Mutex<Option<f64>>,
    /// Files produced and their total size, once done.
    pub result: Mutex<Option<(usize, u64)>>,
    pub streamable: Mutex<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub title: String,
    pub when: String,
    pub seconds: f64,
    pub format: String,
    pub path: String,
    pub bytes: u64,
    pub took: f64,
}

pub struct App {
    pub out_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub api_base: String,
    engines: Mutex<HashMap<Option<String>, Arc<Engine>>>,
    pub downloads: Mutex<HashMap<String, Arc<Download>>>,
    pub probe: Mutex<Option<(String, Record)>>,
    pub jobs: Mutex<Vec<Arc<Job>>>,
    pub wake: Notify,
    pub history: Mutex<Vec<Entry>>,
    next_id: Mutex<u64>,
}

impl App {
    pub fn new(out_dir: PathBuf, cache_dir: PathBuf, api_base: String) -> Arc<Self> {
        let history = std::fs::read(cache_dir.join("history.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Arc::new(Self {
            out_dir,
            cache_dir,
            api_base,
            engines: Mutex::default(),
            downloads: Mutex::default(),
            probe: Mutex::default(),
            jobs: Mutex::default(),
            wake: Notify::new(),
            history: Mutex::new(history),
            next_id: Mutex::new(1),
        })
    }

    /// One engine per session id: all jobs share one connection ceiling.
    pub fn engine(&self, session: Option<&str>) -> Result<Arc<Engine>> {
        let mut map = self.engines.lock().unwrap();
        if let Some(e) = map.get(&session.map(str::to_owned)) {
            return Ok(e.clone());
        }
        let e = Engine::new(session, HARD_CAP, &self.api_base, self.cache_dir.clone())?;
        map.insert(session.map(str::to_owned), e.clone());
        Ok(e)
    }

    /// The running download of a recording, or a fresh one for the whole recording.
    pub async fn download_for(&self, engine: &Arc<Engine>, rec: &Record) -> Result<Arc<Download>> {
        if let Some(d) = self.downloads.lock().unwrap().get(&rec.id).filter(|d| !d.is_finished()) {
            return Ok(d.clone());
        }
        let d = engine.download(rec, rec.tracks.clone(), (0.0, rec.duration), None).await?;
        self.downloads.lock().unwrap().insert(rec.id.clone(), d.clone());
        Ok(d)
    }

    pub fn enqueue(&self, link: String, req: JobReq, session: Option<String>) -> u64 {
        let id = {
            let mut n = self.next_id.lock().unwrap();
            *n += 1;
            *n - 1
        };
        let job = Job {
            id,
            link,
            req,
            session,
            status: Mutex::new(Status::Queued),
            record: Mutex::default(),
            download: Mutex::default(),
            render: Arc::new(Progress::default()),
            path: Mutex::default(),
            error: Mutex::default(),
            cancel: Arc::new(AtomicBool::new(false)),
            started: Mutex::default(),
            took: Mutex::default(),
            result: Mutex::default(),
            streamable: Mutex::new(false),
        };
        self.jobs.lock().unwrap().push(Arc::new(job));
        self.wake.notify_one();
        id
    }

    pub fn job(&self, id: u64) -> Option<Arc<Job>> {
        self.jobs.lock().unwrap().iter().find(|j| j.id == id).cloned()
    }

    pub fn remember(&self, e: Entry) {
        let mut h = self.history.lock().unwrap();
        h.insert(0, e);
        h.truncate(50);
        if let Ok(json) = serde_json::to_vec_pretty(&*h) {
            let _ = std::fs::create_dir_all(&self.cache_dir);
            let _ = std::fs::write(self.cache_dir.join("history.json"), json);
        }
    }
}
