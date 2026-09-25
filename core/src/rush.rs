//! No head-of-line blocking: the mixer never waits long for one slow segment.
//!
//! Every 25 ms we look at the files a decoder is blocked on ([`crate::decode::Gate`]).
//! If such a segment is still queued or backing off, it is fetched right away; if it is in
//! flight for longer than half the median segment, a duplicate goes out — and a second one if the
//! first duplicate is slow too (the server's latency is heavy-tailed: p50 8 s, p99 29 s).
//! Rushes use a small reserved pool that is part of the overall ceiling.
//!
//! A slow segment here is usually slow for *every* request — it queues on the server behind
//! our own ~250 requests. So while the mixer has waited for more than one median segment,
//! workers start no new requests ("focus"): the server's capacity goes to what the mixer needs.
//! Measured on a real 85-minute recording (25.09): longest mixer wait 11.5–13.7 s with focus,
//! 18.9–22.3 s without, at p50 7–8 s; total time unchanged.

use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

use tokio::sync::OwnedSemaphorePermit;

use crate::fetch::{Ctx, DONE, FLIGHT, PENDING};
use crate::hedge::REQUEST_DEADLINE;

const TICK: Duration = Duration::from_millis(25);
/// A request the mixer waits for gets a duplicate once older than half the median (at least this).
const MIN_AGE: Duration = Duration::from_millis(100);
/// At most this many duplicates per segment at a time.
const MAX_RUSHES: u8 = 2;

/// How many connections of `total` are kept for rushing: 1 to 8.
pub fn reserve_for(total: usize) -> usize {
    (total / 16).clamp(1, 8).min(total.saturating_sub(1).max(1))
}

pub(crate) async fn watch(ctx: Arc<Ctx>) {
    if std::env::var_os("WEBINARIP_NO_RUSH").is_some() {
        return; // diagnostics: measure the download without rushing
    }
    let trace = std::env::var_os("WEBINARIP_TRACE").is_some();
    while !ctx.stop.load(Relaxed) && ctx.open.load(Relaxed) > 0 {
        tokio::time::sleep(TICK).await;
        let threshold = MIN_AGE.max(ctx.prog.median_latency() / 2);
        let waiting = ctx.gate.waited();
        let focus = waiting.iter().any(|(_, w)| *w >= threshold * 2);
        if ctx.focus.swap(focus, Relaxed) != focus && trace {
            eprintln!("focus {} t={:.1}s", if focus { "on" } else { "off" }, ctx.now_ms() as f64 / 1000.0);
        }
        for (path, waited) in waiting {
            let Some(&idx) = ctx.index.get(&path) else { continue };
            let age = Duration::from_millis(ctx.now_ms().saturating_sub(ctx.since[idx].load(Relaxed)));
            let due = match ctx.state[idx].load(Relaxed) {
                DONE => false,
                FLIGHT => age >= threshold,
                _ => true, // queued behind others or waiting out a backoff
            };
            let (n, last) = (ctx.rushes[idx].load(Relaxed), ctx.rushed_at[idx].load(Relaxed));
            let another = n == 0 || (n < MAX_RUSHES && Duration::from_millis(ctx.now_ms().saturating_sub(last)) >= threshold);
            if !due || !another {
                continue;
            }
            if trace {
                let st = ["queued", "flight", "done"][ctx.state[idx].load(Relaxed) as usize];
                eprintln!(
                    "rush t={:.1}s {} {st} age {:.1}s mixer waits {:.1}s rushes {n} free {}",
                    ctx.now_ms() as f64 / 1000.0,
                    path.iter()
                        .rev()
                        .take(3)
                        .collect::<Vec<_>>()
                        .iter()
                        .rev()
                        .map(|c| c.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/"),
                    age.as_secs_f64(),
                    waited.as_secs_f64(),
                    ctx.reserve.available_permits()
                );
            }
            if let Ok(permit) = ctx.reserve.clone().try_acquire_owned() {
                ctx.rushes[idx].fetch_add(1, Relaxed);
                ctx.rushed_at[idx].store(ctx.now_ms(), Relaxed);
                tokio::spawn(rush(ctx.clone(), idx, permit));
            }
        }
    }
}

async fn rush(ctx: Arc<Ctx>, idx: usize, _permit: OwnedSemaphorePermit) {
    let took_pending = ctx.claim(idx, PENDING);
    if !took_pending && ctx.state[idx].load(Relaxed) == DONE {
        ctx.rushes[idx].fetch_sub(1, Relaxed);
        return;
    }
    ctx.prog.hedges.fetch_add(1, Relaxed);
    let t0 = Instant::now();
    let superseded = ctx.done[idx].notified();
    tokio::pin!(superseded);
    superseded.as_mut().enable();
    let res = tokio::select! {
        r = ctx.http.get(&ctx.items[idx].url, REQUEST_DEADLINE) => r,
        _ = &mut superseded => {
            ctx.rushes[idx].fetch_sub(1, Relaxed);
            return;
        }
    };
    match res {
        Ok(r) if r.status == 200 => {
            let _ = ctx.finish(idx, r, t0.elapsed()).await; // a write error resurfaces on the regular path
        }
        Ok(r) if r.status == 429 => {
            ctx.prog.http429.fetch_add(1, Relaxed);
            ctx.lim.on_429(r.retry_after);
            if took_pending {
                ctx.requeue_front(idx);
            }
        }
        _ if took_pending => ctx.requeue_front(idx),
        _ => {}
    }
    ctx.rushes[idx].fetch_sub(1, Relaxed); // allow another rush if this one did not help
}

#[cfg(test)]
mod tests {
    use super::reserve_for;

    #[test]
    fn reserve_is_small_and_inside_the_ceiling() {
        assert_eq!(reserve_for(256), 8);
        assert_eq!(reserve_for(64), 4);
        assert_eq!(reserve_for(4), 1);
        assert_eq!(reserve_for(2), 1);
    }
}
