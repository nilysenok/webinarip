//! Parallel segment download: AIMD-limited, retried with backoff, 429-aware, with tail hedging.
//!
//! A pool of workers pulls segments from a priority queue: first by tier (a focused range can
//! be lifted to the front at any time), then by position on the shared timeline. A failed
//! segment goes back *with its priority*, so it is retried next — not behind thousands of
//! later segments while the mixer waits for it.
//!
//! Tail hedging: once the queue is empty, a request that is much slower than usual gets a
//! duplicate on a spare connection; whichever finishes first wins.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::hedge::{backoff, hedged_get};
use crate::http::{Http, Response};
use crate::limiter::Limiter;
use crate::progress::Progress;
use crate::{Error, Result, cache};

const RETRIES: u32 = 6;

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
    items: Vec<Item>,
    queue: Mutex<BinaryHeap<Entry>>,
    /// Items not finished yet: queued, in flight or waiting out a backoff.
    open: AtomicUsize,
    failed: Mutex<Option<Error>>,
    stop: AtomicBool,
}

pub struct Fetcher(Arc<Ctx>);

impl Fetcher {
    /// Items already on disk are only counted; the rest are queued in timeline order.
    pub fn new(http: Arc<Http>, lim: Arc<Limiter>, prog: Arc<Progress>, items: Vec<Item>) -> Self {
        let mut todo = Vec::with_capacity(items.len());
        for it in items {
            match std::fs::metadata(&it.path) {
                Ok(m) => prog.cached(m.len(), it.slot),
                Err(_) => todo.push(it),
            }
        }
        todo.sort_by(|a, b| a.at.total_cmp(&b.at));
        let queue = (0..todo.len()).map(|i| Reverse((1, i, 0))).collect();
        let (open, queue, failed) = (AtomicUsize::new(todo.len()), Mutex::new(queue), Mutex::new(None));
        Self(Arc::new(Ctx {
            http,
            lim,
            prog,
            items: todo,
            queue,
            open,
            failed,
            stop: AtomicBool::new(false),
        }))
    }

    /// Moves the items matching `hot` to the front of the queue (they keep timeline order).
    pub fn prioritize(&self, hot: impl Fn(&Item) -> bool) {
        let mut q = self.0.queue.lock().unwrap();
        let entries: Vec<Entry> = q
            .drain()
            .map(|Reverse((_, i, a))| Reverse((if hot(&self.0.items[i]) { 0 } else { 1 }, i, a)))
            .collect();
        q.extend(entries);
    }

    /// Drops queued items that do not match `keep` — the download then finishes without them.
    pub fn retain(&self, keep: impl Fn(&Item) -> bool) {
        let mut q = self.0.queue.lock().unwrap();
        let (kept, dropped): (Vec<Entry>, Vec<Entry>) = q.drain().partition(|Reverse((_, i, _))| keep(&self.0.items[*i]));
        for Reverse((_, i, _)) in &dropped {
            self.0.prog.unplan(self.0.items[*i].slot);
        }
        self.0.open.fetch_sub(dropped.len(), Relaxed);
        q.extend(kept);
    }

    pub fn cancel(&self) {
        self.0.stop.store(true, Relaxed);
    }

    pub async fn run(&self) -> Result<()> {
        let workers: Vec<_> = (0..crate::HARD_CAP).map(|_| tokio::spawn(worker(self.0.clone()))).collect();
        for w in workers {
            w.await.map_err(|e| Error::Net(e.to_string()))?;
        }
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
        let permit = ctx.lim.acquire().await;
        let Some(Reverse((tier, idx, attempt))) = ctx.queue.lock().unwrap().pop() else {
            drop(permit);
            tokio::time::sleep(Duration::from_millis(50)).await; // others are in flight or backing off
            continue;
        };
        ctx.prog.limit.store(ctx.lim.limit(), Relaxed);
        let t0 = Instant::now();
        let res = hedged_get(&ctx, &ctx.items[idx].url).await;
        drop(permit);
        if let Err(e) = settle(&ctx, (tier, idx, attempt), res, t0).await {
            ctx.fail(e);
        }
    }
}

/// Stores a finished segment, or schedules a retry, or reports a fatal error.
async fn settle(ctx: &Arc<Ctx>, (tier, idx, attempt): (u8, usize, u32), res: Result<Response>, t0: Instant) -> Result<()> {
    let it = &ctx.items[idx];
    match res {
        Ok(r) if r.status == 200 => {
            let len = r.body.len() as u64;
            let path = it.path.clone();
            tokio::task::spawn_blocking(move || cache::write_atomic(&path, &r.body))
                .await
                .map_err(|e| Error::Net(e.to_string()))??;
            ctx.lim.on_success();
            ctx.prog.downloaded(len, t0.elapsed(), it.slot);
            ctx.open.fetch_sub(1, Relaxed);
            return Ok(());
        }
        Ok(r) if r.status == 429 => {
            ctx.prog.http429.fetch_add(1, Relaxed);
            ctx.lim.on_429(r.retry_after);
        }
        Ok(r) if matches!(r.status, 401 | 403) => return Err(Error::Access(r.status)),
        Ok(r) if r.status == 404 => return Err(Error::Status(404, it.url.clone())),
        Ok(_) | Err(_) => ctx.lim.on_error(),
    }
    if attempt + 1 >= RETRIES {
        return Err(Error::Net(format!("gave up after {RETRIES} attempts: {}", it.url)));
    }
    ctx.prog.retries.fetch_add(1, Relaxed);
    let ctx = ctx.clone();
    tokio::spawn(async move {
        tokio::time::sleep(backoff(attempt)).await;
        ctx.queue.lock().unwrap().push(Reverse((tier, idx, attempt + 1)));
    });
    Ok(())
}
