import { render, screen, fireEvent } from "@solidjs/testing-library";
import { test, expect, vi } from "vitest";
import MonitorGrid from "../components/MonitorGrid";

function stubFetch() {
  vi.stubGlobal("fetch", vi.fn(async (url: string) => {
    const u = String(url);
    if (u.includes("/bars")) return { ok: true, json: async () => [] } as any;
    if (u.includes("/stats")) return { ok: true, json: async () => ({ uptime_pct: null, downtime_seconds: 0, avg_ms: null, incidents: 0 }) } as any;
    return { ok: true, json: async () => [] } as any;
  }));
}

const M = [
  { id: 1, name: "alpha", status: "up" },
  { id: 2, name: "bravo", status: "up" },
  { id: 3, name: "charlie", status: "up" },
];

test("grips are shown when reorder is enabled and hidden when not", async () => {
  stubFetch();
  const { unmount } = render(() => <MonitorGrid monitors={M} onOpen={() => {}} reorderEnabled onReorder={() => {}} />);
  await screen.findByText("alpha");
  expect(screen.getAllByLabelText(/Reorder/).length).toBe(3);
  unmount();

  stubFetch();
  render(() => <MonitorGrid monitors={M} onOpen={() => {}} reorderEnabled={false} onReorder={() => {}} />);
  await screen.findByText("alpha");
  expect(screen.queryByLabelText(/Reorder/)).toBeNull();
});

test("ArrowDown on a card's grip calls onReorder with the nudged order", async () => {
  stubFetch();
  const onReorder = vi.fn();
  render(() => <MonitorGrid monitors={M} onOpen={() => {}} reorderEnabled onReorder={onReorder} />);
  await screen.findByText("alpha");
  const grip = screen.getByLabelText("Reorder alpha (use arrow keys)");
  fireEvent.keyDown(grip, { key: "ArrowDown" });
  expect(onReorder).toHaveBeenCalledWith([2, 1, 3]);
});

test("clicking the grip does not open the card", async () => {
  stubFetch();
  const onOpen = vi.fn();
  render(() => <MonitorGrid monitors={M} onOpen={onOpen} reorderEnabled onReorder={() => {}} />);
  await screen.findByText("alpha");
  fireEvent.click(screen.getByLabelText("Reorder alpha (use arrow keys)"));
  expect(onOpen).not.toHaveBeenCalled();
});
