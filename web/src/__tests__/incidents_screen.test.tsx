import { render, screen } from "@solidjs/testing-library";
import { test, expect, vi } from "vitest";
import Incidents from "../components/Incidents";

const incidents = [
  {
    id: 2,
    monitor_id: 1,
    monitor_name: "api.myapp.com",
    started_at: Math.floor(Date.now() / 1000) - 300,
    resolved_at: null,
    duration_seconds: null,
    cause: "timeout",
    status_code: null,
    error_message: "connect timed out",
    acknowledged: false,
  },
  {
    id: 1,
    monitor_id: 2,
    monitor_name: "web.myapp.com",
    started_at: Math.floor(Date.now() / 1000) - 7200,
    resolved_at: Math.floor(Date.now() / 1000) - 7000,
    duration_seconds: 200,
    cause: "status",
    status_code: 503,
    error_message: null,
    acknowledged: true,
  },
];

test("renders global incidents across monitors and shows open-incident header stat", async () => {
  const fetchMock = vi.fn(async (url: string) => {
    if (String(url).includes("/api/incidents")) {
      // No monitor_id param — the global screen must fetch ALL monitors' incidents.
      expect(String(url)).not.toContain("monitor_id");
      return { ok: true, json: async () => incidents } as any;
    }
    return { ok: true, json: async () => [] } as any;
  });
  vi.stubGlobal("fetch", fetchMock);

  render(() => <Incidents />);

  // Both incidents (from different monitors) render.
  expect(await screen.findByText("api.myapp.com")).toBeTruthy();
  expect(await screen.findByText("web.myapp.com")).toBeTruthy();

  // Header stat: 1 open incident (resolved_at === null).
  const openStat = await screen.findByTestId("stat-open");
  expect(openStat.textContent).toContain("1");
});

test("shows empty state when there are no incidents", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({ ok: true, json: async () => [] })) as any,
  );

  render(() => <Incidents />);

  expect(await screen.findByText(/no incidents/i)).toBeTruthy();
});
