//! `/events` — Server-Sent Events. On connect, a forwarder task sends a
//! full `Event::Snapshot` first (so a fresh client never renders a partial
//! world), then relays bus deltas as they arrive. A `Lagged` bus read (the
//! subscriber fell behind the broadcast channel's ring buffer) is treated
//! the same way as first-connect: resync with a fresh snapshot rather than
//! risk the frontend missing an update it never knew it missed.
//!
//! The snapshot fetch is async (a DB query), so it can't happen inline in
//! `Stream::poll_next` — a small forwarder task + `mpsc` channel bridges
//! that gap into the `Sse` response's stream.

use std::convert::Infallible;

use axum::extract::State;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use tokio_stream::wrappers::ReceiverStream;

use crate::app::AppState;
use crate::events::Event;
use crate::models::{Connectivity, Monitor};

async fn build_snapshot(state: &AppState) -> Event {
    let monitors = sqlx::query_as::<_, Monitor>("SELECT * FROM monitors ORDER BY sort_order, id")
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    let online = matches!(state.anchor.current().await, Connectivity::Online);
    Event::Snapshot { monitors, online }
}

fn to_sse(ev: &Event, id: u64) -> SseEvent {
    SseEvent::default()
        .id(id.to_string())
        .data(serde_json::to_string(ev).unwrap_or_default())
}

pub async fn sse_handler(
    State(state): State<AppState>,
) -> Sse<ReceiverStream<Result<SseEvent, Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<SseEvent, Infallible>>(64);

    tokio::spawn(async move {
        let mut id = 0u64;

        // Subscribe BEFORE building the snapshot: build_snapshot awaits a DB
        // query, and a broadcast::Receiver only sees messages sent after it
        // subscribes. Snapshotting first would leave a window where an event
        // broadcast during that query is silently lost. A duplicate event
        // landing right after the snapshot is harmless; a gap is not.
        let mut bus_rx = state.bus.subscribe();

        let snap = build_snapshot(&state).await;
        if tx.send(Ok(to_sse(&snap, id))).await.is_err() {
            return;
        }

        loop {
            match bus_rx.recv().await {
                Ok(ev) => {
                    id += 1;
                    if tx.send(Ok(to_sse(&ev, id))).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    id += 1;
                    let snap = build_snapshot(&state).await;
                    if tx.send(Ok(to_sse(&snap, id))).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::default())
}
