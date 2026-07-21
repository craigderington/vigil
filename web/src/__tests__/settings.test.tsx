import { render, screen, fireEvent } from "@solidjs/testing-library"; import { test, expect, vi } from "vitest";
import Settings from "../components/Settings";
test("shows docker-secret note and no password field", () => {
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok:true, json: async () => ({}) })) as any);
  render(() => <Settings />);
  expect(screen.getByText(/managed via Docker secret/i)).toBeTruthy();
  expect(screen.queryByLabelText(/password/i)).toBeNull();
});

test("add-channel form: default email type shows SMTP username; switching to webhook shows URL and hides SMTP host", () => {
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => [] })) as any);
  render(() => <Settings />);

  // default type is email — its config fields (incl. optional username) show
  expect(screen.getByLabelText(/smtp username/i)).toBeTruthy();
  expect(screen.getByLabelText(/smtp host/i)).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "Webhook" }));

  expect(screen.queryByLabelText(/smtp host/i)).toBeNull();
  expect(screen.queryByLabelText(/smtp username/i)).toBeNull();
  expect(screen.getByLabelText(/^url$/i)).toBeTruthy();
});

test("saving a new webhook channel POSTs type=webhook with url in config", async () => {
  const posts: any[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: any, opts?: any) => {
      if (typeof url === "string" && url === "/api/channels" && opts?.method === "POST") {
        posts.push(JSON.parse(opts.body));
        return {
          ok: true,
          json: async () => ({ id: 42, name: "Hook", type: "webhook", config: opts.body, is_active: true, created_at: 0 }),
        };
      }
      return { ok: true, json: async () => [] };
    }) as any,
  );

  render(() => <Settings />);
  fireEvent.click(screen.getByRole("button", { name: "Webhook" }));
  fireEvent.input(screen.getByLabelText(/^url$/i), { target: { value: "https://hooks.example.com/abc" } });
  fireEvent.click(screen.getByRole("button", { name: /create channel/i }));

  await screen.findByText(/channel added/i);

  expect(posts.length).toBe(1);
  expect(posts[0].type).toBe("webhook");
  const cfg = JSON.parse(posts[0].config);
  expect(cfg.url).toBe("https://hooks.example.com/abc");
});

test("saving re-notify hours PUTs renotify_hours", async () => {
  const puts: any[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: any, opts?: any) => {
      if (url === "/api/settings" && opts?.method === "PUT") {
        puts.push(JSON.parse(opts.body));
        return { ok: true, json: async () => ({}) };
      }
      // GET /api/settings and GET /api/channels
      if (url === "/api/settings") {
        return { ok: true, json: async () => ({ anchors: [], retention_days: 30, renotify_hours: 6, digest_enabled: false, digest_time: "08:00", digest_recipients: [] }) };
      }
      return { ok: true, json: async () => [] };
    }) as any,
  );

  render(() => <Settings />);
  const input = await screen.findByLabelText(/re-notify interval/i);
  fireEvent.input(input, { target: { value: "12" } });
  fireEvent.click(screen.getByRole("button", { name: /save re-notify/i }));

  await screen.findByText(/saved/i);
  const put = puts.find((p) => "renotify_hours" in p);
  expect(put).toBeTruthy();
  expect(put.renotify_hours).toBe(12);
});

test("picking an accent swatch PUTs accent and applies --accent", async () => {
  const puts: any[] = [];
  vi.stubGlobal("fetch", vi.fn(async (url: any, opts?: any) => {
    if (url === "/api/settings" && opts?.method === "PUT") { puts.push(JSON.parse(opts.body)); return { ok: true, json: async () => ({}) }; }
    if (url === "/api/settings") return { ok: true, json: async () => ({ anchors: [], retention_days: 30, accent: "cyan" }) };
    return { ok: true, json: async () => [] };
  }) as any);

  render(() => <Settings />);
  const yellow = await screen.findByRole("button", { name: /yellow accent/i });
  fireEvent.click(yellow);

  await screen.findByText(/appearance saved/i);
  expect(puts.some((p) => p.accent === "the-open-yellow")).toBe(true);
  expect(document.documentElement.style.getPropertyValue("--accent").trim()).toBe("#FFBA00");
});
