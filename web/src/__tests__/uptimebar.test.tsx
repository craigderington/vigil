import { render } from "@solidjs/testing-library";
import { test, expect, vi } from "vitest";
import UptimeBar from "../components/UptimeBar";

const bars = [
  { day: "2026-01-01", uptime_pct: 100, incidents: 0, down_seconds: 0, has_data: true },
  { day: "2026-01-02", uptime_pct: 40, incidents: 2, down_seconds: 5000, has_data: true },
  { day: "2026-01-03", uptime_pct: 100, incidents: 0, down_seconds: 0, has_data: false },
];

test("renders one segment per bar with color tier reflecting up/down/no-data", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({ ok: true, json: async () => bars })) as any,
  );

  const { container, findAllByTestId } = render(() => <UptimeBar monitorId={1} />);

  const segments = await findAllByTestId("uptime-segment");
  expect(segments.length).toBe(3);
  const tiered = container.querySelectorAll("[data-tier]");
  expect(tiered.length).toBe(3);
  expect(tiered[0].getAttribute("data-tier")).toBe("up");
  expect(tiered[1].getAttribute("data-tier")).toBe("down");
  expect(tiered[2].getAttribute("data-tier")).toBe("nodata");
});
