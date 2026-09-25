//! Retry pacing and tail hedging for segment requests.

use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use crate::Result;
use crate::fetch::{Ctx, DONE};
use crate::http::Response;

pub(crate) const REQUEST_DEADLINE: Duration = Duration::from_secs(60);
const MIN_HEDGE_DELAY: Duration = Duration::from_millis(1500);

/// Exponential backoff with jitter: 0.5, 1, 2, 4 s … capped at 15 s.
pub(crate) fn backoff(attempt: u32) -> Duration {
    let base = 500u64 << attempt.min(5);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    Duration::from_millis((base + nanos as u64 % (base / 2 + 1)).min(15_000))
}

/// The request for item `idx`. `Ok(None)` if the segment arrived another way meanwhile
/// (a rush from [`crate::rush`]); once the queue is empty, a slow request also gets a
/// duplicate on a spare connection and the first answer wins.
pub(crate) async fn hedged_get(ctx: &Ctx, idx: usize) -> Result<Option<Response>> {
    let url = &ctx.items[idx].url;
    let superseded = ctx.done[idx].notified();
    tokio::pin!(superseded);
    superseded.as_mut().enable();
    if ctx.state[idx].load(Relaxed) == DONE {
        return Ok(None);
    }
    let primary = ctx.http.get(url, REQUEST_DEADLINE);
    tokio::pin!(primary);
    loop {
        tokio::select! {
            r = &mut primary => return r.map(Some),
            _ = &mut superseded => return Ok(None),
            _ = tokio::time::sleep((ctx.prog.typical_latency() * 3).max(MIN_HEDGE_DELAY)) => {
                if !ctx.tail() {
                    continue;
                }
                let Some(_spare) = ctx.lim.try_acquire() else { continue };
                ctx.prog.hedges.fetch_add(1, Relaxed);
                let hedge = ctx.http.get(url, REQUEST_DEADLINE);
                return tokio::select! {
                    r = &mut primary => r.map(Some),
                    r = hedge => r.map(Some),
                    _ = &mut superseded => Ok(None),
                };
            }
        }
    }
}
