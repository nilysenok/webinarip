//! AIMD concurrency limiter with a hard ceiling.
//!
//! Additive increase while requests succeed, multiplicative decrease on timeouts and errors,
//! and a hard halving plus a global pause on HTTP 429. The ceiling can be lowered by the user
//! but never raised above [`crate::HARD_CAP`] — past ~320 parallel connections the server
//! starts dropping them, and we do not want to hammer someone else's infrastructure.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

struct State {
    target: usize,
    capacity: usize,
    successes: usize,
    last_cut: Option<Instant>,
    pause_until: Option<Instant>,
}

pub struct Limiter {
    sem: Arc<Semaphore>,
    state: Mutex<State>,
    cap: usize,
}

/// Returned to the pool on drop — or retired, if the limit has shrunk meanwhile.
pub struct Permit {
    inner: Option<OwnedSemaphorePermit>,
    lim: Arc<Limiter>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut st = self.lim.state.lock().unwrap();
        if st.capacity > st.target {
            st.capacity -= 1;
            if let Some(p) = self.inner.take() {
                p.forget();
            }
        }
    }
}

const FLOOR: usize = 4;
const CUT_COOLDOWN: Duration = Duration::from_secs(2);

impl Limiter {
    pub fn new(start: usize, cap: usize) -> Arc<Self> {
        let cap = cap.clamp(1, crate::HARD_CAP);
        let start = start.clamp(1, cap);
        Arc::new(Self {
            sem: Arc::new(Semaphore::new(start)),
            state: Mutex::new(State {
                target: start,
                capacity: start,
                successes: 0,
                last_cut: None,
                pause_until: None,
            }),
            cap,
        })
    }

    pub fn limit(&self) -> usize {
        self.state.lock().unwrap().target
    }

    pub async fn acquire(self: &Arc<Self>) -> Permit {
        loop {
            let pause = self.state.lock().unwrap().pause_until;
            match pause {
                Some(t) if t > Instant::now() => tokio::time::sleep_until(t.into()).await,
                _ => break,
            }
        }
        let p = self.sem.clone().acquire_owned().await.expect("semaphore is never closed");
        Permit {
            inner: Some(p),
            lim: self.clone(),
        }
    }

    pub fn try_acquire(self: &Arc<Self>) -> Option<Permit> {
        let p = self.sem.clone().try_acquire_owned().ok()?;
        Some(Permit {
            inner: Some(p),
            lim: self.clone(),
        })
    }

    fn set_target(&self, st: &mut State, target: usize) {
        st.target = target.clamp(FLOOR.min(self.cap), self.cap);
        if st.target > st.capacity {
            self.sem.add_permits(st.target - st.capacity);
            st.capacity = st.target;
        } else if st.capacity > st.target {
            let retired = self.sem.forget_permits(st.capacity - st.target);
            st.capacity -= retired; // the rest retire as permits come back
        }
    }

    /// Additive increase: +8 after every `target / 8` successes in a row.
    pub fn on_success(&self) {
        let mut st = self.state.lock().unwrap();
        st.successes += 1;
        if st.successes >= (st.target / 8).max(1) && st.target < self.cap {
            st.successes = 0;
            let t = st.target + 8;
            self.set_target(&mut st, t);
        }
    }

    fn cut(&self, factor: f64, pause: Option<Duration>) {
        let mut st = self.state.lock().unwrap();
        st.successes = 0;
        if let Some(p) = pause {
            st.pause_until = Some(Instant::now() + p);
        }
        if st.last_cut.is_some_and(|t| t.elapsed() < CUT_COOLDOWN) {
            return; // one burst of failures = one cut
        }
        st.last_cut = Some(Instant::now());
        let t = (st.target as f64 * factor) as usize;
        self.set_target(&mut st, t);
    }

    /// Timeouts, connection errors, 5xx: ×0.75.
    pub fn on_error(&self) {
        self.cut(0.75, None);
    }

    /// HTTP 429: halve and pause everyone for `Retry-After` (at least 1 s).
    pub fn on_429(&self, retry_after: Option<Duration>) {
        self.cut(0.5, Some(retry_after.unwrap_or(Duration::ZERO).max(Duration::from_secs(1))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn grows_to_cap_and_halves_on_429() {
        let lim = Limiter::new(16, 64);
        for _ in 0..200 {
            lim.on_success();
        }
        assert_eq!(lim.limit(), 64);
        lim.on_429(Some(Duration::ZERO));
        assert_eq!(lim.limit(), 32);
        lim.on_error(); // inside the cooldown: no second cut
        assert_eq!(lim.limit(), 32);
    }

    #[tokio::test]
    async fn never_exceeds_hard_cap() {
        let lim = Limiter::new(10_000, 10_000);
        assert_eq!(lim.limit(), crate::HARD_CAP);
    }

    #[tokio::test]
    async fn shrinking_retires_permits_on_release() {
        let lim = Limiter::new(8, 8);
        let held: Vec<_> = (0..8).map(|_| lim.try_acquire().unwrap()).collect();
        lim.on_error(); // target 6 while all 8 are busy
        drop(held);
        let now: Vec<_> = std::iter::from_fn(|| lim.try_acquire()).collect();
        assert_eq!(now.len(), 6);
    }
}
