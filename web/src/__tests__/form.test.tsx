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
