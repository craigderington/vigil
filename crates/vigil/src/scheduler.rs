//! The scheduling loop: a min-heap of `(next_run_at, monitor_id)` driven by
//! `tokio::select!` between "soonest entry's timer elapses" and "a
//! `SchedCmd` arrived". A monitor is popped when it fires and is only
//! re-added once the worker finishes and sends `SchedCmd::Upsert` — so a
//! monitor can never be scheduled twice at once.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
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

/// Drives every non-paused monitor's probe cycle. Runs for the lifetime of
/// the app; exits when `rx` closes (all senders — i.e. `AppState` clones —
/// dropped).
pub async fn run_scheduler(
    state: AppState,
    mut rx: mpsc::UnboundedReceiver<SchedCmd>,
    sem: Arc<Semaphore>,
) {
    let mut heap: BinaryHeap<Reverse<(Ts, i64)>> = BinaryHeap::new();

    // Catch-up on restart: any monitor whose next_run_at is null or in the
    // past sorts first (unwrap_or(0)) and fires as soon as the loop starts.
    if let Ok(rows) = sqlx::query_as::<_, (i64, Option<Ts>)>(
        "SELECT id, next_run_at FROM monitors WHERE is_paused = 0",
    )
    .fetch_all(&state.db)
    .await
    {
        for (id, next_run_at) in rows {
            heap.push(Reverse((next_run_at.unwrap_or(0), id)));
        }
    }

    loop {
        let sleep_secs: u64 = match heap.peek() {
            Some(Reverse((t, _))) => (*t - now()).max(0) as u64,
            None => IDLE_SLEEP_SECS,
        };

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(sleep_secs)) => {
                let Some(Reverse((_, id))) = heap.pop() else { continue };

                // Lazy-deletion guard: the monitor may have been deleted or
                // paused since it was heaped. Re-check before spawning
                // rather than trusting the stale heap entry.
                let still_eligible: Option<i64> = sqlx::query_scalar(
                    "SELECT id FROM monitors WHERE id = ? AND is_paused = 0",
                )
                .bind(id)
                .fetch_optional(&state.db)
                .await
                .ok()
                .flatten();

                if still_eligible.is_none() {
                    continue;
                }

                let Ok(permit) = sem.clone().acquire_owned().await else { continue };
                let st = state.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    worker::run_check(&st, id).await;
                });
            }
            cmd = rx.recv() => {
                match cmd {
                    Some(SchedCmd::Upsert(id)) => {
                        let row: Option<Option<Ts>> = sqlx::query_scalar(
                            "SELECT next_run_at FROM monitors WHERE id = ? AND is_paused = 0",
                        )
                        .bind(id)
                        .fetch_optional(&state.db)
                        .await
                        .ok()
                        .flatten();
                        if let Some(next_run_at) = row {
                            heap.push(Reverse((next_run_at.unwrap_or(0), id)));
                        }
                    }
                    Some(SchedCmd::Remove(id)) => {
                        heap.retain(|Reverse((_, hid))| *hid != id);
                    }
                    Some(SchedCmd::CheckNow(id)) => {
                        heap.push(Reverse((0, id)));
                    }
                    None => break,
                }
            }
        }
    }
}
