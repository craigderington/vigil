import { createResource, createSignal, For, onCleanup, Show, type Component } from "solid-js";
import * as api from "../api";
import type { Incident } from "../api";

export interface IncidentTimelineProps {
  monitorId: number;
  /**
   * Optional "YYYY-MM-DD" (UTC) filter, wired up by the 90-day uptime bar's
   * click-to-filter (spec §11.5) in a later task. When set, only incidents
   * overlapping that UTC day are shown.
   */
  dayFilter?: string;
}

const CAUSE_LABEL: Record<string, string> = {
  timeout: "Timeout",
  status: "Bad status",
  connection: "Connection",
  dns: "DNS",
  keyword: "Keyword",
  ssl: "SSL",
  heartbeat: "Heartbeat missed",
};

const CAUSE_ICON: Record<string, string> = {
  timeout: "⏱", // stopwatch
  status: "⚠", // warning
  connection: "⛔", // no entry
  dns: "⎈", // DNS-ish glyph
  keyword: "🔍", // magnifying glass
  ssl: "🔒", // lock
  heartbeat: "💓", // heartbeat
};

function causeLabel(cause: string | null): string {
  if (!cause) return "Unknown";
  return CAUSE_LABEL[cause] ?? cause;
}

function causeIcon(cause: string | null): string {
  if (!cause) return "❓"; // question mark
  return CAUSE_ICON[cause] ?? "❓";
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

/** UTC day ("YYYY-MM-DD") an epoch-seconds timestamp falls on. */
function utcDay(epochSeconds: number): string {
  return new Date(epochSeconds * 1000).toISOString().slice(0, 10);
}

/** Whether the incident's `[started_at, resolved_at ?? now]` span touches the given UTC day. */
function overlapsDay(inc: Incident, day: string): boolean {
  const dayStart = Date.parse(`${day}T00:00:00Z`) / 1000;
  const dayEnd = dayStart + 86400;
  const end = inc.resolved_at ?? Math.floor(Date.now() / 1000);
  return inc.started_at < dayEnd && end >= dayStart;
}

/**
 * One incident row's live-ticking duration for ongoing incidents (§11.6 #8:
 * "Ongoing incidents show a live-ticking duration"). Ticks once per second;
 * resolved incidents just format the stored `duration_seconds` and never
 * start a timer.
 */
const IncidentDuration: Component<{ incident: Incident }> = (props) => {
  const [now, setNow] = createSignal(Date.now());

  const timer = setInterval(() => setNow(Date.now()), 1000);
  onCleanup(() => clearInterval(timer));

  return (
    <span class="mono">
      <Show
        when={props.incident.resolved_at == null}
        fallback={formatDuration(props.incident.duration_seconds)}
      >
        {formatDuration(now() / 1000 - props.incident.started_at)}
      </Show>
    </span>
  );
};

/**
 * Detail panel's incident history (§11.6 #8): reverse-chronological timeline
 * with cause, started/resolved times, a live-ticking duration for ongoing
 * incidents, and an Acknowledge action to silence re-notification on an
 * ongoing, unacknowledged outage.
 */
const IncidentTimeline: Component<IncidentTimelineProps> = (props) => {
  const [incidents, { refetch }] = createResource(
    () => props.monitorId,
    (id) => api.getIncidents("90d", id).catch(() => [] as Incident[]),
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

  // Backend already returns started_at DESC (reverse-chronological); apply
  // the optional day filter on top without re-sorting.
  const visible = () => {
    const all = incidents() ?? [];
    if (!props.dayFilter) return all;
    return all.filter((inc) => overlapsDay(inc, props.dayFilter!));
  };

  return (
    <section class="detail-section incident-timeline">
      <h3 class="detail-section-h">Incident history</h3>
      <Show
        when={visible().length > 0}
        fallback={<div class="incident-empty">No incidents</div>}
      >
        <ul class="incident-list">
          <For each={visible()}>
            {(inc) => (
              <li class="incident-row">
                <span class="incident-cause" title={causeLabel(inc.cause)}>
                  <span aria-hidden="true">{causeIcon(inc.cause)}</span> {causeLabel(inc.cause)}
                </span>
                <span class="incident-started" title={absoluteFrom(inc.started_at)}>
                  {relativeFrom(inc.started_at)}
                </span>
                <IncidentDuration incident={inc} />
                <span class="incident-resolved" title={absoluteFrom(inc.resolved_at)}>
                  <Show when={inc.resolved_at != null} fallback="ongoing">
                    {relativeFrom(inc.resolved_at)}
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
                    title="Acknowledge (silences re-notify reminders)"
                  >
                    Acknowledge
                  </button>
                </Show>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </section>
  );
};

export default IncidentTimeline;
