import { render, screen, fireEvent } from "@solidjs/testing-library"; import { test, expect, vi } from "vitest";
import MonitorForm from "../components/MonitorForm";
test("test-check calls api and shows result", async () => {
  const testCheck = vi.fn(async () => ({ ok:true, status_code:200, response_time_ms:12 }));
  render(() => <MonitorForm api={{ testCheck } as any} onSaved={()=>{}} onClose={()=>{}} />);
  fireEvent.input(screen.getByLabelText("Name"), { target:{ value:"x" }});
  fireEvent.input(screen.getByLabelText("URL"), { target:{ value:"https://e.com" }});
  fireEvent.click(screen.getByText("Test check"));
  expect(testCheck).toHaveBeenCalled();
  expect(await screen.findByText(/200/)).toBeTruthy();
});

test("clamps a below-floor custom interval to 15 on save", async () => {
  const createMonitor = vi.fn(async (_dto: any) => ({ id: 1 }));
  render(() => <MonitorForm api={{ createMonitor } as any} onSaved={()=>{}} onClose={()=>{}} />);
  fireEvent.input(screen.getByLabelText("Name"), { target:{ value:"x" }});
  fireEvent.input(screen.getByLabelText("URL"), { target:{ value:"https://e.com" }});
  // set a custom interval below the floor, then save
  const custom = screen.getByLabelText(/custom interval/i);
  fireEvent.input(custom, { target:{ value:"5" }});
  fireEvent.click(screen.getByText(/create monitor/i));
  await Promise.resolve();
  expect(createMonitor).toHaveBeenCalled();
  const dto = createMonitor.mock.calls[0][0];
  expect(dto.interval_seconds).toBeGreaterThanOrEqual(15);
});
