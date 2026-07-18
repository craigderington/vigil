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

export type StatsRange = "24h" | "7d" | "30d" | "90d";

export interface Stats {
  uptime_pct: number | null;
  downtime_seconds: number;
  avg_ms: number | null;
  incidents: number;
}

export function getStats(id: number, range: StatsRange = "24h"): Promise<Stats> {
  return fetch(`/api/monitors/${id}/stats?range=${range}`).then((r) => json(r));
}

export interface Bar {
  day: string;
  uptime_pct: number;
  incidents: number;
  down_seconds: number;
  has_data: boolean;
}

export function getBars(id: number, days: number): Promise<Bar[]> {
  return fetch(`/api/monitors/${id}/bars?days=${days}`).then((r) => json(r));
}

export interface SeriesPoint {
  t: number;
  ms: number | null;
  status: "up" | "down";
}

/** Bucketed response-time/status series for the detail panel's chart (§11.6 #4). */
export function getSeries(id: number, range: StatsRange = "24h"): Promise<SeriesPoint[]> {
  return fetch(`/api/monitors/${id}/series?range=${range}`).then((r) => json(r));
}

export interface Incident {
  id: number;
  monitor_id: number;
  monitor_name: string;
  started_at: number;
  resolved_at: number | null;
  duration_seconds: number | null;
  cause: string | null;
  status_code: number | null;
  error_message: string | null;
  acknowledged: boolean;
}

/**
 * Global/per-monitor incident timeline. `monitor_id` is only included when
 * `monitorId` is provided, so the global Incidents screen (Task 12) can
 * reuse this for all monitors by omitting it.
 */
export function getIncidents(range?: string, monitorId?: number): Promise<Incident[]> {
  const params = new URLSearchParams();
  if (monitorId != null) params.set("monitor_id", String(monitorId));
  if (range) params.set("range", range);
  const qs = params.toString();
  return fetch(`/api/incidents${qs ? `?${qs}` : ""}`).then((r) => json(r));
}

/** Silences re-notifications on an ongoing incident without resolving it. */
export function acknowledgeIncident(id: number): Promise<{ ok: boolean }> {
  return fetch(`/api/incidents/${id}/acknowledge`, { method: "POST" }).then((r) => json(r));
}
