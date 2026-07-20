# Vigil — Drag-to-Reorder Monitor Cards — Design Spec

> Backlog feature (CLAUDE.md §10 reserves `reorder(ids[])`; `monitors.sort_order` column exists,
> default 0, never written today). Lets the operator drag grid **cards** into a custom order that
> persists. Grid-only, hand-rolled pointer events (no new deps), a dedicated grip handle, plus a
> keyboard fallback for a11y. All user-confirmed via brainstorming.

---

## 1. Goals & Non-Goals

**Goals**
- Reorder the monitor **grid cards** by dragging a dedicated **grip handle**; order persists in
  `monitors.sort_order` via the reserved `reorder(ids[])` endpoint and survives reload + SSE ticks.
- **Keyboard-accessible:** the grip is focusable; ↑/↓ move the card one slot and persist (the app
  commits to full keyboard nav, §11.9).
- **Zero new dependencies** — native Pointer Events, matching the frontend's hand-rolled convention
  (only `solid-js` + `uplot` today).

**Non-Goals (v1)** (all user-confirmed)
- **Grid only.** The dense **list view keeps its existing sortable column headers** — no row drag.
- **No DnD library.**
- **No touch-specific tuning** — desktop pointer + keyboard (pointer events still fire on touch, but
  the interaction is designed for mouse/trackpad; no long-press/scroll-lock work).
- **No cross-tab live push** — a reorder persists to the DB; other open tabs adopt it on their next
  SSE `snapshot` (reconnect/lag) or manual refresh. No new event type, no `Snapshot` broadcast.
- **No reorder while filtered/searched** — see §7.
- Auto-scroll-near-viewport-edge during a drag is **optional polish**, not required for v1.

---

## 2. Context — what exists / what's reused

Verified against the tree.
- **Backend order is already `sort_order`-driven.** `monitors::list` (`api/monitors.rs:144`) and the
  SSE snapshot (`api/sse.rs:23`) both `SELECT * FROM monitors ORDER BY sort_order, id`. `sort_order`
  is in the `Monitor` model (`models.rs:124`) and **serialized** (no `skip_serializing`). It is
  **never written** today (create/update omit it) → every monitor is `sort_order=0`, so order falls
  through to `id`. **No reorder/sort endpoint exists.**
- **`sort_order`** — migration `0001_init.sql:26`, `INTEGER NOT NULL DEFAULT 0`. No new migration
  needed.
- **Frontend store** (`web/src/store.ts`, `createMonitorStore`): holds `monitors` (a Solid store
  array), seeded by `refresh()` (`GET /api/monitors`), kept live by `EventSource("/events")`. The
  pure reducer `applyEvent` (`store.ts:30-62`): `snapshot` **replaces** the array with the backend
  order; `monitor_updated`/`monitor_transition` **patch in place** via `patchMonitor` (order-
  preserving) and carry **no `sort_order`**. Committed with `reconcile(next, {key:"id"})`.
- **Grid** (`MonitorGrid.tsx`) renders `props.monitors` array order verbatim into a reflowing CSS
  grid (`.monitor-grid`, `repeat(auto-fill,minmax(300px,1fr))`, `theme.css:95`). **List**
  (`ListView.tsx:166`) *re-sorts* client-side: when no column sort is active it orders by
  `sort_order` then `name`. So a reorder must update **both** the grid's array order **and** each
  monitor's `sort_order` value.
- **Card root** (`MonitorCard.tsx:108`): `<div class="monitor-card" role="button" tabindex="0"
  onClick={openCard} …>` — **no `data-monitor-id`** today (list rows have one). `⋯` menu and
  `openCard` already use `stopPropagation` (the model for keeping the grip's events off `openCard`).
- **No DnD dep**, no existing pointer/drag handlers. `App.filtered()` (`App.tsx:28`) applies
  search + status-chip filter over `store.monitors()` and feeds both views.

---

## 3. Backend — `POST /api/monitors/reorder`

Handler `monitors::reorder`, route in `api/mod.rs` next to `/monitors/test-check` (a static segment
coexists with `/monitors/:id` under axum/matchit — `test-check` is the precedent):

```rust
pub async fn reorder(
    State(state): State<AppState>,
    Json(ids): Json<Vec<i64>>,
) -> ApiResult<serde_json::Value> {
    let mut tx = state.db.begin().await.map_err(db_err)?;              // multi-stmt tx (set_notifications precedent)
    for (i, id) in ids.iter().enumerate() {
        sqlx::query("UPDATE monitors SET sort_order = ? WHERE id = ?")
            .bind(i as i64).bind(id).execute(&mut *tx).await.map_err(db_err)?;
    }
    tx.commit().await.map_err(db_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}
```

- **Contract:** body is the **complete** ordered id list (`[3,1,2,…]`). Assigning
  `sort_order = array index` yields distinct, gapless values (no collisions). `GET /monitors` +
  the SSE snapshot immediately reflect it (both already `ORDER BY sort_order, id`).
- **Lenient:** an id not in `monitors` updates 0 rows (harmless). An empty array is a no-op. Does
  **not** touch `updated_at` (a reorder is not a config edit). Returns `{ok:true}`.
- **No event broadcast** (single operator; frontend is optimistic — §4). Documented boundary:
  other tabs resync on their next snapshot.
- **`api.ts`:** `reorderMonitors(ids: number[]): Promise<{ok:boolean}>` → `POST` the JSON array
  (mirrors `checkNow` etc.).

---

## 4. Frontend state — surviving SSE (the crux)

A pure helper + a store method:

- **`computeReorder(order: number[], draggedId: number, targetId: number): number[]`** (pure,
  exported, unit-tested): remove `draggedId`, then insert it at the current index of `targetId`
  (i.e. the dragged card lands in the target's slot). No-op if `draggedId === targetId` or either
  is absent.
- **`store.persistReorder(orderedIds: number[])`** (async): **optimistically** (a) reorder
  `state.monitors` to match `orderedIds` **and** (b) set each monitor's `sort_order = its new
  index`, committed via `setState("monitors", reconcile(next, {key:"id"}))` so cards keep identity
  and just move; then `await api.reorderMonitors(orderedIds)`; **on failure** call `refresh()` to
  revert to the server truth and surface the error (a `reorderError` signal or console — the grid
  shows nothing worse than the pre-drag order).

**Why this survives SSE:** a `monitor_updated`/`monitor_transition` delta preserves array order and
carries no `sort_order` → it can't disturb the optimistic order. A `snapshot` frame replaces the
array with the DB's `sort_order, id` order — which, once `persistReorder`'s POST has committed,
**equals** the optimistic order. (If a snapshot races in *before* the POST commits, it briefly shows
the old order, then the POST lands; acceptable for a single operator.) A `store.test.ts` unit test
locks this: reorder → feed a `monitor_updated` (order preserved) → feed a `snapshot` with the
persisted order (adopted, identical).

---

## 5. Drag interaction (hand-rolled Pointer Events)

**Grip:** a small handle (inline SVG "grip" — two columns of dots, Feather-style `currentColor`, to
match the rail icons) at the card header's leading edge (before the status dot). `.card-grip`:
`color: var(--text-tertiary)`, brightens on card hover, `cursor: grab` (`grabbing` while dragging).
The card root gains **`data-monitor-id={m.id}`** (for hit-testing). The grip's `pointerdown`
`stopPropagation`s so it never triggers `openCard`.

**Orchestration lives in `MonitorGrid`** (it knows all cards); `MonitorCard` just renders the grip
and calls `props.onGripDown(id, event)` / exposes keyboard handlers.

- **`dragOrder` signal** (`number[] | null`) + **`draggingId` signal**. The grid renders by
  `dragOrder() ?? props.monitors.map(id)` — so during a drag the local order drives the view.
- **onGripDown(id, e):** set `draggingId=id`, `dragOrder=current ids`, `e.currentTarget`
  `setPointerCapture`, attach `pointermove`/`pointerup`/`pointercancel` + `keydown(Escape)`.
  The dragged card gets `.dragging` (raised shadow, slight scale, dimmed, **`pointer-events:none`**
  so `elementsFromPoint` sees the cards beneath).
- **onPointerMove(e):** `document.elementsFromPoint(e.clientX, e.clientY)` → nearest
  `.monitor-card[data-monitor-id]` → `targetId`. If `targetId && targetId !== draggingId`:
  `setDragOrder(computeReorder(dragOrder(), draggingId, targetId))`.
- **onPointerUp:** `final = dragOrder()`; clear `draggingId`/`dragOrder`/`.dragging`; if `final`
  differs from the original id order → `store.persistReorder(final)`.
- **Escape / pointercancel:** abort — clear drag state, `dragOrder=null` (grid snaps back to
  `props.monitors`), no POST.
- Honors `prefers-reduced-motion` (skip the lift transform/animation; instant state).

**Testability split (jsdom can't do `elementFromPoint`/`getBoundingClientRect`):** the ordering is
the **pure `computeReorder`** (unit-tested); only the pixel hit-testing (`elementsFromPoint`) is an
untested DOM shim. The real drag gets **live browser verification** (§9).

---

## 6. Keyboard reorder (a11y)

The grip is `tabindex=0`, `role="button"`, `aria-label="Reorder {name} (use arrow keys)"`. When it
has focus and reorder is enabled:
- **ArrowUp / ArrowDown** move this card one slot: compute the new order by swapping this id with its
  neighbor in `props.monitors` order → `store.persistReorder(newOrder)`. Focus stays on the grip
  (the card element keeps its `reconcile` key, so the DOM node — and focus — moves with it).
- An **ARIA live region** in `MonitorGrid` (`aria-live="polite"`) announces
  `"Moved {name} to position {n} of {total}."`
- Fully unit-testable (index-based, no pixels): a test focuses a grip, fires ArrowDown, asserts
  `api.reorderMonitors` was called with the expected order.

---

## 7. When reorder is disabled

Reorder (drag **and** keyboard) is disabled whenever the visible grid isn't the full global order —
i.e. **search active (`query().trim()` non-empty) or a status filter active (`statusFilter()
!== null`)** — because a filtered subset can't map cleanly onto a global `sort_order`. `App` passes
`reorderEnabled={!query().trim() && statusFilter() === null}` to `MonitorGrid`. When disabled, the
grip is **hidden** (and, if we keep it visible-but-dimmed, a `title="Clear search/filters to
reorder"`); pointerdown/arrow handlers are no-ops. List view is unchanged (grid-only scope).

---

## 8. Module / file structure

- **Backend edits:** `api/monitors.rs` (`reorder` handler); `api/mod.rs` (route
  `.route("/monitors/reorder", post(monitors::reorder))`); `tests/api.rs` (reorder test).
- **Frontend edits:** `web/src/api.ts` (`reorderMonitors`); `web/src/store.ts` (`computeReorder`
  pure export + `persistReorder`); `web/src/components/MonitorGrid.tsx` (drag orchestration +
  `dragOrder`/`draggingId` + live region + `reorderEnabled` prop); `web/src/components/
  MonitorCard.tsx` (grip element + `data-monitor-id` + `onGripDown`/keyboard, gated on
  `reorderEnabled`); `web/src/App.tsx` (compute + pass `reorderEnabled`); `web/src/theme.css`
  (`.card-grip`, `.monitor-card.dragging`).
- **New tests:** `web/src/__tests__/reorder.test.ts` (pure `computeReorder`), store-reorder cases
  in `store.test.ts`, grid/card cases in a `monitorgrid`/`monitorcard` test (grip visibility gated
  on `reorderEnabled`; keyboard ArrowUp/Down calls the endpoint).
- **No new migration, no new deps, no new SSE event.**

---

## 9. Testing & verification

- **Backend** (`tests/api.rs`, real axum + reqwest): create 3 monitors → `POST /api/monitors/
  reorder` with a permuted id array → `GET /api/monitors` asserts the returned order matches and
  each `sort_order` equals its index; an unknown id in the array is a harmless no-op.
- **Frontend** (vitest/jsdom):
  - `computeReorder`: move up, move down, move to end, target==dragged (no-op), absent id (no-op).
  - `store.persistReorder`/reorder: array reordered **and** each `sort_order` patched; a following
    `monitor_updated` preserves order; a `snapshot` with the persisted order is adopted identically;
    a failed POST triggers `refresh()`.
  - Card/grid: grip **present** when `reorderEnabled`, **hidden** when a filter/search is active;
    keyboard ArrowDown on a focused grip calls `api.reorderMonitors` with the expected order;
    pressing the grip does not open the detail panel (stopPropagation).
- **Live (the one thing jsdom can't cover):** drive a real browser (Playwright) — grab a card's
  grip, drag it to a new slot, drop, reload, confirm order persisted; confirm the grip is gone when
  a search filter is active. Also eyeball the shrunk rail summary (separate commit `281ce3a`).
- Suites: backend `cargo test -p vigil -- --test-threads=1`; frontend `npx vitest run` + `npm run
  build` clean.

---

## 10. Documented boundaries (recap)
- **Grid only**; list keeps sortable headers. Desktop pointer + keyboard (no touch tuning).
- **Zero new deps**; hand-rolled pointer events; the pixel hit-test is verified live, not in jsdom.
- Reorder **persists per drop / per keyboard move** (one POST), optimistic with revert-on-failure.
- **No cross-tab push**; other tabs adopt on the next snapshot. No new SSE event, no migration.
- **Disabled while search/filter active** (grip hidden).
- `reorder` body is the **full** ordered id list; `sort_order = index`; `updated_at` untouched.

---

*End of spec. §3 backend, §4–§7 frontend behavior, §8 structure, §9 testing — build-ready for the
implementation plan.*
