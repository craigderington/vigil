import { render, screen } from "@solidjs/testing-library"; import { test, expect, vi } from "vitest";
import DetailPanel from "../components/DetailPanel";
test("uptime tile shows dash when null", async () => {
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok:true, json: async () => ({ uptime_pct:null, downtime_seconds:0, avg_ms:null, incidents:0 }) })) as any);
  render(() => <DetailPanel monitor={{ id:1, name:"x", status:"pending" } as any} onClose={()=>{}} />);
  expect(await screen.findByText("—")).toBeTruthy();
});
