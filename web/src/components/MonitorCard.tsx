import { createResource, createSignal, onCleanup, Show, type Component } from "solid-js";
import * as api from "../api";
import { displayStatus } from "../maintenance_ids";
import UptimeBar from "./UptimeBar";

export interface MonitorCardProps {
  monitor: any;
  onOpen: (id: number) => void;
  onChanged?: () => void;
  reorderEnabled?: boolean;
  dragging?: boolean;
  onGripDown?: (id: number, e: PointerEvent) => void;
  onGripKey?: (id: number, e: KeyboardEvent) => void;
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

/** Wraps the module-level `displayStatus` (maintenance_ids.ts), which
 *  applies the paused > maintenance > real precedence, with the same
 *  "unrecognized status falls back to unknown" guard this local helper had
 *  before. */
function statusClass(monitor: any): string {
  const s = displayStatus({ id: monitor.id, is_paused: monitor.is_paused, status: monitor.status ?? "pending" });
  if (STATUS_LABEL[s]) return s;
  return "unknown";
}

/** Humane relative-time string from an epoch-seconds timestamp — used for
 *  a heartbeat monitor's "last ping" in place of the response-time slot. */
function relativeFrom(epochSeconds: number | null | undefined): string {
  if (epochSeconds == null) return "—";
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

const MonitorCard: Component<MonitorCardProps> = (props) => {
  const [menuOpen, setMenuOpen] = createSignal(false);

  const [stats, { refetch: refetchStats }] = createResource(
    () => props.monitor.id,
    (id) => api.getStats(id, "24h").catch(() => null),
  );

  function toggleMenu(e: MouseEvent) {
    e.stopPropagation();
    setMenuOpen((v) => {
      const next = !v;
      if (next) document.addEventListener("click", closeMenu, { once: true });
      return next;
    });
  }

  function closeMenu() {
    setMenuOpen(false);
  }

  onCleanup(() => document.removeEventListener("click", closeMenu));

  async function runAction(fn: () => Promise<any>, e: MouseEvent) {
    e.stopPropagation();
    closeMenu();
    try {
      await fn();
    } finally {
      refetchStats();
      props.onChanged?.();
    }
  }

  function openCard() {
    closeMenu();
    props.onOpen(props.monitor.id);
  }

  function onCardKeyDown(e: KeyboardEvent) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      openCard();
    }
  }

  // Live SSE response_time_ms wins when present; otherwise fall back to the
  // 24h stats average so the card isn't blank until the monitor's first
  // scheduled check (which can be up to `interval_seconds` after load).
  function responseMs(): number | null {
    const live = props.monitor.response_time_ms;
    if (typeof live === "number") return live;
    const avg = stats()?.avg_ms;
    if (typeof avg === "number") return avg;
    return null;
  }

  return (
    // A native <button> can't legally contain the nested action <button>s
    // below (invalid HTML — browsers silently reparse it, breaking click
    // handling), so the card itself is a div with button semantics instead.
    <div
      class={`monitor-card${props.dragging ? " dragging" : ""}`}
      data-monitor-id={props.monitor.id}
      role="button"
      tabindex="0"
      onClick={openCard}
      onKeyDown={onCardKeyDown}
    >
      <div class="card-header">
        <Show when={props.reorderEnabled}>
          <button
            class="card-grip"
            type="button"
            aria-label={`Reorder ${props.monitor.name} (use arrow keys)`}
            title="Drag to reorder"
            onPointerDown={(e) => { e.stopPropagation(); props.onGripDown?.(props.monitor.id, e); }}
            onKeyDown={(e) => props.onGripKey?.(props.monitor.id, e)}
            onClick={(e) => e.stopPropagation()}
          >
            <svg class="grip-icon" viewBox="0 0 10 16" width="10" height="16" aria-hidden="true">
              <circle cx="2.5" cy="3" r="1.3" /><circle cx="7.5" cy="3" r="1.3" />
              <circle cx="2.5" cy="8" r="1.3" /><circle cx="7.5" cy="8" r="1.3" />
              <circle cx="2.5" cy="13" r="1.3" /><circle cx="7.5" cy="13" r="1.3" />
            </svg>
          </button>
        </Show>
        <span
          class={`status-dot ${statusClass(props.monitor)}`}
          aria-label={`status: ${STATUS_LABEL[statusClass(props.monitor)]}`}
          title={STATUS_LABEL[statusClass(props.monitor)]}
        />
        <span class="card-name">{props.monitor.name}</span>
        <button
          class="card-menu-btn"
          type="button"
          aria-label="Monitor actions"
          aria-haspopup="menu"
          aria-expanded={menuOpen()}
          onClick={toggleMenu}
        >
          &#8942;
        </button>
      </div>

      <div class="card-subline">
        <span class="card-url">{props.monitor.url ?? props.monitor.host ?? ""}</span>
        <span class="card-type">{props.monitor.type ?? "http"}</span>
      </div>

      <div class="card-metrics">
        <span class="card-response mono">
          <Show
            when={props.monitor.type === "heartbeat"}
            fallback={
              <Show when={responseMs() != null} fallback="—">
                {Math.round(responseMs() as number)}
                <span class="unit">ms</span>
              </Show>
            }
          >
            {relativeFrom(props.monitor.last_ping_at)}
          </Show>
        </span>
      </div>

      <div class="card-badges">
        <span class="uptime-badge mono">
          <Show when={stats() && stats()!.uptime_pct != null} fallback="— · 24h">
            {stats()!.uptime_pct!.toFixed(2)}% · 24h
          </Show>
        </span>
      </div>

      <UptimeBar monitorId={props.monitor.id} compact />

      <Show when={menuOpen()}>
        {/* eslint-disable-next-line */}
        <div class="card-menu" role="menu" onClick={(e) => e.stopPropagation()}>
          <button type="button" role="menuitem" onClick={[runAction, () => api.checkNow(props.monitor.id)]}>
            Check now
          </button>
          <Show
            when={!props.monitor.is_paused}
            fallback={
              <button type="button" role="menuitem" onClick={[runAction, () => api.resumeMonitor(props.monitor.id)]}>
                Resume
              </button>
            }
          >
            <button type="button" role="menuitem" onClick={[runAction, () => api.pauseMonitor(props.monitor.id)]}>
              Pause
            </button>
          </Show>
          <button
            type="button"
            role="menuitem"
            class="danger"
            onClick={[runAction, () => api.deleteMonitor(props.monitor.id)]}
          >
            Delete
          </button>
        </div>
      </Show>
    </div>
  );
};

export default MonitorCard;
