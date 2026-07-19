import { test, expect } from "vitest"; import { applyEvent, applyCertBump, type StoreState } from "../store";
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
