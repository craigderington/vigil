/** Thin fetch wrappers around the `/api/*` REST surface (crates/vigil/src/api). */

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) {
    const body = await res.text().catch(() => "");
    throw new Error(`${res.status} ${res.statusText}${body ? `: ${body}` : ""}`);
  }
  return res.json() as Promise<T>;
}

export function listMonitors(): Promise<any[]> {
  return fetch("/api/monitors").then((r) => json(r));
}

export function getMonitor(id: number): Promise<any> {
  return fetch(`/api/monitors/${id}`).then((r) => json(r));
}

export function pauseMonitor(id: number): Promise<{ ok: boolean }> {
  return fetch(`/api/monitors/${id}/pause`, { method: "POST" }).then((r) => json(r));
}

export function resumeMonitor(id: number): Promise<{ ok: boolean }> {
  return fetch(`/api/monitors/${id}/resume`, { method: "POST" }).then((r) => json(r));
}

export function checkNow(id: number): Promise<{ ok: boolean }> {
  return fetch(`/api/monitors/${id}/check-now`, { method: "POST" }).then((r) => json(r));
}

export function deleteMonitor(id: number): Promise<{ ok: boolean }> {
  return fetch(`/api/monitors/${id}`, { method: "DELETE" }).then((r) => json(r));
}

export function createMonitor(dto: any): Promise<any> {
  return fetch("/api/monitors", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(dto),
  }).then((r) => json(r));
}

export function testCheck(dto: any): Promise<any> {
  return fetch("/api/monitors/test-check", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(dto),
  }).then((r) => json(r));
}

export function updateMonitor(id: number, dto: any): Promise<any> {
  return fetch(`/api/monitors/${id}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(dto),
  }).then((r) => json(r));
}

export function getChannels(): Promise<any[]> {
  return fetch("/api/channels").then((r) => json(r));
}

export function createChannel(dto: { name: string; type: string; config: any }): Promise<any> {
  return fetch("/api/channels", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(dto),
  }).then((r) => json(r));
}

export function updateChannel(
  id: number,
  dto: { name?: string; config?: any; is_active?: boolean },
): Promise<any> {
  return fetch(`/api/channels/${id}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(dto),
  }).then((r) => json(r));
}

export function testChannel(id: number): Promise<{ ok: boolean; error?: string | null }> {
  return fetch(`/api/channels/${id}/test`, { method: "POST" }).then((r) => json(r));
}

export interface Settings {
  anchors: string[];
  cooldown_minutes: number;
  retention_days: number;
  accent: string;
}

export function getSettings(): Promise<Settings> {
  return fetch("/api/settings").then((r) => json(r));
}

export function updateSettings(patch: Partial<Settings>): Promise<Settings> {
  return fetch("/api/settings", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(patch),
  }).then((r) => json(r));
}

export interface MonitorNotification {
  channel_id: number;
  triggers: string[];
}

export function getMonitorNotifications(id: number): Promise<MonitorNotification[]> {
  return fetch(`/api/monitors/${id}/notifications`).then((r) => json(r));
}

export function setMonitorNotifications(
  id: number,
  list: MonitorNotification[],
): Promise<{ ok: boolean }> {
  return fetch(`/api/monitors/${id}/notifications`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(list),
  }).then((r) => json(r));
}

export type StatsRange = "24h" | "7d";

export interface Stats {
  uptime_pct: number | null;
  downtime_seconds: number;
  avg_ms: number | null;
  incidents: number;
}

export function getStats(id: number, range: StatsRange = "24h"): Promise<Stats> {
  return fetch(`/api/monitors/${id}/stats?range=${range}`).then((r) => json(r));
}
