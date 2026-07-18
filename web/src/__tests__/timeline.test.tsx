import { render, screen, fireEvent } from "@solidjs/testing-library";
import { test, expect, vi } from "vitest";
import IncidentTimeline from "../components/IncidentTimeline";

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
    monitor_id: 1,
    monitor_name: "api.myapp.com",
    started_at: Math.floor(Date.now() / 1000) - 7200,
    resolved_at: Math.floor(Date.now() / 1000) - 7000,
    duration_seconds: 200,
    cause: "status",
    status_code: 503,
    error_message: null,
    acknowledged: true,
  },
];

test("renders ongoing + resolved incidents; acknowledge posts to the ack endpoint", async () => {
  const fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
    if (url.includes("/acknowledge")) {
      return { ok: true, json: async () => ({ ok: true }) } as any;
    }
    if (url.includes("/api/incidents")) {
      return { ok: true, json: async () => incidents } as any;
    }
    return { ok: true, json: async () => [] } as any;
  });
  vi.stubGlobal("fetch", fetchMock);

  render(() => <IncidentTimeline monitorId={1} />);

  // Both incidents render.
  const timeoutRows = await screen.findAllByText(/timeout/i);
  expect(timeoutRows.length).toBeGreaterThan(0);
  expect(await screen.findByText(/connect timed out/i)).toBeTruthy();
  expect(await screen.findByText(/503/)).toBeTruthy();

  // Only the ongoing (unacknowledged) incident shows an Acknowledge button.
  const ackButtons = await screen.findAllByRole("button", { name: /acknowledge/i });
  expect(ackButtons.length).toBe(1);

  fireEvent.click(ackButtons[0]);

  await vi.waitFor(() => {
    const ackCall = fetchMock.mock.calls.find((c) => String(c[0]).includes("/acknowledge"));
    expect(ackCall).toBeTruthy();
    expect(String(ackCall![0])).toContain("/api/incidents/2/acknowledge");
    expect((ackCall![1] as RequestInit)?.method).toBe("POST");
  });
});

test("shows empty state when there are no incidents", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({ ok: true, json: async () => [] })) as any,
  );

  render(() => <IncidentTimeline monitorId={1} />);

  expect(await screen.findByText(/no incidents/i)).toBeTruthy();
});
