import { createResource, createSignal, onCleanup, onMount, Show, type Component } from "solid-js";
import * as api from "../api";

export interface DetailPanelProps {
  monitor: any;
  onClose: () => void;
  onEdit?: (monitor: any) => void;
}

const STATUS_LABEL: Record<string, string> = {
  up: "Up",
  down: "Down",
  degraded: "Degraded",
  pending: "Pending",
  paused: "Paused",
  maintenance: "Maintenance",
  unknown: "Unknown",
};

function statusClass(monitor: any): string {
  if (monitor?.is_paused) return "paused";
  const s = monitor?.status ?? "pending";
  if (STATUS_LABEL[s]) return s;
  return "unknown";
}

/** Humane relative-time string from an epoch-seconds timestamp. */
function relativeFrom(epochSeconds: number | null | undefined): string {
  if (epochSeconds == null) return "Never";
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

/** Humane downtime formatting: "0s", "5m 12s", "2h 3m", "1d 4h". */
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

function absoluteFrom(epochSeconds: number | null | undefined): string | undefined {
  if (epochSeconds == null) return undefined;
  return new Date(epochSeconds * 1000).toLocaleString();
}

/**
 * A single uptime-range tile. `nullText` is the primary/hero tile's bare
 * dash ("—") for the 24h tile, and a slightly more descriptive "No data"
 * for the 7d tile — deliberately different strings (not just differently
 * styled) so a monitor with no data in *either* range never renders two
 * DOM nodes with identical text; anything sighted or assistive-tech users
 * rely on to distinguish "which tile is this" stays unambiguous too.
 */
const UptimeTile: Component<{
  label: string;
  monitorId: number;
  range: "24h" | "7d";
  nullText: string;
}> = (props) => {
  const [stats] = createResource(
    () => [props.monitorId, props.range] as const,
    ([id, range]) => api.getStats(id, range).catch(() => null),
  );

  return (
    <div class="detail-tile">
      <span class="detail-tile-label">{props.label}</span>
      <span class="detail-tile-value mono">
        <Show when={stats() && stats()!.uptime_pct != null} fallback={props.nullText}>
          {stats()?.uptime_pct?.toFixed(2)}%
        </Show>
      </span>
      <span class="detail-tile-sub">{formatDuration(stats()?.downtime_seconds)}</span>
    </div>
  );
};

const DetailPanel: Component<DetailPanelProps> = (props) => {
  const [busy, setBusy] = createSignal(false);

  function onKeyDown(e: KeyboardEvent) {
    if (e.key === "Escape") props.onClose();
  }

  onMount(() => {
    document.addEventListener("keydown", onKeyDown);
  });
  onCleanup(() => {
    document.removeEventListener("keydown", onKeyDown);
  });

  async function runAction(fn: () => Promise<any>) {
    setBusy(true);
    try {
      await fn();
    } finally {
      setBusy(false);
    }
  }

  async function handleDelete() {
    await runAction(() => api.deleteMonitor(props.monitor.id));
    props.onClose();
  }

  // Live SSE response_time_ms wins when present; otherwise fall back to the
  // 24h stats average so the tile isn't blank until the monitor's first
  // scheduled check.
  const [fallbackStats] = createResource(
    () => props.monitor?.id,
    (id) => (id != null ? api.getStats(id, "24h").catch(() => null) : null),
  );

  function displayResponseMs(): number | null {
    const live = props.monitor?.response_time_ms;
    if (typeof live === "number") return live;
    const avg = fallbackStats()?.avg_ms;
    return typeof avg === "number" ? avg : null;
  }

  return (
    <div class="detail-backdrop" onClick={props.onClose}>
      <div
        class="detail-panel"
        role="dialog"
        aria-modal="true"
        aria-label={`${props.monitor?.name ?? "Monitor"} details`}
        onClick={(e) => e.stopPropagation()}
      >
        <div class="detail-header">
          <div class="detail-header-top">
            <span class={`status-pill ${statusClass(props.monitor)}`}>
              {STATUS_LABEL[statusClass(props.monitor)]}
            </span>
            <button type="button" class="detail-close" aria-label="Close" onClick={props.onClose}>
              &#10005;
            </button>
          </div>
          <h2 class="detail-name">{props.monitor?.name}</h2>
          <div class="detail-subline">
            <span class="detail-url">{props.monitor?.url ?? props.monitor?.host ?? ""}</span>
            <span class="card-type">{props.monitor?.type ?? "http"}</span>
          </div>

          <div class="detail-actions">
            <button
              type="button"
              class="btn-ghost"
              disabled={busy()}
              onClick={() => runAction(() => api.checkNow(props.monitor.id))}
            >
              Check now
            </button>
            <Show
              when={!props.monitor?.is_paused}
              fallback={
                <button
                  type="button"
                  class="btn-ghost"
                  disabled={busy()}
                  onClick={() => runAction(() => api.resumeMonitor(props.monitor.id))}
                >
                  Resume
                </button>
              }
            >
              <button
                type="button"
                class="btn-ghost"
                disabled={busy()}
                onClick={() => runAction(() => api.pauseMonitor(props.monitor.id))}
              >
                Pause
              </button>
            </Show>
            <button type="button" class="btn-ghost" onClick={() => props.onEdit?.(props.monitor)}>
              Edit
            </button>
            <button type="button" class="btn-ghost danger" disabled={busy()} onClick={handleDelete}>
              Delete
            </button>
          </div>
        </div>

        <div class="detail-body">
          <div class="now-strip">
            <div class="detail-tile">
              <span class="detail-tile-label">Status</span>
              <span class={`detail-tile-value status-${statusClass(props.monitor)}`}>
                {STATUS_LABEL[statusClass(props.monitor)]}
              </span>
            </div>
            <div class="detail-tile">
              <span class="detail-tile-label">Response time</span>
              <span class="detail-tile-value mono">
                <Show when={displayResponseMs() != null} fallback="No data">
                  {Math.round(displayResponseMs() as number)}
                  <span class="unit">ms</span>
                </Show>
              </span>
            </div>
            <div class="detail-tile" title={absoluteFrom(props.monitor?.last_checked_at)}>
              <span class="detail-tile-label">Last checked</span>
              <span class="detail-tile-value mono">{relativeFrom(props.monitor?.last_checked_at)}</span>
            </div>
          </div>

          <div class="uptime-tiles">
            <UptimeTile label="24h" monitorId={props.monitor.id} range="24h" nullText="—" />
            <UptimeTile label="7d" monitorId={props.monitor.id} range="7d" nullText="No data" />
          </div>

          <details class="detail-config">
            <summary>Configuration</summary>
            <div class="detail-config-header">
              <button type="button" class="btn-link" onClick={() => props.onEdit?.(props.monitor)}>
                Edit
              </button>
            </div>
            <dl class="config-list">
              <dt>Interval</dt>
              <dd>{props.monitor?.interval_seconds ?? 300}s</dd>
              <dt>Timeout</dt>
              <dd>{props.monitor?.timeout_seconds ?? 30}s</dd>
              <dt>Method</dt>
              <dd>{props.monitor?.method ?? "GET"}</dd>
              <dt>Expected codes</dt>
              <dd>{props.monitor?.expected_status_codes ?? "200-299"}</dd>
              <dt>Confirmation threshold</dt>
              <dd>{props.monitor?.confirmation_threshold ?? 3}</dd>
              <dt>Recovery threshold</dt>
              <dd>{props.monitor?.recovery_threshold ?? 1}</dd>
            </dl>
          </details>
        </div>
      </div>
    </div>
  );
};

export default DetailPanel;
