import { test, expect } from "vitest"; import { applyEvent, applyCertBump, type StoreState } from "../store";
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
