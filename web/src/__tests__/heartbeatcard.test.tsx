import { render, screen } from "@solidjs/testing-library";
import { test, expect, vi } from "vitest";
import HeartbeatCard from "../components/HeartbeatCard";

test("renders the ping URL, last ping, and next expected by", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({
      ok: true,
      json: async () => ({ token: "abc123", ping_path: "/ping/abc123" }),
    })) as any,
  );

  const monitor = {
    id: 1,
    type: "heartbeat",
    interval_seconds: 300,
    heartbeat_grace_seconds: 60,
    last_ping_at: Math.floor(Date.now() / 1000) - 30,
  };

  render(() => <HeartbeatCard monitor={monitor as any} />);

  const matches = await screen.findAllByText(/\/ping\/abc123/);
  expect(matches.length).toBeGreaterThan(0);
  expect(await screen.findByText(/ago$/)).toBeTruthy();
  expect(screen.getByText("Next expected by")).toBeTruthy();
  expect(screen.queryByText("Waiting for first ping")).toBeNull();
});

test("shows 'Waiting for first ping' when last_ping_at is null", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({
      ok: true,
      json: async () => ({ token: "xyz", ping_path: "/ping/xyz" }),
    })) as any,
  );

  const monitor = {
    id: 2,
    type: "heartbeat",
    interval_seconds: 300,
    heartbeat_grace_seconds: 60,
    last_ping_at: null,
  };

  render(() => <HeartbeatCard monitor={monitor as any} />);

  expect(await screen.findByText("Waiting for first ping")).toBeTruthy();
});
