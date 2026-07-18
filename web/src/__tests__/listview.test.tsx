import { render, screen, fireEvent } from "@solidjs/testing-library";
import { test, expect, vi } from "vitest";
import ListView from "../components/ListView";

// sort_order deliberately does NOT match alphabetical name order, so the
// default (sort_order-then-name) render and the post-click (name-sorted)
// render produce different first rows — proving the click actually re-sorts
// rather than coincidentally leaving the same order.
const monitors = [
  { id: 1, name: "Zebra", type: "http", status: "up", sort_order: 1, last_checked_at: null, response_time_ms: 120 },
  { id: 2, name: "Alpha", type: "http", status: "up", sort_order: 2, last_checked_at: null, response_time_ms: 80 },
  { id: 3, name: "Mango", type: "http", status: "down", sort_order: 3, last_checked_at: null, response_time_ms: null },
];

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

test("renders a dense table row per monitor, default-sorted by sort_order then name", async () => {
  stubFetch();
  const { container } = render(() => (
    <ListView monitors={monitors} onOpen={() => {}} onChanged={() => {}} />
  ));

  await screen.findByText("Zebra");

  const rows = Array.from(container.querySelectorAll("[data-monitor-id]"));
  expect(rows.length).toBe(3);
  expect(rows.map((r) => r.getAttribute("data-monitor-id"))).toEqual(["1", "2", "3"]);
});

test("clicking the Name column header sorts alphabetically, changing row order", async () => {
  stubFetch();
  const { container } = render(() => (
    <ListView monitors={monitors} onOpen={() => {}} onChanged={() => {}} />
  ));

  await screen.findByText("Zebra");
  const before = container.querySelector("[data-monitor-id]")?.getAttribute("data-monitor-id");
  expect(before).toBe("1"); // Zebra first, by sort_order

  fireEvent.click(screen.getByText("Name"));

  await vi.waitFor(() => {
    const after = container.querySelector("[data-monitor-id]")?.getAttribute("data-monitor-id");
    expect(after).toBe("2"); // Alpha first, alphabetically
    expect(after).not.toBe(before);
  });
});

test("row click opens the detail panel", async () => {
  stubFetch();
  const onOpen = vi.fn();
  render(() => <ListView monitors={monitors} onOpen={onOpen} onChanged={() => {}} />);

  await screen.findByText("Zebra");
  fireEvent.click(screen.getByText("Zebra"));
  expect(onOpen).toHaveBeenCalledWith(1);
});
