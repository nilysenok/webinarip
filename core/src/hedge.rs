//! Retry pacing and tail hedging for segment requests.

use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use crate::Result;
use crate::fetch::Ctx;
use crate::http::Response;

const REQUEST_DEADLINE: Duration = Duration::from_secs(60);
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

pub(crate) async fn hedged_get(ctx: &Ctx, url: &str) -> Result<Response> {
    let primary = ctx.http.get(url, REQUEST_DEADLINE);
    tokio::pin!(primary);
    loop {
        tokio::select! {
            r = &mut primary => return r,
            _ = tokio::time::sleep((ctx.prog.typical_latency() * 3).max(MIN_HEDGE_DELAY)) => {
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
