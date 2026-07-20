import { render, screen, fireEvent } from "@solidjs/testing-library";
import { test, expect, vi } from "vitest";
import Settings from "../components/Settings";

function stubFetch(posts: any[]) {
  vi.stubGlobal("fetch", vi.fn(async (url: any, opts?: any) => {
    if (url === "/api/backup/info") {
      return { ok: true, json: async () => ({ schema_version: 6, db_size_bytes: 4096, generated_at: 0, counts: { monitors: 2, incidents: 0, checks: 0, reports: 0, channels: 1 } }) };
    }
    if (url === "/api/backup/import" && opts?.method === "POST") {
      posts.push(url);
      // pending promise: keeps the component out of its post-success location.reload()
      return new Promise(() => {});
    }
    if (url === "/api/settings") return { ok: true, json: async () => ({ anchors: [], retention_days: 30 }) };
    return { ok: true, json: async () => [] };
  }) as any);
}

test("backup section shows the info readout and a download link", async () => {
  stubFetch([]);
  render(() => <Settings />);
  expect(await screen.findByText(/Backup & restore/i)).toBeTruthy();
  const link = await screen.findByRole("link", { name: /download backup/i });
  expect(link.getAttribute("href")).toBe("/api/backup/export");
  // schema readout renders only after the (3rd, sequential) getBackupInfo() in
  // onMount resolves — await it rather than reading synchronously.
  expect(await screen.findByText(/schema v6/i)).toBeTruthy();
});

test("import requires an explicit confirm before POSTing", async () => {
  const posts: any[] = [];
  stubFetch(posts);
  render(() => <Settings />);

  const fileInput = (await screen.findByLabelText(/choose backup file/i)) as HTMLInputElement;
  const file = new File([new Uint8Array([0x53, 0x51, 0x4c])], "b.db", { type: "application/octet-stream" });
  fireEvent.change(fileInput, { target: { files: [file] } });

  fireEvent.click(screen.getByRole("button", { name: /import & replace/i }));
  // nothing sent until the destructive confirm is clicked
  expect(posts.length).toBe(0);
  fireEvent.click(screen.getByRole("button", { name: /yes, replace everything/i }));
  await Promise.resolve();
  expect(posts).toContain("/api/backup/import");
});
