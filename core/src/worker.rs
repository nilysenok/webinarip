//! One download worker: takes the earliest queued segment, fetches it, stores it or schedules
//! a retry that keeps its place in the queue.

use std::cmp::Reverse;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

use crate::fetch::{Ctx, DONE, PENDING};
use crate::hedge::{backoff, hedged_get};
use crate::http::Response;
use crate::{Error, Result};

const RETRIES: u32 = 6;

pub(crate) async fn run(ctx: Arc<Ctx>) {
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
