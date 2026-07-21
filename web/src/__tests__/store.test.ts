import { test, expect, vi } from "vitest"; import { applyEvent, applyCertBump, computeReorder, moveByOffset, reorderState, createMonitorStore, type StoreState } from "../store";
import { displayStatusWith, setMaintenanceIds, patchMaintenance, inMaintenance } from "../maintenance_ids";
test("snapshot replaces then delta patches", () => {
  let s: StoreState = { monitors: [], online: true };
  s = applyEvent(s, { event: "snapshot", data: { monitors: [{ id:1, status:"up" } as any], online:true }});
  expect(s.monitors.length).toBe(1);
  s = applyEvent(s, { event: "monitor_updated", data: { id:1, status:"down", response_time_ms:10, checked_at:0 }});
  expect(s.monitors[0].status).toBe("down");
});

test("cert_updated frame bumps certVersion(id)", () => {
  let bump: Record<number, number> = {};
  bump = applyCertBump(bump, { event: "cert_updated", data: { id: 5 } });
  expect(bump[5]).toBe(1);
  bump = applyCertBump(bump, { event: "cert_updated", data: { id: 5 } });
  expect(bump[5]).toBe(2);
  // Unrelated frames must not bump the counter.
  bump = applyCertBump(bump, { event: "monitor_updated", data: { id: 5 } });
  expect(bump[5]).toBe(2);
});

// ---- maintenanceIds module signal (P4.2 §7/M7): store.ts's es.onmessage
// wires a `snapshot` frame's `maintenance_ids` to `setMaintenanceIds`
// (REPLACE) and a `maintenance_changed` frame to `patchMaintenance` (single
// add/remove) — see store.ts. Those two calls are a trivial pass-through of
// frame.data, so the meaningful behavior to prove is in the module signal
// itself, imported directly from maintenance_ids.ts (kept OUTSIDE
// applyEvent/StoreState, like certBump, per M7).

test("a snapshot's maintenance_ids REPLACES the set — a resync must reset, not accumulate", () => {
  setMaintenanceIds([1]);
  expect(inMaintenance(1)).toBe(true);
  // A later snapshot frame with a DIFFERENT set must fully replace it, not
  // merge with the previous one (otherwise a monitor pulled out of
  // maintenance server-side would incorrectly stay flagged forever).
  setMaintenanceIds([2, 3]);
  expect(inMaintenance(1)).toBe(false);
  expect(inMaintenance(2)).toBe(true);
  expect(inMaintenance(3)).toBe(true);
});

test("a maintenance_changed frame adds/removes a single id without touching the rest", () => {
  setMaintenanceIds([1, 2]);
  patchMaintenance(1, false); // {id:1, in_maintenance:false}
  expect(inMaintenance(1)).toBe(false);
  expect(inMaintenance(2)).toBe(true);
  patchMaintenance(5, true); // {id:5, in_maintenance:true}
  expect(inMaintenance(5)).toBe(true);
  expect(inMaintenance(2)).toBe(true);
});

test("displayStatusWith: paused > maintenance > real status precedence", () => {
  const ids = new Set([1, 2]);
  // In the maintenance set, not paused -> "maintenance".
  expect(displayStatusWith({ id: 1, status: "up" }, ids)).toBe("maintenance");
  // In the maintenance set AND paused -> "paused" wins.
  expect(displayStatusWith({ id: 2, is_paused: true, status: "up" }, ids)).toBe("paused");
  // Not in the set -> real status passes through untouched.
  expect(displayStatusWith({ id: 3, status: "down" }, ids)).toBe("down");
});

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
