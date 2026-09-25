//! Parallel segment download: AIMD-limited, retried with backoff, 429-aware.
//!
//! Workers pull segments from a priority queue ordered by position on the shared recording
//! timeline, so the mixer can follow right behind the download front. A failed segment goes
//! back *with its priority*. On top of that, [`crate::rush`] watches which file the mixer is
//! blocked on and fetches it immediately on a reserved connection — no head-of-line blocking
//! behind one slow request. Whoever brings a segment first wins; the other request is dropped.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{Notify, Semaphore};

use crate::decode::Gate;
use crate::hedge::{backoff, hedged_get};
use crate::http::{Http, Response};
use crate::limiter::Limiter;
use crate::progress::Progress;
use crate::{Error, Result, cache};

const RETRIES: u32 = 6;
pub(crate) const PENDING: u8 = 0;
pub(crate) const FLIGHT: u8 = 1;
pub(crate) const DONE: u8 = 2;

#[derive(Debug, Clone)]
pub struct Item {
    pub url: String,
    pub path: PathBuf,
    /// Timeline position (init pieces: before everything).
    pub at: f64,
    /// Track slot, for per-track progress.
    pub slot: usize,
}

/// Queue entry: (tier, timeline order, attempts). Smallest first.
type Entry = Reverse<(u8, usize, u32)>;

pub(crate) struct Ctx {
    pub(crate) http: Arc<Http>,
    pub(crate) lim: Arc<Limiter>,
    pub(crate) prog: Arc<Progress>,
    pub(crate) items: Vec<Item>,
    queue: Mutex<BinaryHeap<Entry>>,
    /// Items not finished yet: queued, in flight or waiting out a backoff.
    pub(crate) open: AtomicUsize,
    failed: Mutex<Option<Error>>,
    pub(crate) stop: AtomicBool,
    /// Set while the mixer has waited too long: no new regular requests, the server's
    /// capacity goes to what the mixer needs.
    pub(crate) focus: AtomicBool,
    pub(crate) state: Vec<AtomicU8>,
    /// Milliseconds since `epoch` when the item went in flight.
    pub(crate) since: Vec<AtomicU64>,
    pub(crate) done: Vec<Notify>,
    /// Rush requests in flight for the item, and when the last one started (ms since epoch).
    pub(crate) rushes: Vec<AtomicU8>,
    pub(crate) rushed_at: Vec<AtomicU64>,
    pub(crate) index: HashMap<PathBuf, usize>,
    pub(crate) epoch: Instant,
    pub(crate) gate: Arc<Gate>,
    pub(crate) reserve: Arc<Semaphore>,
}

pub struct Fetcher(Arc<Ctx>);

impl Fetcher {
    /// Items already on disk are only counted; the rest are queued in timeline order.
    /// `reserve` is the small pool of connections kept for rushing what the mixer waits for.
    pub fn new(
        http: Arc<Http>,
        (lim, reserve): (Arc<Limiter>, Arc<Semaphore>),
        prog: Arc<Progress>,
        gate: Arc<Gate>,
        items: Vec<Item>,
    ) -> Self {
        let mut todo = Vec::with_capacity(items.len());
        for it in items {
            match std::fs::metadata(&it.path) {
                Ok(m) => prog.cached(m.len(), it.slot),
                Err(_) => todo.push(it),
            }
        }
        todo.sort_by(|a, b| a.at.total_cmp(&b.at));
        let n = todo.len();
        Self(Arc::new(Ctx {
            http,
            lim,
            prog,
            queue: Mutex::new((0..n).map(|i| Reverse((1, i, 0))).collect()),
            open: AtomicUsize::new(n),
            failed: Mutex::new(None),
            stop: AtomicBool::new(false),
            focus: AtomicBool::new(false),
            state: (0..n).map(|_| AtomicU8::new(PENDING)).collect(),
            since: (0..n).map(|_| AtomicU64::new(0)).collect(),
            done: (0..n).map(|_| Notify::new()).collect(),
            rushes: (0..n).map(|_| AtomicU8::new(0)).collect(),
            rushed_at: (0..n).map(|_| AtomicU64::new(0)).collect(),
            index: todo.iter().enumerate().map(|(i, it)| (it.path.clone(), i)).collect(),
            items: todo,
            epoch: Instant::now(),
            gate,
            reserve,
        }))
    }

    /// Drops queued items that do not match `keep` — the download then finishes without them.
    pub fn retain(&self, keep: impl Fn(&Item) -> bool) {
        let mut q = self.0.queue.lock().unwrap();
        let (kept, dropped): (Vec<Entry>, Vec<Entry>) = q.drain().partition(|Reverse((_, i, _))| keep(&self.0.items[*i]));
        for Reverse((_, i, _)) in &dropped {
            if self.0.state[*i].swap(DONE, Relaxed) != DONE {
                self.0.prog.unplan(self.0.items[*i].slot);
                self.0.open.fetch_sub(1, Relaxed);
            }
        }
        q.extend(kept);
    }

    pub fn cancel(&self) {
        self.0.stop.store(true, Relaxed);
    }

    pub async fn run(&self) -> Result<()> {
        let rush = tokio::spawn(crate::rush::watch(self.0.clone()));
        let workers: Vec<_> = (0..crate::HARD_CAP).map(|_| tokio::spawn(worker(self.0.clone()))).collect();
        for w in workers {
            w.await.map_err(|e| Error::Net(e.to_string()))?;
        }
        rush.abort();
        match self.0.failed.lock().unwrap().take() {
            Some(e) => Err(e),
            None if self.0.open.load(Relaxed) > 0 => Err(Error::Usage("cancelled".into())),
            None => Ok(()),
        }
    }
}

impl Ctx {
    pub(crate) fn tail(&self) -> bool {
        self.queue.lock().unwrap().is_empty()
    }

    pub(crate) fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    /// Takes an item for a request; `false` if someone else already has it.
    pub(crate) fn claim(&self, idx: usize, from: u8) -> bool {
        let ok = self.state[idx].compare_exchange(from, FLIGHT, Relaxed, Relaxed).is_ok();
        if ok {
            self.since[idx].store(self.now_ms(), Relaxed);
        }
        ok
    }

    pub(crate) fn requeue_front(&self, idx: usize) {
        self.state[idx].store(PENDING, Relaxed);
        self.queue.lock().unwrap().push(Reverse((0, idx, 0)));
    }

    /// Stores the segment and counts it — once, whoever brought it first.
    pub(crate) async fn finish(&self, idx: usize, r: Response, took: Duration) -> Result<()> {
        if self.state[idx].load(Relaxed) == DONE {
            return Ok(());
        }
        let (path, len) = (self.items[idx].path.clone(), r.body.len() as u64);
        tokio::task::spawn_blocking(move || cache::write_atomic(&path, &r.body))
            .await
            .map_err(|e| Error::Net(e.to_string()))??;
        if self.state[idx].swap(DONE, Relaxed) != DONE {
            self.prog.downloaded(len, took, self.items[idx].slot);
            self.open.fetch_sub(1, Relaxed);
            self.done[idx].notify_waiters();
        }
        Ok(())
    }

    fn fail(&self, e: Error) {
        self.failed.lock().unwrap().get_or_insert(e);
        self.stop.store(true, Relaxed);
    }
}

async fn worker(ctx: Arc<Ctx>) {
    loop {
        if ctx.stop.load(Relaxed) || ctx.open.load(Relaxed) == 0 {
            return;
        }
        if ctx.focus.load(Relaxed) {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        }
        let permit = ctx.lim.acquire().await;
        let Some(Reverse((tier, idx, attempt))) = ctx.queue.lock().unwrap().pop() else {
            drop(permit);
            tokio::time::sleep(Duration::from_millis(50)).await; // others are in flight or backing off
            continue;
        };
        if !ctx.claim(idx, PENDING) {
            continue; // already rushed or done
        }
        ctx.prog.limit.store(ctx.lim.limit(), Relaxed);
        let t0 = Instant::now();
        let res = hedged_get(&ctx, idx).await;
        drop(permit);
        if let Err(e) = settle(&ctx, (tier, idx, attempt), res, t0).await {
            ctx.fail(e);
        }
    }
}

/// Stores a finished segment, or schedules a retry, or reports a fatal error.
async fn settle(ctx: &Arc<Ctx>, (tier, idx, attempt): (u8, usize, u32), res: Result<Option<Response>>, t0: Instant) -> Result<()> {
    let it = &ctx.items[idx];
    match res {
        Ok(None) => return Ok(()), // superseded: someone else brought it
        Ok(Some(r)) if r.status == 200 => {
            ctx.lim.on_success();
            return ctx.finish(idx, r, t0.elapsed()).await;
        }
        Ok(Some(r)) if r.status == 429 => {
            ctx.prog.http429.fetch_add(1, Relaxed);
            ctx.lim.on_429(r.retry_after);
        }
        Ok(Some(r)) if matches!(r.status, 401 | 403) => return Err(Error::Access(r.status)),
        Ok(Some(r)) if r.status == 404 => return Err(Error::Status(404, it.url.clone())),
        Ok(Some(_)) | Err(_) => ctx.lim.on_error(),
    }
    if ctx.state[idx].load(Relaxed) == DONE {
        return Ok(());
    }
    if attempt + 1 >= RETRIES {
        return Err(Error::Net(format!("gave up after {RETRIES} attempts: {}", it.url)));
    }
    ctx.prog.retries.fetch_add(1, Relaxed);
    ctx.state[idx].store(PENDING, Relaxed); // the rush watcher may take it before the backoff ends
    let ctx = ctx.clone();
    tokio::spawn(async move {
        tokio::time::sleep(backoff(attempt)).await;
        if ctx.state[idx].load(Relaxed) == PENDING {
            ctx.queue.lock().unwrap().push(Reverse((tier, idx, attempt + 1)));
        }
    });
    Ok(())
}
