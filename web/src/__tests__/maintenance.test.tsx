import { render, screen, fireEvent } from "@solidjs/testing-library";
import { test, expect, vi } from "vitest";
import Maintenance from "../components/Maintenance";

const sampleWindows = [
  {
    id: 1,
    name: "Router upgrade",
    scope: "all",
    target_ref: null,
    starts_at: 1000,
    ends_at: 2000,
    recurrence: null,
    suppress: "alerts",
    is_active: true,
    created_at: 0,
  },
];

function stubFetch(handlers: { list?: any[]; onPost?: (url: string, body: any) => any }) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: any, opts?: any) => {
      const u = String(url);
      if (u === "/api/maintenance-windows" && (!opts || opts.method === undefined)) {
        return { ok: true, json: async () => handlers.list ?? [] } as any;
      }
      if (u === "/api/maintenance-windows" && opts?.method === "POST") {
        const body = JSON.parse(opts.body);
        const result = handlers.onPost?.(u, body) ?? { id: 99, ...body, is_active: true, created_at: 0 };
        return { ok: true, json: async () => result } as any;
      }
      if (u === "/api/maintenance-windows/preview") {
        return { ok: true, json: async () => ({ affected_monitor_ids: [], active_now: null }) } as any;
      }
      if (u.startsWith("/api/maintenance-windows/") && opts?.method === "DELETE") {
        return { ok: true, json: async () => ({ ok: true }) } as any;
      }
      return { ok: true, json: async () => [] } as any;
    }) as any,
  );
}

test("renders a window list and the create form", async () => {
  stubFetch({ list: sampleWindows });
  render(() => <Maintenance />);

  expect(await screen.findByText("Router upgrade")).toBeTruthy();
  expect(screen.getByLabelText("Name")).toBeTruthy();
  expect(screen.getByRole("button", { name: /create window/i })).toBeTruthy();
});

test("shows an empty state when there are no windows yet", async () => {
  stubFetch({ list: [] });
  render(() => <Maintenance />);
  expect(await screen.findByText(/no maintenance windows yet/i)).toBeTruthy();
});

test("scope picker toggles tag vs monitors inputs", async () => {
  stubFetch({ list: [] });
  render(() => <Maintenance />);
  await screen.findByText(/no maintenance windows yet/i);

  // Default scope "all" — neither field shown.
  expect(screen.queryByLabelText("Tag")).toBeNull();
  expect(screen.queryByLabelText(/monitor ids/i)).toBeNull();

  fireEvent.click(screen.getByRole("button", { name: "Tag" }));
  expect(screen.getByLabelText("Tag")).toBeTruthy();
  expect(screen.queryByLabelText(/monitor ids/i)).toBeNull();

  fireEvent.click(screen.getByRole("button", { name: "Monitors" }));
  expect(screen.queryByLabelText("Tag")).toBeNull();
  expect(screen.getByLabelText(/monitor ids/i)).toBeTruthy();
});

test("one-off vs recurring toggle swaps datetime fields for a cron field", async () => {
  stubFetch({ list: [] });
  render(() => <Maintenance />);
  await screen.findByText(/no maintenance windows yet/i);

  // Default mode "one-off" — start/end datetime inputs, no cron field.
  expect(screen.getByLabelText("Starts at")).toBeTruthy();
  expect(screen.getByLabelText("Ends at")).toBeTruthy();
  expect(screen.queryByLabelText(/cron expression/i)).toBeNull();

  fireEvent.click(screen.getByRole("button", { name: "Recurring" }));

  expect(screen.queryByLabelText("Starts at")).toBeNull();
  expect(screen.queryByLabelText("Ends at")).toBeNull();
  expect(screen.getByLabelText(/cron expression/i)).toBeTruthy();
  expect(screen.getByLabelText(/recurs from/i)).toBeTruthy();
  expect(screen.getByLabelText(/duration/i)).toBeTruthy();
});

test("saving a one-off window calls createMaintenanceWindow with the right DTO", async () => {
  const posts: any[] = [];
  stubFetch({
    list: [],
    onPost: (_u, body) => {
      posts.push(body);
      return { id: 7, ...body, is_active: true, created_at: 0 };
    },
  });
  render(() => <Maintenance />);
  await screen.findByText(/no maintenance windows yet/i);

  fireEvent.input(screen.getByLabelText("Name"), { target: { value: "DB failover drill" } });
  fireEvent.click(screen.getByRole("button", { name: "Tag" }));
  fireEvent.input(screen.getByLabelText("Tag"), { target: { value: "prod" } });
  fireEvent.input(screen.getByLabelText("Starts at"), { target: { value: "2026-08-01T02:00" } });
  fireEvent.input(screen.getByLabelText("Ends at"), { target: { value: "2026-08-01T04:00" } });
  fireEvent.click(screen.getByLabelText("Checks"));

  fireEvent.click(screen.getByRole("button", { name: /create window/i }));

  await screen.findByText(/window created/i);

  expect(posts.length).toBe(1);
  const dto = posts[0];
  expect(dto.name).toBe("DB failover drill");
  expect(dto.scope).toBe("tag");
  expect(dto.target_ref).toBe("prod");
  expect(dto.suppress).toBe("checks");
  expect(typeof dto.starts_at).toBe("number");
  expect(typeof dto.ends_at).toBe("number");
  expect(dto.ends_at).toBeGreaterThan(dto.starts_at);
  // one-off: no recurrence field sent
  expect(dto.recurrence).toBeUndefined();
});

test("saving a recurring window sends a recurrence string and monitors[] target_ref", async () => {
  const posts: any[] = [];
  stubFetch({
    list: [],
    onPost: (_u, body) => {
      posts.push(body);
      return { id: 8, ...body, is_active: true, created_at: 0 };
    },
  });
  render(() => <Maintenance />);
  await screen.findByText(/no maintenance windows yet/i);

  fireEvent.input(screen.getByLabelText("Name"), { target: { value: "Nightly patch window" } });
  fireEvent.click(screen.getByRole("button", { name: "Monitors" }));
  fireEvent.input(screen.getByLabelText(/monitor ids/i), { target: { value: "1, 2, 3" } });
  fireEvent.click(screen.getByRole("button", { name: "Recurring" }));
  fireEvent.input(screen.getByLabelText(/cron expression/i), { target: { value: "0 2 * * *" } });
  fireEvent.input(screen.getByLabelText(/recurs from/i), { target: { value: "2026-08-01T02:00" } });
  fireEvent.input(screen.getByLabelText(/duration/i), { target: { value: "30" } });

  fireEvent.click(screen.getByRole("button", { name: /create window/i }));

  await screen.findByText(/window created/i);

  expect(posts.length).toBe(1);
  const dto = posts[0];
  expect(dto.scope).toBe("monitors");
  expect(dto.target_ref).toEqual([1, 2, 3]);
  expect(dto.recurrence).toBe("0 2 * * *");
  // duration is minutes -> ends_at - starts_at should be 30*60 seconds
  expect(dto.ends_at - dto.starts_at).toBe(30 * 60);
});
