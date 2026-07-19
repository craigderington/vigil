/**
 * Module-level "which monitors are currently under an active maintenance
 * window" set (P4.2 §7/M7). Maintenance is a CLIENT-SIDE display overlay —
 * the backend keeps `monitor.status` truthful (never "maintenance") and
 * instead publishes a `maintenance_ids` set on the SSE `snapshot` frame plus
 * `maintenance_changed` deltas (see `store.ts`'s `es.onmessage`).
 *
 * This lives OUTSIDE the Solid store instance (module-level `createSignal`,
 * like `certBump` conceptually but even more so) because the leaf
 * `statusClass`/dot-rendering helpers in `MonitorCard.tsx` and
 * `ListView.tsx` are plain functions with no access to a store instance —
 * they can only read a signal that exists at import time.
 */
import { createSignal } from "solid-js";

const [ids, setIds] = createSignal<Set<number>>(new Set());

/** Read-only accessor for the raw signal, for components that want to
 *  subscribe to the whole set (e.g. Rail's summary tally). */
export const maintenanceIds = ids;

/** REPLACES the set — called on every SSE `snapshot` frame (a resync must
 *  reset, not accumulate: S6). */
export function setMaintenanceIds(list: number[]): void {
  setIds(new Set(list));
}

/** Adds/removes a single id — called on each `maintenance_changed` frame. */
export function patchMaintenance(id: number, on: boolean): void {
  setIds((prev) => {
    const next = new Set(prev);
    if (on) next.add(id);
    else next.delete(id);
    return next;
  });
}

export const inMaintenance = (id: number): boolean => ids().has(id);

/**
 * PURE precedence rule, taking the id set as a parameter so it's testable
 * without touching the module signal (`store.test.ts` asserts this
 * directly). Precedence: paused > maintenance > real status — a paused
 * monitor stays "paused" even if it also falls inside an active
 * maintenance window.
 */
export function displayStatusWith(
  m: { id: number; is_paused?: boolean; status: string },
  set: Set<number>,
): string {
  return m.is_paused ? "paused" : set.has(m.id) ? "maintenance" : m.status;
}

/** Convenience wrapper reading the live module signal — what components use. */
export function displayStatus(m: { id: number; is_paused?: boolean; status: string }): string {
  return displayStatusWith(m, ids());
}
