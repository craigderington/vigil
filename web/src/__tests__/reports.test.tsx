import { render, screen, fireEvent } from "@solidjs/testing-library";
import { test, expect, vi } from "vitest";
import Reports from "../components/Reports";

function stub(reports: any[]) {
  const posts: any[] = [];
  vi.stubGlobal("fetch", vi.fn(async (url: any, opts?: any) => {
    const u = String(url);
    if (u === "/api/reports" && (!opts || !opts.method)) return { ok: true, json: async () => reports } as any;
    if (u === "/api/reports/generate" && opts?.method === "POST") { posts.push(JSON.parse(opts.body)); return { ok: true, json: async () => ({ id: 9, label: "March 2026", period_start: 0 }) } as any; }
    if (u === "/api/channels") return { ok: true, json: async () => [] } as any;
    return { ok: true, json: async () => [] } as any;
  }) as any);
  return posts;
}

test("renders month cards and generates a report", async () => {
  const posts = stub([{ id: 1, label: "February 2026", period_start: 100, generated_at: 0, emailed_at: null, headline: { uptime_pct: 99.9, incidents: 1, downtime_seconds: 60 } }]);
  render(() => <Reports />);
  expect(await screen.findByText("February 2026")).toBeTruthy();
  fireEvent.input(screen.getByLabelText(/month/i), { target: { value: "2026-03" } });
  fireEvent.click(screen.getByRole("button", { name: /generate/i }));
  await vi.waitFor(() => expect(posts.length).toBe(1));
  expect(posts[0].period).toBe("2026-03");
});

test("empty state", async () => {
  stub([]);
  render(() => <Reports />);
  expect(await screen.findByText(/no reports yet/i)).toBeTruthy();
});
