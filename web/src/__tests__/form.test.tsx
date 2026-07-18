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

test("type selector: switching to port shows host+port and hides url; saves a port dto", async () => {
  const createMonitor = vi.fn(async (_dto: any) => ({ id: 2 }));
  render(() => <MonitorForm api={{ createMonitor } as any} onSaved={()=>{}} onClose={()=>{}} />);

  fireEvent.input(screen.getByLabelText("Name"), { target:{ value:"db" }});

  // default type is http — url field present, host/port not
  expect(screen.getByLabelText("URL")).toBeTruthy();
  expect(screen.queryByLabelText("Host")).toBeNull();

  fireEvent.click(screen.getByRole("button", { name: "Port" }));

  expect(screen.queryByLabelText("URL")).toBeNull();
  const host = screen.getByLabelText("Host");
  const port = screen.getByLabelText("Port");
  expect(host).toBeTruthy();
  expect(port).toBeTruthy();

  fireEvent.input(host, { target:{ value:"db.internal" }});
  fireEvent.input(port, { target:{ value:"5432" }});

  fireEvent.click(screen.getByText(/create monitor/i));
  await Promise.resolve();

  expect(createMonitor).toHaveBeenCalled();
  const dto = createMonitor.mock.calls[0][0];
  expect(dto.type).toBe("port");
  expect(dto.host).toBe("db.internal");
  expect(dto.port).toBe(5432);
});

test("https url: enabling SSL toggle shows alert-days editor and saves ssl_check_enabled", async () => {
  const createMonitor = vi.fn(async (_dto: any) => ({ id: 3 }));
  render(() => <MonitorForm api={{ createMonitor } as any} onSaved={()=>{}} onClose={()=>{}} />);

  fireEvent.input(screen.getByLabelText("Name"), { target:{ value:"secure" }});
  fireEvent.input(screen.getByLabelText("URL"), { target:{ value:"https://x" }});

  const sslToggle = screen.getByLabelText(/enable ssl/i);
  expect(screen.queryByLabelText(/ssl alert days/i)).toBeNull();
  fireEvent.click(sslToggle);
  const sslDays = screen.getByLabelText(/ssl alert days/i);
  expect(sslDays).toBeTruthy();

  fireEvent.click(screen.getByText(/create monitor/i));
  await Promise.resolve();

  expect(createMonitor).toHaveBeenCalled();
  const dto = createMonitor.mock.calls[0][0];
  expect(dto.ssl_check_enabled).toBe(true);
  expect(dto.ssl_alert_days).toBe("[30,14,7,3,1]");
});

test("type selector: switching to ssl shows host+port fields and forces SSL toggle on", async () => {
  const createMonitor = vi.fn(async (_dto: any) => ({ id: 4 }));
  render(() => <MonitorForm api={{ createMonitor } as any} onSaved={()=>{}} onClose={()=>{}} />);

  fireEvent.input(screen.getByLabelText("Name"), { target:{ value:"cert" }});
  fireEvent.click(screen.getByRole("button", { name: "SSL" }));

  const host = screen.getByLabelText("Host");
  const port = screen.getByLabelText("Port");
  expect(host).toBeTruthy();
  expect(port).toBeTruthy();

  const sslToggle = screen.getByLabelText(/enable ssl/i) as HTMLInputElement;
  expect(sslToggle.checked).toBe(true);
  expect(sslToggle.disabled).toBe(true);

  fireEvent.input(host, { target:{ value:"example.com" }});
  fireEvent.input(port, { target:{ value:"443" }});

  fireEvent.click(screen.getByText(/create monitor/i));
  await Promise.resolve();

  expect(createMonitor).toHaveBeenCalled();
  const dto = createMonitor.mock.calls[0][0];
  expect(dto.type).toBe("ssl");
  expect(dto.ssl_check_enabled).toBe(true);
});
