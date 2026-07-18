import { render } from "@solidjs/testing-library";
import { test, expect, vi } from "vitest";
import ResponseChart from "../components/ResponseChart";

test("renders empty-state and does not throw when series is empty (jsdom guard)", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string) => {
      if (url.includes("/series")) {
        return { ok: true, json: async () => [] } as any;
      }
      if (url.includes("/incidents")) {
        return { ok: true, json: async () => [] } as any;
      }
      return { ok: true, json: async () => [] } as any;
    }),
  );

  const { findByText } = render(() => <ResponseChart monitorId={1} />);

  expect(await findByText(/no response/i)).toBeTruthy();
});
