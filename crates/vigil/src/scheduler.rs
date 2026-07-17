//! The scheduling loop: a min-heap of `(next_run_at, monitor_id)` driven by
//! `tokio::select!` between "soonest entry's timer elapses" and "a
//! `SchedCmd` arrived". Scheduling state lives in `SchedState`, a pure,
//! unit-testable struct that guarantees **at most one heap entry per
//! monitor id** and an **in-flight guard** so a monitor can never have two
//! `worker::run_check` calls running concurrently — that double-fire was
//! the root cause of duplicate incident rows and corrupted failure streaks
//! (races in `engine::apply_result`, which reads-then-writes status/streaks/
//! open-incident with no per-monitor lock).
//!
//! A monitor is popped when it fires and marked in-flight; it is only
//! eligible to fire again once the worker finishes and sends
//! `SchedCmd::Complete`, which clears the in-flight marker.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rand::Rng;
use tokio::sync::{mpsc, Semaphore};

use crate::app::{AppState, SchedCmd};
use crate::models::Ts;
use crate::worker;

/// How long to sleep when the heap is empty, so `select!` still services
/// `rx` instead of busy-looping.
const IDLE_SLEEP_SECS: u64 = 3600;

fn now() -> Ts {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

/// `now + base_secs`, jittered by up to ±5% of `base_secs`, so many
/// monitors sharing an interval don't all stampede on the same tick.
pub fn next_run_with_jitter(now: Ts, base_secs: i64) -> Ts {
    let span = (base_secs * 5 / 100).max(0);
    let jitter = rand::thread_rng().gen_range(-span..=span);
    now + base_secs + jitter
}

/// Pure scheduling state: a min-heap of `(next_run_at, monitor_id)` plus a
/// set of ids currently running in a worker. All mutation goes through
/// methods that maintain the "at most one heap entry per id" and "never
/// hand out an id that's already in-flight" invariants, so this struct can
/// be unit-tested without any async runtime, DB, or semaphore.
#[derive(Default)]
pub struct SchedState {
    heap: BinaryHeap<Reverse<(Ts, i64)>>,
    in_flight: HashSet<i64>,
}

impl SchedState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert/replace the single scheduled entry for `id` (dedups any
    /// existing entries for `id`).
    pub fn schedule(&mut self, id: i64, next: Ts) {
        self.heap = self.heap.drain().filter(|Reverse((_, hid))| *hid != id).collect();
        self.heap.push(Reverse((next, id)));
    }

    /// Soonest scheduled time (for computing the sleep), or None if empty.
    pub fn next_due(&self) -> Option<Ts> {
        self.heap.peek().map(|Reverse((t, _))| *t)
    }

    /// Pop the soonest entry IF due (t <= now) and not already in-flight;
    /// mark it in-flight and return its id. Stale entries for
    /// already-in-flight ids are popped and discarded. Returns None if
    /// nothing is due/runnable.
    pub fn take_due(&mut self, now: Ts) -> Option<i64> {
        while let Some(&Reverse((t, id))) = self.heap.peek() {
            if t > now {
                return None;
            }
            self.heap.pop();
            if self.in_flight.contains(&id) {
                continue;
            }
            self.in_flight.insert(id);
            return Some(id);
        }
        None
    }

    /// Worker finished: clear in-flight so the monitor can be scheduled
    /// again.
    pub fn complete(&mut self, id: i64) {
        self.in_flight.remove(&id);
    }

    /// "Check now": schedule ASAP, but only if not currently running
    /// (already covered if in-flight).
    pub fn check_now(&mut self, id: i64) {
        if !self.in_flight.contains(&id) {
            self.schedule(id, 0);
        }
    }

    /// Delete/pause: drop from heap and clear any in-flight marker.
    pub fn remove(&mut self, id: i64) {
        self.heap = self.heap.drain().filter(|Reverse((_, hid))| *hid != id).collect();
        self.in_flight.remove(&id);
    }
}

/// Re-reads a single monitor's `next_run_at`/`is_paused` from the DB and
/// re-heaps it if still eligible. Used for `SchedCmd::Upsert` (create/edit)
/// and after `SchedCmd::Complete` (worker finished, pick up whatever
/// `next_run_at` it just persisted). A deleted monitor (`Ok(None)`) is
/// simply not re-heaped; a query error is logged rather than silently
/// dropping the monitor from the schedule.
async fn reschedule_from_db(state: &AppState, sched: &mut SchedState, id: i64) {
    let row: Result<Option<(Option<Ts>, bool)>, sqlx::Error> = sqlx::query_as(
        "SELECT next_run_at, is_paused FROM monitors WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await;

    match row {
        Ok(Some((next_run_at, is_paused))) => {
            if !is_paused {
                sched.schedule(id, next_run_at.unwrap_or(0));
            }
        }
        Ok(None) => {
            // Deleted since it fired/was upserted — nothing to reschedule.
        }
        Err(e) => {
            tracing::error!(monitor_id = id, error = %e, "failed to reschedule monitor from db");
        }
    }
}

/// Drives every non-paused monitor's probe cycle. Runs for the lifetime of
/// the app; exits when `rx` closes (all senders — i.e. `AppState` clones —
/// dropped).
pub async fn run_scheduler(
    state: AppState,
    mut rx: mpsc::UnboundedReceiver<SchedCmd>,
    sem: Arc<Semaphore>,
) {
    let mut sched = SchedState::new();

    // Catch-up on restart: any monitor whose next_run_at is null or in the
    // past sorts first (unwrap_or(0)) and fires as soon as the loop starts.
    match sqlx::query_as::<_, (i64, Option<Ts>)>(
        "SELECT id, next_run_at FROM monitors WHERE is_paused = 0",
    )
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => {
            for (id, next_run_at) in rows {
                sched.schedule(id, next_run_at.unwrap_or(0));
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to seed scheduler from db; starting empty");
        }
    }

    loop {
        let wake_secs: u64 = match sched.next_due() {
            Some(t) => (t - now()).max(0) as u64,
            None => IDLE_SLEEP_SECS,
        };

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(wake_secs)) => {
                let now2 = now();
                while let Some(id) = sched.take_due(now2) {
                    let st = state.clone();
                    let sem2 = sem.clone();
                    // The permit is acquired *inside* the spawned task, not
                    // in this loop, so a saturated concurrency cap never
                    // blocks the scheduler from servicing `rx` (new
                    // CheckNow/Upsert/Remove/Complete commands) or firing
                    // the next due entry.
                    tokio::spawn(async move {
                        let _permit = match sem2.acquire_owned().await {
                            Ok(p) => p,
                            Err(_) => return,
                        };
                        worker::run_check(&st, id).await;
                    });
                }
            }
            cmd = rx.recv() => {
                match cmd {
                    Some(SchedCmd::Complete(id)) => {
                        sched.complete(id);
                        reschedule_from_db(&state, &mut sched, id).await;
                    }
                    Some(SchedCmd::Upsert(id)) => {
                        reschedule_from_db(&state, &mut sched, id).await;
                    }
                    Some(SchedCmd::CheckNow(id)) => {
                        sched.check_now(id);
                    }
                    Some(SchedCmd::Remove(id)) => {
                        sched.remove(id);
                    }
                    None => break,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SchedState;

    #[test]
    fn schedule_dedups_to_one_entry() {
        let mut s = SchedState::new();
        s.schedule(1, 100);
        s.schedule(1, 50);
        assert_eq!(s.take_due(100), Some(1)); // fires once
        assert_eq!(s.take_due(100), None); // no second entry for id 1
    }

    #[test]
    fn take_due_marks_in_flight_no_double_fire() {
        let mut s = SchedState::new();
        s.schedule(1, 0);
        assert_eq!(s.take_due(10), Some(1)); // now in-flight
        s.schedule(1, 0); // e.g. a CheckNow/Upsert lands while running
        assert_eq!(s.take_due(10), None, "in-flight monitor must not fire again");
        s.complete(1);
        s.schedule(1, 0);
        assert_eq!(s.take_due(10), Some(1), "after complete it can run again");
    }

    #[test]
    fn not_due_yet_returns_none() {
        let mut s = SchedState::new();
        s.schedule(1, 100);
        assert_eq!(s.take_due(50), None);
    }

    #[test]
    fn check_now_skipped_while_in_flight() {
        let mut s = SchedState::new();
        s.schedule(1, 100);
        assert_eq!(s.take_due(100), Some(1));
        s.check_now(1); // ignored (in-flight)
        assert_eq!(s.take_due(100), None);
    }

    #[test]
    fn remove_evicts_and_clears() {
        let mut s = SchedState::new();
        s.schedule(1, 0);
        s.remove(1);
        assert_eq!(s.take_due(100), None);
    }
}
