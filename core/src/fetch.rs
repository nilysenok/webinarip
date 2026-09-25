//! Parallel segment download: AIMD-limited, retried with backoff, 429-aware, with tail hedging.
//!
//! A pool of workers pulls segments from a priority queue ordered by position on the shared
//! timeline. A failed segment goes back *with its original priority*, so it is retried next —
//! not behind thousands of later segments while the mixer waits for it.
//!
//! Tail hedging: once the queue is empty, a request that is much slower than usual gets a
//! duplicate on a spare connection; whichever finishes first wins.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::http::{Http, Response};
use crate::limiter::Limiter;
use crate::progress::Progress;
use crate::{Error, Result, cache};

const RETRIES: u32 = 6;
const REQUEST_DEADLINE: Duration = Duration::from_secs(60);
const MIN_HEDGE_DELAY: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone)]
pub struct Item {
    pub url: String,
    pub path: PathBuf,
}

struct Ctx {
    http: Arc<Http>,
    lim: Arc<Limiter>,
    prog: Arc<Progress>,
    items: Vec<Item>,
    /// (index = priority, attempts so far); smaller index first.
    queue: Mutex<BinaryHeap<Reverse<(usize, u32)>>>,
    /// Items not finished yet: queued, in flight or waiting out a backoff.
    open: AtomicUsize,
    failed: Mutex<Option<Error>>,
    stop: AtomicBool,
}

impl Ctx {
    fn tail(&self) -> bool {
        self.queue.lock().unwrap().is_empty()
    }

    fn hedge_delay(&self) -> Duration {
        (self.prog.typical_latency() * 3).max(MIN_HEDGE_DELAY)
    }

    fn fail(&self, e: Error) {
        self.failed.lock().unwrap().get_or_insert(e);
        self.stop.store(true, Relaxed);
    }
}

/// Downloads every item not cached yet, earliest first. Cached items are only counted.
pub async fn fetch_all(http: Arc<Http>, lim: Arc<Limiter>, prog: Arc<Progress>, items: Vec<Item>) -> Result<()> {
    let mut todo = Vec::with_capacity(items.len());
    for it in items {
        match std::fs::metadata(&it.path) {
            Ok(m) => prog.cached(m.len()),
            Err(_) => todo.push(it),
        }
    }
    let queue = (0..todo.len()).map(|i| Reverse((i, 0))).collect();
    let open = AtomicUsize::new(todo.len());
    let ctx = Arc::new(Ctx {
        http,
        lim,
        prog,
        items: todo,
        queue: Mutex::new(queue),
        open,
        failed: Mutex::new(None),
        stop: AtomicBool::new(false),
    });
    let workers: Vec<_> = (0..crate::HARD_CAP).map(|_| tokio::spawn(worker(ctx.clone()))).collect();
    for w in workers {
        w.await.map_err(|e| Error::Net(e.to_string()))?;
    }
    match ctx.failed.lock().unwrap().take() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

async fn worker(ctx: Arc<Ctx>) {
    loop {
        if ctx.stop.load(Relaxed) || ctx.open.load(Relaxed) == 0 {
            return;
        }
        let permit = ctx.lim.acquire().await;
        let Some(Reverse((idx, attempt))) = ctx.queue.lock().unwrap().pop() else {
            drop(permit);
            tokio::time::sleep(Duration::from_millis(50)).await; // others are in flight or backing off
            continue;
        };
        ctx.prog.limit.store(ctx.lim.limit(), Relaxed);
        let t0 = Instant::now();
        let res = hedged_get(&ctx, &ctx.items[idx].url).await;
        drop(permit);
        if let Err(e) = settle(&ctx, idx, attempt, res, t0).await {
            ctx.fail(e);
        }
    }
}

/// Stores a finished segment, or schedules a retry, or reports a fatal error.
async fn settle(ctx: &Arc<Ctx>, idx: usize, attempt: u32, res: Result<Response>, t0: Instant) -> Result<()> {
    let it = &ctx.items[idx];
    match res {
        Ok(r) if r.status == 200 => {
            let len = r.body.len() as u64;
            let path = it.path.clone();
            tokio::task::spawn_blocking(move || cache::write_atomic(&path, &r.body))
                .await
                .map_err(|e| Error::Net(e.to_string()))??;
            ctx.lim.on_success();
            ctx.prog.downloaded(len, t0.elapsed());
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
        ctx.queue.lock().unwrap().push(Reverse((idx, attempt + 1)));
    });
    Ok(())
}

/// Exponential backoff with jitter: 0.5, 1, 2, 4 s … capped at 15 s.
fn backoff(attempt: u32) -> Duration {
    let base = 500u64 << attempt.min(5);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    Duration::from_millis((base + nanos as u64 % (base / 2 + 1)).min(15_000))
}

async fn hedged_get(ctx: &Ctx, url: &str) -> Result<Response> {
    let primary = ctx.http.get(url, REQUEST_DEADLINE);
    tokio::pin!(primary);
    loop {
        tokio::select! {
            r = &mut primary => return r,
            _ = tokio::time::sleep(ctx.hedge_delay()) => {
                if !ctx.tail() {
                    continue;
                }
                let Some(_spare) = ctx.lim.try_acquire() else { continue };
                ctx.prog.hedges.fetch_add(1, Relaxed);
                let hedge = ctx.http.get(url, REQUEST_DEADLINE);
                return tokio::select! {
                    r = &mut primary => r,
                    r = hedge => r,
                };
            }
        }
    }
}
