import { createResource, createSignal, onCleanup, Show, type Component } from "solid-js";
import * as api from "../api";

export interface MonitorCardProps {
  monitor: any;
  onOpen: (id: number) => void;
  onChanged?: () => void;
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
  if (monitor.is_paused) return "paused";
  const s = monitor.status ?? "pending";
  if (STATUS_LABEL[s]) return s;
  return "unknown";
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

  return (
    // A native <button> can't legally contain the nested action <button>s
    // below (invalid HTML — browsers silently reparse it, breaking click
    // handling), so the card itself is a div with button semantics instead.
    <div
      class="monitor-card"
      role="button"
      tabindex="0"
      onClick={openCard}
      onKeyDown={onCardKeyDown}
    >
      <div class="card-header">
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
          <Show when={props.monitor.response_time_ms != null} fallback="—">
            {props.monitor.response_time_ms}
            <span class="unit">ms</span>
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
