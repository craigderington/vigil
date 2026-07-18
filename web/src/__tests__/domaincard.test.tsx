import { render, screen } from "@solidjs/testing-library";
import { test, expect, vi } from "vitest";
import DomainCard from "../components/DomainCard";

test("renders green tier and registrar", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({
      ok: true,
      json: async () => ({
        registrar: "NameCheap",
        expiry_date: 1900000000,
        days_remaining: 60,
        name_servers: "ns1,ns2",
        status_codes: "clientTransferProhibited",
        queryable: true,
        source: "rdap",
        alerted_days: null,
        last_checked: 1700500000,
      }),
    })) as any,
  );

  const { container } = render(() => <DomainCard monitorId={1} />);

  expect(await screen.findByText("NameCheap")).toBeTruthy();
  const ring = container.querySelector("[data-tier]");
  expect(ring?.getAttribute("data-tier")).toBe("green");
});

test("shows a not-queryable note instead of the ring when the registry can't be queried", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({
      ok: true,
      json: async () => ({
        registrar: null,
        expiry_date: null,
        days_remaining: null,
        name_servers: null,
        status_codes: null,
        queryable: false,
        source: null,
        alerted_days: null,
        last_checked: 1700500000,
      }),
    })) as any,
  );

  render(() => <DomainCard monitorId={2} />);

  expect(await screen.findByText(/not queryable/i)).toBeTruthy();
});
