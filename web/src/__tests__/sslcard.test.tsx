import { render, screen } from "@solidjs/testing-library";
import { test, expect, vi } from "vitest";
import SslCard from "../components/SslCard";

test("renders amber tier, days remaining, and issuer", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({
      ok: true,
      json: async () => ({
        issuer: "Let's Encrypt",
        subject: "api.myapp.com",
        valid_from: 1700000000,
        valid_until: 1701000000,
        days_remaining: 14,
        is_valid: true,
        chain_ok: true,
        hostname_match: true,
        self_signed: false,
        error: null,
        alerted_days: null,
        invalid_alerted: false,
        last_checked: 1700500000,
      }),
    })) as any,
  );

  const { container } = render(() => <SslCard monitorId={1} />);

  expect(await screen.findByText("Let's Encrypt")).toBeTruthy();
  expect(await screen.findByText("14")).toBeTruthy();
  const ring = container.querySelector("[data-tier]");
  expect(ring?.getAttribute("data-tier")).toBe("amber");
});

test("shows the error message when the cert check failed", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({
      ok: true,
      json: async () => ({
        issuer: null,
        subject: null,
        valid_from: null,
        valid_until: null,
        days_remaining: null,
        is_valid: false,
        chain_ok: false,
        hostname_match: false,
        self_signed: false,
        error: "handshake failed: connection refused",
        alerted_days: null,
        invalid_alerted: false,
        last_checked: 1700500000,
      }),
    })) as any,
  );

  render(() => <SslCard monitorId={2} />);

  expect(await screen.findByText("handshake failed: connection refused")).toBeTruthy();
});
