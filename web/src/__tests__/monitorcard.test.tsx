import { render, screen } from "@solidjs/testing-library";
import { test, expect, vi, beforeEach } from "vitest";
import MonitorCard from "../components/MonitorCard";
import { setMaintenanceIds } from "../maintenance_ids";

/**
 * Regression coverage for the maintenance overlay actually reaching the
 * rendered dot (finding 2, task 7 review): `maintenance_ids.ts`'s
 * `displayStatusWith` precedence is unit-tested directly in
 * `store.test.ts`, but nothing previously rendered `MonitorCard` with an
 * in-maintenance monitor and asserted the DOM. This locks in the wiring
 * through `statusClass()` in MonitorCard.tsx.
 */

function stubFetch() {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string) => {
      const u = String(url);
      if (u.includes("/bars")) return { ok: true, json: async () => [] } as any;
      if (u.includes("/stats")) {
        return {
          ok: true,
          json: async () => ({ uptime_pct: null, downtime_seconds: 0, avg_ms: null, incidents: 0 }),
        } as any;
      }
      return { ok: true, json: async () => [] } as any;
    }),
  );
}

beforeEach(() => {
  // Reset the module-level signal between tests so one test's maintenance
  // set can't leak into the next (this signal lives outside the Solid
  // store instance, per maintenance_ids.ts's design note).
  setMaintenanceIds([]);
});

test("a monitor whose id is in the maintenance set renders a status-dot with the maintenance class", async () => {
  stubFetch();
  setMaintenanceIds([42]);

  const { container } = render(() => (
    <MonitorCard monitor={{ id: 42, name: "api.myapp.com", status: "up" }} onOpen={() => {}} />
  ));

  await screen.findByText("api.myapp.com");
  const dot = container.querySelector(".status-dot");
  expect(dot?.className).toContain("maintenance");
  expect(dot?.className).not.toContain("paused");
});

test("a paused monitor inside the maintenance set still renders paused (precedence)", async () => {
  stubFetch();
  setMaintenanceIds([42]);

  const { container } = render(() => (
    <MonitorCard monitor={{ id: 42, name: "api.myapp.com", status: "up", is_paused: true }} onOpen={() => {}} />
  ));

  await screen.findByText("api.myapp.com");
  const dot = container.querySelector(".status-dot");
  expect(dot?.className).toContain("paused");
  expect(dot?.className).not.toContain("maintenance");
});

test("a monitor NOT in the maintenance set renders its real status", async () => {
  stubFetch();
  setMaintenanceIds([42]);

  const { container } = render(() => (
    <MonitorCard monitor={{ id: 7, name: "other.example.com", status: "down" }} onOpen={() => {}} />
  ));

  await screen.findByText("other.example.com");
  const dot = container.querySelector(".status-dot");
  expect(dot?.className).toContain("down");
  expect(dot?.className).not.toContain("maintenance");
});
