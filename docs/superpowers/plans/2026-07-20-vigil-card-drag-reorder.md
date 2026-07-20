# Drag-to-Reorder Monitor Cards — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the operator drag grid monitor cards into a custom order (via a dedicated grip handle) that persists in `monitors.sort_order` and survives reload + SSE, with a keyboard fallback.

**Architecture:** New `POST /api/monitors/reorder` writes `sort_order = array index` in one transaction (both `GET /monitors` and the SSE snapshot already `ORDER BY sort_order, id`). Frontend: pure ordering helpers + a `store.persistReorder` that optimistically reorders the store array AND patches each `sort_order` (so the grid's array order and ListView's value-sort agree, and the next snapshot matches), then POSTs. The drag itself is hand-rolled Pointer Events with a `data-monitor-id` hit-test; the pixel hit-test is verified live, the ordering math is pure and unit-tested.

**Tech Stack:** Rust (axum 0.7, sqlx 0.8 SQLite) backend; SolidJS + Vite frontend. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-07-20-vigil-card-drag-reorder-design.md` (read it first).

## Global Constraints

- **No new dependencies** (backend or frontend), **no new migration**, **no new SSE event**. `monitors.sort_order` already exists (migration 0001, `INTEGER NOT NULL DEFAULT 0`).
- **Grid only.** The ListView keeps its existing sortable column headers — do not add row drag.
- **Reorder disabled when search or a status filter is active** (`reorderEnabled = !query.trim() && statusFilter === null`); the grip is hidden in that state.
- **`reorder` body = the full ordered id list**; handler sets `sort_order = index`; does NOT touch `updated_at`; returns `{ok:true}`; lenient on unknown ids (0-row update).
- **Optimistic + revert:** on a failed POST, `store.refresh()` restores server truth.
- **jsdom limitation:** `elementFromPoint`/`getBoundingClientRect` don't work in jsdom — the pixel drag is verified in a **real browser** (Task 4); the pure ordering functions + keyboard reorder are unit-tested.
- Backend tests: `cargo test -p vigil -- --test-threads=1`. Frontend: `cd web && npx vitest run` + `npm run build` must stay clean. Match existing conventions (`ApiResult`/`db_err`/`super::{now}`; Solid `createSignal`/`<For>`/`<Show>`; the `stubFetch()` test pattern).

---

## File Structure

- **Backend:** `crates/vigil/src/api/monitors.rs` (new `reorder` handler) · `crates/vigil/src/api/mod.rs` (route) · `crates/vigil/tests/api.rs` (test).
- **Frontend data:** `web/src/store.ts` (pure `computeReorder`/`moveByOffset`/`reorderState` exports + `persistReorder` method) · `web/src/__tests__/store.test.ts` (pure-fn tests).
- **Frontend UI:** `web/src/components/MonitorCard.tsx` (grip + `data-monitor-id` + drag/keyboard props) · `web/src/components/MonitorGrid.tsx` (drag orchestration + live region + `reorderEnabled`/`onReorder`) · `web/src/App.tsx` (pass `reorderEnabled`/`onReorder`) · `web/src/theme.css` (`.card-grip`, `.monitor-card.dragging`, `.sr-only`) · `web/src/__tests__/monitorgrid.test.tsx` (new).

---

## Task 1: Backend `POST /api/monitors/reorder`

**Files:**
- Modify: `crates/vigil/src/api/monitors.rs`, `crates/vigil/src/api/mod.rs`
- Test: `crates/vigil/tests/api.rs`

**Interfaces:**
- Produces: `monitors::reorder(State, Json<Vec<i64>>) -> ApiResult<serde_json::Value>`; route `POST /api/monitors/reorder`.

- [ ] **Step 1: Write the failing test**

Append to `crates/vigil/tests/api.rs`:

```rust
#[tokio::test]
async fn reorder_persists_sort_order() {
    let env = test_state().await;
    let a = serve(env.state.clone()).await;
    let c = reqwest::Client::new();

    let mut ids = vec![];
    for n in ["a", "b", "c"] {
        let m: serde_json::Value = c.post(format!("http://{a}/api/monitors"))
            .json(&serde_json::json!({"name": n, "url": "https://e.com"}))
            .send().await.unwrap().json().await.unwrap();
        ids.push(m["id"].as_i64().unwrap());
    }

    // reverse the order
    let new_order = vec![ids[2], ids[1], ids[0]];
    let r = c.post(format!("http://{a}/api/monitors/reorder"))
        .json(&new_order).send().await.unwrap();
    assert!(r.status().is_success(), "reorder status: {}", r.status());

    let list: serde_json::Value = c.get(format!("http://{a}/api/monitors"))
        .send().await.unwrap().json().await.unwrap();
    let got: Vec<i64> = list.as_array().unwrap().iter().map(|m| m["id"].as_i64().unwrap()).collect();
    assert_eq!(got, new_order, "GET /monitors returns the new order");
    let sort_orders: Vec<i64> = list.as_array().unwrap().iter().map(|m| m["sort_order"].as_i64().unwrap()).collect();
    assert_eq!(sort_orders, vec![0, 1, 2], "sort_order == array index");

    // Lenient contract: an unknown id in the body is a harmless no-op (0 rows),
    // the known ids still get their positions.
    let with_unknown = vec![ids[0], 9999, ids[1]];
    let r2 = c.post(format!("http://{a}/api/monitors/reorder"))
        .json(&with_unknown).send().await.unwrap();
    assert!(r2.status().is_success(), "unknown id must not error: {}", r2.status());
    let list2: serde_json::Value = c.get(format!("http://{a}/api/monitors"))
        .send().await.unwrap().json().await.unwrap();
    let so = |id: i64| -> Option<i64> {
        list2.as_array().unwrap().iter()
            .find(|m| m["id"].as_i64() == Some(id)).unwrap()["sort_order"].as_i64()
    };
    assert_eq!(so(ids[0]), Some(0), "ids[0] -> index 0");
    assert_eq!(so(ids[1]), Some(2), "ids[1] -> index 2 (index 1's id 9999 doesn't exist)");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p vigil --test api reorder_persists_sort_order -- --test-threads=1`
Expected: FAIL — 404 (route not mounted), so `r.status().is_success()` fails.

- [ ] **Step 3: Add the handler**

In `crates/vigil/src/api/monitors.rs`, add the handler (near `create`/`update`). If `serde_json::json` isn't already imported in this file, use the fully-qualified `serde_json::json!` as below (no new `use` needed):

```rust
/// Persist a new card order: body is the complete ordered list of monitor
/// ids; each id's `sort_order` is set to its position. Both `list` and the
/// SSE snapshot already `ORDER BY sort_order, id`, so the new order is
/// immediately authoritative. Lenient: an unknown id updates 0 rows.
pub async fn reorder(
    State(state): State<AppState>,
    Json(ids): Json<Vec<i64>>,
) -> ApiResult<serde_json::Value> {
    let mut tx = state.db.begin().await.map_err(db_err)?;
    for (i, id) in ids.iter().enumerate() {
        sqlx::query("UPDATE monitors SET sort_order = ? WHERE id = ?")
            .bind(i as i64)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
    }
    tx.commit().await.map_err(db_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}
```

> Confirm the top of `monitors.rs` already imports `State`, `Json`, and `db_err` (it does — used by `create`/`update`/`list`). `ApiResult` is the module's existing alias. `state.db.begin()` + `execute(&mut *tx)` + `tx.commit()` is the same tx shape as `set_notifications`.

- [ ] **Step 4: Add the route**

In `crates/vigil/src/api/mod.rs`, add immediately after the `"/monitors/test-check"` route (line ~39):

```rust
        .route("/monitors/test-check", post(monitors::test_check))
        .route("/monitors/reorder", post(monitors::reorder))
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p vigil --test api reorder_persists_sort_order -- --test-threads=1`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/vigil/src/api/monitors.rs crates/vigil/src/api/mod.rs crates/vigil/tests/api.rs
git commit -m "feat(reorder): POST /api/monitors/reorder writes sort_order=index

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Frontend pure reorder logic + `store.persistReorder`

**Files:**
- Modify: `web/src/store.ts`
- Test: `web/src/__tests__/store.test.ts`

**Interfaces:**
- Consumes: nothing new.
- Produces: pure exports `computeReorder(order:number[], draggedId:number, targetId:number):number[]`, `moveByOffset(order:number[], id:number, delta:number):number[]`, `reorderState(s:StoreState, orderedIds:number[]):StoreState`; store method `persistReorder(orderedIds:number[]):Promise<void>` (on the object returned by `createMonitorStore`).

- [ ] **Step 1: Write the failing tests**

First extend the **existing** top-of-file import (line 1) — do NOT add a second `import … from "../store"` line (re-importing `applyEvent` is a duplicate-binding SyntaxError). Change line 1 to:

```ts
import { test, expect, vi } from "vitest"; import { applyEvent, applyCertBump, computeReorder, moveByOffset, reorderState, createMonitorStore, type StoreState } from "../store";
```

Then append these test cases (they reuse the already-imported `applyEvent`):

```ts
test("computeReorder does a standard array-move to the target's position", () => {
  expect(computeReorder([1, 2, 3], 3, 1)).toEqual([3, 1, 2]); // drag 3 onto 1
  expect(computeReorder([1, 2, 3], 1, 3)).toEqual([2, 3, 1]); // drag 1 onto 3
  expect(computeReorder([1, 2, 3], 2, 2)).toEqual([1, 2, 3]); // onto itself → no-op
  expect(computeReorder([1, 2, 3], 9, 1)).toEqual([1, 2, 3]); // absent id → no-op
});

test("moveByOffset nudges one slot and clamps at the ends", () => {
  expect(moveByOffset([1, 2, 3], 2, -1)).toEqual([2, 1, 3]); // up
  expect(moveByOffset([1, 2, 3], 2, 1)).toEqual([1, 3, 2]);  // down
  expect(moveByOffset([1, 2, 3], 1, -1)).toEqual([1, 2, 3]); // already first → clamp
  expect(moveByOffset([1, 2, 3], 3, 1)).toEqual([1, 2, 3]);  // already last → clamp
});

test("reorderState reorders monitors AND patches each sort_order to its index", () => {
  const s = { monitors: [{ id: 1, sort_order: 0 }, { id: 2, sort_order: 1 }, { id: 3, sort_order: 2 }], online: true };
  const out = reorderState(s, [3, 1, 2]);
  expect(out.monitors.map((m) => m.id)).toEqual([3, 1, 2]);
  expect(out.monitors.map((m) => m.sort_order)).toEqual([0, 1, 2]);
});

test("an optimistic reorder survives a monitor_updated delta and matches a later snapshot", () => {
  const s = { monitors: [{ id: 1, sort_order: 0, status: "up" }, { id: 2, sort_order: 1, status: "up" }], online: true };
  const reordered = reorderState(s, [2, 1]);
  // a live status delta must NOT disturb order
  const afterDelta = applyEvent(reordered, { event: "monitor_updated", data: { id: 1, status: "down" } });
  expect(afterDelta.monitors.map((m) => m.id)).toEqual([2, 1]);
  // a snapshot carrying the persisted order is adopted identically
  const afterSnap = applyEvent(afterDelta, { event: "snapshot", data: { monitors: [{ id: 2, sort_order: 0 }, { id: 1, sort_order: 1 }], online: true } });
  expect(afterSnap.monitors.map((m) => m.id)).toEqual([2, 1]);
});

test("persistReorder reverts via refresh() when the POST fails", async () => {
  // createMonitorStore opens an EventSource on init; stub it so jsdom doesn't throw.
  vi.stubGlobal("EventSource", class { onmessage: any = null; close() {} } as any);
  const calls: string[] = [];
  vi.stubGlobal("fetch", vi.fn(async (url: string, opts?: any) => {
    calls.push(`${opts?.method ?? "GET"} ${url}`);
    if (String(url).includes("/reorder")) return { ok: false, status: 500, json: async () => ({}) } as any;
    return { ok: true, json: async () => [] } as any; // GET /api/monitors (initial + revert refresh)
  }));

  const store = createMonitorStore();
  await store.persistReorder([2, 1]);
  store.close();

  const postIdx = calls.findIndex((c) => c.startsWith("POST /api/monitors/reorder"));
  expect(postIdx).toBeGreaterThanOrEqual(0);
  // a failed reorder POST must be followed by a GET /api/monitors (refresh() revert)
  expect(calls.slice(postIdx + 1)).toContain("GET /api/monitors");

  vi.unstubAllGlobals();
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd web && npx vitest run src/__tests__/store.test.ts`
Expected: FAIL — `computeReorder`/`moveByOffset`/`reorderState` are not exported.

- [ ] **Step 3: Add the pure functions to store.ts**

In `web/src/store.ts`, add near `applyEvent` (module scope, exported):

```ts
/** Move `draggedId` to `targetId`'s position (standard array-move: remove the
 *  dragged id, then insert it at the target's ORIGINAL index). This gives the
 *  intuitive result in both directions — dragging onto the next neighbor swaps
 *  them (NOT a no-op), dragging onto a lower card lands after it. Pure; used by
 *  the drag hit-test. No-op if either id is absent or they're the same. */
export function computeReorder(order: number[], draggedId: number, targetId: number): number[] {
  if (draggedId === targetId) return order;
  const from = order.indexOf(draggedId);
  const to = order.indexOf(targetId);
  if (from === -1 || to === -1) return order;
  const next = order.slice();
  next.splice(from, 1);
  next.splice(to, 0, draggedId);
  return next;
}

/** Nudge `id` by `delta` slots (clamped). Pure; used by the keyboard reorder. */
export function moveByOffset(order: number[], id: number, delta: number): number[] {
  const i = order.indexOf(id);
  if (i === -1) return order;
  const j = Math.max(0, Math.min(order.length - 1, i + delta));
  if (i === j) return order;
  const next = order.slice();
  next.splice(i, 1);
  next.splice(j, 0, id);
  return next;
}

/** Apply an id order to the monitor list: reorder AND set each monitor's
 *  `sort_order` to its new index (the grid renders array order; ListView
 *  value-sorts by `sort_order` — both must agree). Monitors not named in
 *  `orderedIds` (shouldn't happen — reorder is disabled while filtered) keep
 *  their relative order, appended. Pure. */
export function reorderState(s: StoreState, orderedIds: number[]): StoreState {
  const byId = new Map(s.monitors.map((m) => [m.id, m]));
  const named = new Set(orderedIds);
  const moved = orderedIds.filter((id) => byId.has(id)).map((id, i) => ({ ...byId.get(id), sort_order: i }));
  const rest = s.monitors.filter((m) => !named.has(m.id));
  return { ...s, monitors: [...moved, ...rest] };
}
```

- [ ] **Step 4: Add `persistReorder` to the store**

Inside `createMonitorStore` (after `refresh`, before `connect`), add:

```ts
  /** Optimistically apply a new card order, then persist it. On failure,
   *  refresh() reverts to server truth. The optimistic step (reorderState)
   *  makes the drag survive both `monitor_updated` deltas and the next
   *  `snapshot` (which will now carry the same DB order). */
  async function persistReorder(orderedIds: number[]) {
    setState("monitors", reconcile(reorderState(state, orderedIds).monitors, { key: "id" }));
    try {
      const res = await fetch("/api/monitors/reorder", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(orderedIds),
      });
      if (!res.ok) throw new Error(String(res.status));
    } catch {
      await refresh();
    }
  }
```

And add `persistReorder` to the returned object (next to `refresh`):

```ts
    refresh,
    persistReorder,
```

> Deliberate deviation from spec §3's "api.ts reorderMonitors": the POST lives in `persistReorder` (which owns the optimistic update + revert), matching how `refresh()` already inline-`fetch`es. No `api.ts` change needed; components call `store.persistReorder`, never the endpoint directly.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd web && npx vitest run src/__tests__/store.test.ts && npm run build`
Expected: store tests PASS; tsc/vite build clean.

- [ ] **Step 6: Commit**

```bash
git add web/src/store.ts web/src/__tests__/store.test.ts
git commit -m "feat(reorder): pure computeReorder/moveByOffset/reorderState + store.persistReorder

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Grip handle + grid drag/keyboard orchestration + wiring + CSS

**Files:**
- Modify: `web/src/components/MonitorCard.tsx`, `web/src/components/MonitorGrid.tsx`, `web/src/App.tsx`, `web/src/theme.css`
- Test: `web/src/__tests__/monitorgrid.test.tsx` (new)

**Interfaces:**
- Consumes: `store.persistReorder` (Task 2), `computeReorder`/`moveByOffset` (Task 2).
- Produces: `MonitorGridProps` gains `reorderEnabled?: boolean` + `onReorder?: (ids:number[]) => void`; `MonitorCardProps` gains `reorderEnabled?`, `dragging?`, `onGripDown?:(id,e:PointerEvent)=>void`, `onGripKey?:(id,e:KeyboardEvent)=>void`.

- [ ] **Step 1: Write the failing test**

Create `web/src/__tests__/monitorgrid.test.tsx`:

```tsx
import { render, screen, fireEvent } from "@solidjs/testing-library";
import { test, expect, vi } from "vitest";
import MonitorGrid from "../components/MonitorGrid";

function stubFetch() {
  vi.stubGlobal("fetch", vi.fn(async (url: string) => {
    const u = String(url);
    if (u.includes("/bars")) return { ok: true, json: async () => [] } as any;
    if (u.includes("/stats")) return { ok: true, json: async () => ({ uptime_pct: null, downtime_seconds: 0, avg_ms: null, incidents: 0 }) } as any;
    return { ok: true, json: async () => [] } as any;
  }));
}

const M = [
  { id: 1, name: "alpha", status: "up" },
  { id: 2, name: "bravo", status: "up" },
  { id: 3, name: "charlie", status: "up" },
];

test("grips are shown when reorder is enabled and hidden when not", async () => {
  stubFetch();
  const { unmount } = render(() => <MonitorGrid monitors={M} onOpen={() => {}} reorderEnabled onReorder={() => {}} />);
  await screen.findByText("alpha");
  expect(screen.getAllByLabelText(/Reorder/).length).toBe(3);
  unmount();

  stubFetch();
  render(() => <MonitorGrid monitors={M} onOpen={() => {}} reorderEnabled={false} onReorder={() => {}} />);
  await screen.findByText("alpha");
  expect(screen.queryByLabelText(/Reorder/)).toBeNull();
});

test("ArrowDown on a card's grip calls onReorder with the nudged order", async () => {
  stubFetch();
  const onReorder = vi.fn();
  render(() => <MonitorGrid monitors={M} onOpen={() => {}} reorderEnabled onReorder={onReorder} />);
  await screen.findByText("alpha");
  const grip = screen.getByLabelText("Reorder alpha (use arrow keys)");
  fireEvent.keyDown(grip, { key: "ArrowDown" });
  expect(onReorder).toHaveBeenCalledWith([2, 1, 3]);
});

test("clicking the grip does not open the card", async () => {
  stubFetch();
  const onOpen = vi.fn();
  render(() => <MonitorGrid monitors={M} onOpen={onOpen} reorderEnabled onReorder={() => {}} />);
  await screen.findByText("alpha");
  fireEvent.click(screen.getByLabelText("Reorder alpha (use arrow keys)"));
  expect(onOpen).not.toHaveBeenCalled();
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd web && npx vitest run src/__tests__/monitorgrid.test.tsx`
Expected: FAIL — no grip element (`getAllByLabelText(/Reorder/)` finds none).

- [ ] **Step 3: Add the grip to MonitorCard**

In `web/src/components/MonitorCard.tsx`, extend `MonitorCardProps`:

```tsx
export interface MonitorCardProps {
  monitor: any;
  onOpen: (id: number) => void;
  onChanged?: () => void;
  reorderEnabled?: boolean;
  dragging?: boolean;
  onGripDown?: (id: number, e: PointerEvent) => void;
  onGripKey?: (id: number, e: KeyboardEvent) => void;
}
```

Add `data-monitor-id` and the `dragging` class to the card root (replace the opening `<div class="monitor-card" …>`):

```tsx
    <div
      class={`monitor-card${props.dragging ? " dragging" : ""}`}
      data-monitor-id={props.monitor.id}
      role="button"
      tabindex="0"
      onClick={openCard}
      onKeyDown={onCardKeyDown}
    >
```

Add the grip as the first child of `.card-header` (before the `status-dot` span):

```tsx
      <div class="card-header">
        <Show when={props.reorderEnabled}>
          <button
            class="card-grip"
            type="button"
            aria-label={`Reorder ${props.monitor.name} (use arrow keys)`}
            title="Drag to reorder"
            onPointerDown={(e) => { e.stopPropagation(); props.onGripDown?.(props.monitor.id, e); }}
            onKeyDown={(e) => props.onGripKey?.(props.monitor.id, e)}
            onClick={(e) => e.stopPropagation()}
          >
            <svg class="grip-icon" viewBox="0 0 10 16" width="10" height="16" aria-hidden="true">
              <circle cx="2.5" cy="3" r="1.3" /><circle cx="7.5" cy="3" r="1.3" />
              <circle cx="2.5" cy="8" r="1.3" /><circle cx="7.5" cy="8" r="1.3" />
              <circle cx="2.5" cy="13" r="1.3" /><circle cx="7.5" cy="13" r="1.3" />
            </svg>
          </button>
        </Show>
        <span
          class={`status-dot ${statusClass(props.monitor)}`}
```

(`Show` is already imported in MonitorCard.tsx.)

- [ ] **Step 4: Add drag + keyboard orchestration to MonitorGrid**

Replace the entire body of `web/src/components/MonitorGrid.tsx`:

```tsx
import { createSignal, onCleanup, For, Show, type Component } from "solid-js";
import MonitorCard from "./MonitorCard";
import { computeReorder, moveByOffset } from "../store";

export interface MonitorGridProps {
  monitors: any[];
  onOpen: (id: number) => void;
  onChanged?: () => void;
  reorderEnabled?: boolean;
  onReorder?: (ids: number[]) => void;
}

const MonitorGrid: Component<MonitorGridProps> = (props) => {
  const [draggingId, setDraggingId] = createSignal<number | null>(null);
  const [dragOrder, setDragOrder] = createSignal<number[] | null>(null);
  const [announce, setAnnounce] = createSignal("");
  let cleanup: (() => void) | null = null;

  const currentIds = () => props.monitors.map((m) => m.id);

  // During a drag, render by the live drag order; otherwise trust the store.
  const displayed = () => {
    const order = dragOrder();
    if (!order) return props.monitors;
    const byId = new Map(props.monitors.map((m) => [m.id, m]));
    return order.map((id) => byId.get(id)).filter(Boolean);
  };

  function onGripDown(id: number, _e: PointerEvent) {
    if (!props.reorderEnabled) return;
    setDraggingId(id);
    setDragOrder(currentIds());
    const move = (ev: PointerEvent) => onMove(ev);
    const up = () => finishDrag(true);
    const cancel = () => finishDrag(false);
    const key = (ev: KeyboardEvent) => { if (ev.key === "Escape") finishDrag(false); };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", cancel);
    window.addEventListener("keydown", key);
    cleanup = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", cancel);
      window.removeEventListener("keydown", key);
    };
  }

  function onMove(e: PointerEvent) {
    const dragging = draggingId();
    if (dragging == null) return;
    // .dragging has pointer-events:none, so elementsFromPoint sees the card beneath.
    const card = document.elementsFromPoint(e.clientX, e.clientY)
      .find((el) => (el as HTMLElement).dataset?.monitorId) as HTMLElement | undefined;
    if (!card) return;
    const targetId = Number(card.dataset.monitorId);
    if (targetId === dragging) return;
    setDragOrder((ord) => computeReorder(ord ?? currentIds(), dragging, targetId));
  }

  function finishDrag(commit: boolean) {
    cleanup?.(); cleanup = null;
    const order = dragOrder();
    const dragging = draggingId();
    setDraggingId(null);
    setDragOrder(null);
    if (commit && order && dragging != null && order.join(",") !== currentIds().join(",")) {
      props.onReorder?.(order);
    }
  }

  function onGripKey(id: number, e: KeyboardEvent) {
    if (!props.reorderEnabled) return;
    const delta = e.key === "ArrowUp" ? -1 : e.key === "ArrowDown" ? 1 : 0;
    if (delta === 0) return;
    e.preventDefault();
    const ids = currentIds();
    const next = moveByOffset(ids, id, delta);
    if (next.join(",") === ids.join(",")) return;
    props.onReorder?.(next);
    const name = props.monitors.find((m) => m.id === id)?.name ?? "monitor";
    setAnnounce(`Moved ${name} to position ${next.indexOf(id) + 1} of ${next.length}`);
  }

  // A grid disposed mid-drag (rare) must detach its window listeners.
  onCleanup(() => cleanup?.());

  return (
    <Show
      when={props.monitors.length > 0}
      fallback={<div class="empty-state">No monitors match. Add your first monitor to get started.</div>}
    >
      <div class="monitor-grid">
        <For each={displayed()}>
          {(m) => (
            <MonitorCard
              monitor={m}
              onOpen={props.onOpen}
              onChanged={props.onChanged}
              reorderEnabled={props.reorderEnabled}
              dragging={draggingId() === m.id}
              onGripDown={onGripDown}
              onGripKey={onGripKey}
            />
          )}
        </For>
      </div>
      <div class="sr-only" aria-live="polite">{announce()}</div>
    </Show>
  );
};

export default MonitorGrid;
```

> **Note (deliberate deviation from spec §5's `setPointerCapture`):** the drag uses **window-level** `pointermove`/`pointerup`/`pointercancel` listeners rather than `setPointerCapture`. Window listeners receive the mouse-driven pointer stream while the grip is held, and the dragged card's `pointer-events:none` (`.dragging`) is what enables the `elementsFromPoint` hit-test — so capture isn't needed. `onCleanup` (added above) detaches them if the grid unmounts mid-drag.

- [ ] **Step 5: Wire `reorderEnabled`/`onReorder` in App.tsx**

In `web/src/App.tsx`, replace the `<MonitorGrid …/>` fallback (line ~107):

```tsx
                  fallback={
                    <MonitorGrid
                      monitors={filtered()}
                      onOpen={setOpenMonitorId}
                      onChanged={store.refresh}
                      reorderEnabled={!query().trim() && statusFilter() === null}
                      onReorder={store.persistReorder}
                    />
                  }
```

- [ ] **Step 6: Add CSS**

Append to `web/src/theme.css`:

```css
.card-grip{display:inline-flex;align-items:center;justify-content:center;padding:0 4px;margin-right:2px;background:none;border:none;color:var(--text-tertiary);cursor:grab;touch-action:none;line-height:0;border-radius:var(--r-sm)}
.card-grip:hover{color:var(--text-secondary)}
.card-grip:active{cursor:grabbing}
.card-grip:focus-visible{outline:2px solid var(--focus-ring);outline-offset:2px}
.card-grip .grip-icon{fill:currentColor}
.monitor-card.dragging{opacity:.6;transform:scale(1.02);box-shadow:0 12px 28px rgba(0,0,0,.5);pointer-events:none;z-index:2}
@media (prefers-reduced-motion:reduce){.monitor-card.dragging{transform:none}}
.sr-only{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0 0 0 0);white-space:nowrap;border:0}
```

> Verified: `--text-tertiary`, `--text-secondary`, `--focus-ring`, `--r-sm` all exist in `theme.css`'s `:root`. **`--shadow-panel` does NOT exist** (it's in the CLAUDE.md §11.1 token table but was never added to `theme.css`) — that's why the dragged-card lift uses the **literal** `box-shadow:0 12px 28px rgba(0,0,0,.5)` above. Do not reference `var(--shadow-panel)`/`var(--shadow-card)`.

- [ ] **Step 7: Run the tests + build**

Run: `cd web && npx vitest run src/__tests__/monitorgrid.test.tsx`
Expected: PASS (3 tests).

Run: `cd web && npx vitest run && npm run build`
Expected: full frontend suite PASS (no regressions — existing MonitorCard/ListView/store tests still green), build clean.

- [ ] **Step 8: Commit**

```bash
git add web/src/components/MonitorCard.tsx web/src/components/MonitorGrid.tsx web/src/App.tsx web/src/theme.css web/src/__tests__/monitorgrid.test.tsx
git commit -m "feat(reorder): grip handle + hand-rolled pointer drag + keyboard reorder (grid)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Live browser verification + finish

**Files:** none (verification + merge).

- [ ] **Step 1: Full suites green**

Run: `cargo test -p vigil -- --test-threads=1` (all pass) and `cd web && npx vitest run && npm run build` (all pass, build clean).

- [ ] **Step 2: Live drag verification in a real browser**

The pixel drag (`elementsFromPoint`) cannot run in jsdom — verify it live. Build + run an **isolated** container (do NOT touch the operator's live instance/volume — build a distinct tag, fresh volume, alt port), then drive a real browser (Playwright) against it:

```bash
cd /home/cd/Work/vigil
docker build -t vigil:reorder-accept .
docker rm -f vigil-reorder >/dev/null 2>&1; docker volume rm vigil-reorder-data >/dev/null 2>&1
docker run -d --name vigil-reorder -p 18093:8090 -v vigil-reorder-data:/data -e VIGIL_SMTP_PASSWORD=dummy vigil:reorder-accept
# wait for healthz 200, then seed 3 monitors via curl POST /api/monitors
```

Then with Playwright (the `mcp__plugin_playwright_playwright__*` tools): navigate to `http://127.0.0.1:18093`, confirm each card shows a grip, **drag one card's grip to a new slot**, reload the page, and assert the order persisted. Confirm the grip **disappears** when a search term is typed. Also **eyeball the shrunk rail summary** (commit `281ce3a` — the up/down/pause/maintenance icons should read noticeably smaller). Tear down: `docker rm -f vigil-reorder && docker volume rm vigil-reorder-data && docker rmi vigil:reorder-accept`.

Record the result (pass/fail + what was observed) — the drag is the one thing unit tests can't cover.

- [ ] **Step 3: Finish the branch**

Use `superpowers:finishing-a-development-branch` to complete: verify tests, then merge `feat/card-reorder` to `master` (local fast-forward, delete branch, no origin push — this repo's pattern). This branch carries the reorder feature **and** the rail-summary resize (`281ce3a`).

---

## Self-Review

**1. Spec coverage:** §3 endpoint → Task 1. §4 pure helpers + persistReorder (SSE survival) → Task 2. §5 drag (grip, pointer events, `computeReorder` hit-test, `.dragging` pointer-events:none) → Task 3. §6 keyboard (grip focus, ↑/↓, live region) → Task 3. §7 disable-when-filtered (`reorderEnabled`) → Task 3 (App). §8 file structure → all tasks. §9 testing (backend, pure fns, keyboard, gating, live drag) → Tasks 1/2/3/4. §10 boundaries honored (grid-only, zero deps, no migration/event). One deliberate, documented deviation: the reorder POST lives in `store.persistReorder` rather than `api.ts` (Task 2 Step 4 note).

**2. Placeholder scan:** none — every step has complete code. The only conditional guidance (confirm token names exist in `theme.css`; the live drag can't be jsdom-tested) is spec-sanctioned and specific.

**3. Type consistency:** `computeReorder(order, draggedId, targetId)` and `moveByOffset(order, id, delta)` signatures match between store.ts (Task 2) and MonitorGrid (Task 3). `reorderEnabled`/`onReorder` prop names match between MonitorGrid, App, and the tests. `onGripDown`/`onGripKey`/`dragging`/`reorderEnabled` match between MonitorCard and MonitorGrid. Endpoint path `/api/monitors/reorder` matches between handler route, backend test, and `persistReorder`. `store.persistReorder` name matches between Task 2 (definition) and Task 3 (App wiring).
