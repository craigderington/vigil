import { createResource, createSignal, Show, type Component } from "solid-js";
import * as api from "../api";
import { pingUrl } from "../api";

export interface HeartbeatCardProps {
  monitor: any;
}

/** Humane relative-time string from an epoch-seconds timestamp — mirrors
 *  the identically-named helpers in DetailPanel/ListView (each component
 *  keeps its own copy rather than sharing a util, matching the existing
 *  pattern in this codebase). */
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

function absoluteFrom(epochSeconds: number | null | undefined): string | undefined {
  if (epochSeconds == null) return undefined;
  return new Date(epochSeconds * 1000).toLocaleString();
}

/** `last_ping_at + interval_seconds + heartbeat_grace_seconds` — the same
 *  overdue boundary the backend reaper (`heartbeat::reap_once`) uses to
 *  drive a silent heartbeat DOWN. `null` when there's no ping yet. */
function nextExpectedBy(monitor: any): number | null {
  const last = monitor?.last_ping_at;
  if (last == null) return null;
  const interval = monitor?.interval_seconds ?? 0;
  const grace = monitor?.heartbeat_grace_seconds ?? 0;
  return last + interval + grace;
}

/**
 * Heartbeat card (§11.6, mirrors SslCard's structure): the copyable ping
 * URL (fetched via the dedicated `getHeartbeat` endpoint — the token is
 * never on the `Monitor` object itself), last-ping and next-expected-by,
 * and a "Waiting for first ping" empty state before the monitor is armed.
 */
const HeartbeatCard: Component<HeartbeatCardProps> = (props) => {
  const [hb] = createResource(
    () => props.monitor?.id,
    (id) => (id != null ? api.getHeartbeat(id).catch(() => null) : null),
  );

  const [copied, setCopied] = createSignal(false);

  async function handleCopy() {
    const info = hb();
    if (!info) return;
    try {
      await navigator.clipboard.writeText(pingUrl(info.ping_path));
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // clipboard API may be unavailable (e.g. insecure context) — no-op
    }
  }

  return (
    <section class="detail-section heartbeat-card">
      <div class="detail-section-head">
        <h3 class="detail-section-h">Heartbeat</h3>
      </div>

      <Show when={hb() != null} fallback={<div class="ssl-empty">Loading ping URL…</div>}>
        <div class="test-result mono">
          {pingUrl(hb()!.ping_path)}
          <button type="button" class="btn-link" onClick={handleCopy}>
            {copied() ? "Copied" : "Copy"}
          </button>
        </div>
        <div class="test-result mono">curl -fsS {pingUrl(hb()!.ping_path)}</div>
      </Show>

      <Show
        when={props.monitor?.last_ping_at != null}
        fallback={<div class="ssl-empty">Waiting for first ping</div>}
      >
        <div class="ssl-detail-row">
          <span class="ssl-detail-label">Last ping</span>
          <span class="ssl-detail-value mono" title={absoluteFrom(props.monitor?.last_ping_at)}>
            {relativeFrom(props.monitor?.last_ping_at)}
          </span>
        </div>
        <div class="ssl-detail-row">
          <span class="ssl-detail-label">Next expected by</span>
          <span class="ssl-detail-value mono" title={absoluteFrom(nextExpectedBy(props.monitor))}>
            <Show when={nextExpectedBy(props.monitor) != null} fallback="—">
              {absoluteFrom(nextExpectedBy(props.monitor))}
            </Show>
          </span>
        </div>
      </Show>
    </section>
  );
};

export default HeartbeatCard;
