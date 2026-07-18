import { render, screen } from "@solidjs/testing-library"; import { test, expect, vi } from "vitest";
import Settings from "../components/Settings";
test("shows docker-secret note and no password field", () => {
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok:true, json: async () => ({}) })) as any);
  render(() => <Settings />);
  expect(screen.getByText(/managed via Docker secret/i)).toBeTruthy();
  expect(screen.queryByLabelText(/password/i)).toBeNull();
});
