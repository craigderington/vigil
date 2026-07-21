import { createSignal } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { setMaintenanceIds, patchMaintenance } from "./maintenance_ids";

export type StoreState = { monitors: any[]; online: boolean };

export type StoreEvent = { event: string; data: any };

/** Dashboard grid⇄list toggle (spec §11.2 top bar). */
export type Layout = "grid" | "list";

/** Active ListView sort column + direction (spec §11.4). `col: null` means
 *  the default sort_order-then-name order. */
export type SortCol =
  | null
  | "name"
  | "type"
  | "last_checked_at"
  | "response_time_ms"
  | "uptime_24h"
  | "uptime_7d"
  | "uptime_30d";
export type SortDir = "asc" | "desc";
export interface ListSort {
  col: SortCol;
  dir: SortDir;
}

/** Pure reducer: given a state and an SSE frame, returns a NEW state. */
export function applyEvent(s: StoreState, ev: StoreEvent): StoreState {
  switch (ev.event) {
    case "snapshot":
      return { monitors: ev.data.monitors, online: ev.data.online };

    case "monitor_updated": {
      const { id, ...patch } = ev.data;
      // The SSE frame carries `checked_at`, but the Monitor model / detail
      // panel read `last_checked_at` — merge it across so "Last checked"
      // updates live instead of staying stale until the next full refresh.
      return {
        ...s,
        monitors: patchMonitor(s.monitors, id, { ...patch, last_checked_at: ev.data.checked_at }),
      };
    }

    case "monitor_transition": {
      return { ...s, monitors: patchMonitor(s.monitors, ev.data.id, { status: ev.data.to }) };
    }

    case "connectivity_changed":
      return { ...s, online: ev.data.online };

    case "incident_opened":
    case "incident_resolved":
      // No monitor-list shape carries incident detail (P1 grid/list; detail
      // panel is Task 16) — nothing to patch here.
      return s;

    default:
      return s;
  }
}

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

function patchMonitor(monitors: any[], id: number, patch: Record<string, any>): any[] {
  const idx = monitors.findIndex((m) => m.id === id);
  if (idx === -1) return monitors;
  const next = monitors.slice();
  next[idx] = { ...next[idx], ...patch };
  return next;
}

/**
 * Pure reducer for the SSL "cert version" map (§11.6 #6 live update): a
 * `cert_updated` SSE frame (fired by the backend's 12h cert scheduler or a
 * manual refresh) bumps that monitor's counter so `SslCard`'s
 * `createResource` — keyed in part on `certVersion(id)` — refetches. Split
 * out from `applyEvent` (which only knows about `StoreState`, not the
 * separate cert-bump map) so it can be unit-tested the same way `applyEvent`
 * is: as a plain function over plain data, no store/EventSource involved.
 */
export function applyCertBump(
  bump: Record<number, number>,
  ev: StoreEvent,
): Record<number, number> {
  if (ev.event !== "cert_updated") return bump;
  const id = ev.data?.id;
  if (id == null) return bump;
  return { ...bump, [id]: (bump[id] ?? 0) + 1 };
}

/**
 * Solid store seeded from GET /api/monitors, then kept live via
 * `EventSource("/events")`. Every inbound frame is run through the same
 * pure `applyEvent` reducer used by the tests, so store behavior and test
 * behavior can never drift.
 */
export function createMonitorStore() {
  const [state, setState] = createStore<StoreState>({ monitors: [], online: true });

  // View + sort live here (rather than as local signals inside ListView)
  // so they survive toggling back and forth between grid and list within a
  // session — a signal created inside ListView itself would reset every
  // time the component unmounts on switching back to grid.
  const [layout, setLayout] = createSignal<Layout>("grid");
  const [sort, setSort] = createSignal<ListSort>({ col: null, dir: "asc" });

  // Bump counter per monitor id, incremented on each `cert_updated` SSE
  // frame. `SslCard` reads it via `certVersion(id)` as part of its
  // `createResource` source tuple so it refetches after a backend-side
  // cert refresh, without the rest of the store needing to know about SSL.
  const [certBump, setCertBump] = createSignal<Record<number, number>>({});

  async function refresh() {
    try {
      const res = await fetch("/api/monitors");
      if (!res.ok) return;
      const monitors = await res.json();
      setState("monitors", reconcile(monitors, { key: "id" }));
    } catch {
      // Network hiccup on initial load — the SSE snapshot frame (sent
      // immediately on connect) will fill this in a moment later.
    }
  }

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

  function connect() {
    const es = new EventSource("/events");
    es.onmessage = (msg) => {
      let frame: StoreEvent;
      try {
        frame = JSON.parse(msg.data);
      } catch {
        return;
      }
      setCertBump((b) => applyCertBump(b, frame));
      // Maintenance overlay (P4.2 §7/M7): a SEPARATE module-level signal,
      // like certBump — not part of StoreState/applyEvent, since the leaf
      // status-dot renderers need to read it without a store instance. A
      // `snapshot` REPLACES the whole set (a resync must reset, not
      // accumulate); `maintenance_changed` patches a single id.
      if (frame.event === "snapshot") {
        setMaintenanceIds(frame.data?.maintenance_ids ?? []);
      } else if (frame.event === "maintenance_changed") {
        patchMaintenance(frame.data.id, frame.data.in_maintenance);
      }
      const next = applyEvent(state, frame);
      setState("online", next.online);
      setState("monitors", reconcile(next.monitors, { key: "id" }));
    };
    return es;
  }

  refresh();
  const source = connect();

  function monitorById(id: number | null | undefined) {
    if (id == null) return undefined;
    return state.monitors.find((m) => m.id === id);
  }

  return {
    monitors: () => state.monitors,
    online: () => state.online,
    monitorById,
    refresh,
    persistReorder,
    close: () => source.close(),
    layout,
    setLayout,
    sort,
    setSort,
    certVersion: (id: number) => certBump()[id] ?? 0,
  };
}
