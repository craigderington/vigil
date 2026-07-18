import { createResource, createSignal, For, Show, type Component } from "solid-js";
import * as api from "../api";
import type { Incident, StatsRange } from "../api";

const RANGES: StatsRange[] = ["24h", "7d", "30d", "90d"];

const CAUSE_LABEL: Record<string, string> = {
  timeout: "Timeout",
  status: "Bad status",
  connection: "Connection",
  dns: "DNS",
  keyword: "Keyword",
  ssl: "SSL",
  heartbeat: "Heartbeat missed",
};

function causeLabel(cause: string | null): string {
  if (!cause) return "Unknown";
  return CAUSE_LABEL[cause] ?? cause;
}

/** Humane relative-time string from an epoch-seconds timestamp. */
function relativeFrom(epochSeconds: number | null | undefined): string {
  if (epochSeconds == null) return "";
  const deltaMs = Date.now() - epochSeconds * 1000;
  const s = Math.max(0, Math.round(deltaMs / 1000));
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  const d = Math.floor(h / 24);
  return `${d}d ago`;
}

function absoluteFrom(epochSeconds: number | null | undefined): string | undefined {
  if (epochSeconds == null) return undefined;
  return new Date(epochSeconds * 1000).toLocaleString();
}

/** Humane duration formatting: "0s", "5m 12s", "2h 3m", "1d 4h". */
function formatDuration(totalSeconds: number | null | undefined): string {
  if (totalSeconds == null) return "0s";
  const s = Math.max(0, Math.round(totalSeconds));
  if (s === 0) return "0s";
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${sec}s`;
  return `${sec}s`;
}

/**
 * Global Incidents screen (spec §11.8): a fleet-wide timeline across all
 * monitors, filterable by range, with header stats (open incidents, MTTR,
 * count-in-range) and inline acknowledge.
 */
const Incidents: Component = () => {
  const [range, setRange] = createSignal<StatsRange>("30d");

  const [incidents, { refetch }] = createResource(
    () => range(),
    (r) => api.getIncidents(r).catch(() => [] as Incident[]),
  );

  const [ackingId, setAckingId] = createSignal<number | null>(null);

  async function acknowledge(id: number) {
    setAckingId(id);
    try {
      await api.acknowledgeIncident(id);
      await refetch();
    } finally {
      setAckingId(null);
    }
  }

  const list = () => incidents() ?? [];

  const openCount = () => list().filter((inc) => inc.resolved_at == null).length;

  const mttrLabel = () => {
    const resolved = list().filter((inc) => inc.duration_seconds != null);
    if (resolved.length === 0) return "—";
    const total = resolved.reduce((sum, inc) => sum + (inc.duration_seconds ?? 0), 0);
    return formatDuration(total / resolved.length);
  };

  return (
    <div class="incidents-screen">
      <div class="detail-section-head incidents-header">
        <h2 class="detail-section-h">Incidents</h2>
        <div class="range-toggle" role="group" aria-label="Range">
          <For each={RANGES}>
            {(r) => (
              <button
                type="button"
                class="chip"
                aria-pressed={range() === r}
                onClick={() => setRange(r)}
              >
                {r}
              </button>
            )}
          </For>
        </div>
      </div>

      <div class="incidents-stats mono">
        <div class="incidents-stat" data-testid="stat-open">
          <span class="incidents-stat-value">{openCount()}</span>
          <span class="incidents-stat-label">Open</span>
        </div>
        <div class="incidents-stat" data-testid="stat-mttr">
          <span class="incidents-stat-value">{mttrLabel()}</span>
          <span class="incidents-stat-label">MTTR</span>
        </div>
        <div class="incidents-stat" data-testid="stat-count">
          <span class="incidents-stat-value">{list().length}</span>
          <span class="incidents-stat-label">{range()} count</span>
        </div>
      </div>

      <Show
        when={list().length > 0}
        fallback={<div class="incident-empty">No incidents</div>}
      >
        <ul class="incident-list incidents-list-global">
          <For each={list()}>
            {(inc) => (
              <li class="incident-row">
                <span class="incident-monitor">{inc.monitor_name}</span>
                <span class="incident-cause" title={causeLabel(inc.cause)}>
                  {causeLabel(inc.cause)}
                </span>
                <span class="incident-started" title={absoluteFrom(inc.started_at)}>
                  {relativeFrom(inc.started_at)}
                </span>
                <span class="mono">
                  <Show when={inc.resolved_at == null} fallback={formatDuration(inc.duration_seconds)}>
                    ongoing
                  </Show>
                </span>
                <Show when={inc.status_code != null || inc.error_message}>
                  <span class="incident-detail">
                    <Show when={inc.status_code != null}>{`HTTP ${inc.status_code}`}</Show>
                    <Show when={inc.error_message}>{inc.error_message}</Show>
                  </span>
                </Show>
                <Show when={inc.resolved_at == null && !inc.acknowledged}>
                  <button
                    type="button"
                    class="btn-ghost btn-sm"
                    disabled={ackingId() === inc.id}
                    onClick={() => acknowledge(inc.id)}
                  >
                    Acknowledge
                  </button>
                </Show>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </div>
  );
};

export default Incidents;
