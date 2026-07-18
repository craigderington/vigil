import { createMemo, createResource, createSignal, For, onCleanup, Show, type Component } from "solid-js";
import * as api from "../api";
import type { Stats } from "../api";
import UptimeBar from "./UptimeBar";

export type SortCol =
  | null
  | "name"
  | "type"
  | "last_checked_at"
  | "response_time_ms"
  | "uptime_24h"
  | "uptime_7d"
  | "uptime_30d";
export type SortDir = "asc" | "desc";
export interface ListSort {
  col: SortCol;
  dir: SortDir;
}

export interface ListViewProps {
  monitors: any[];
  onOpen: (id: number) => void;
  onChanged?: () => void;
  /**
   * Controlled sort state. Falls back to an internal signal when omitted
   * (isolated tests, or any caller that doesn't care) so App/store can lift
   * it and have it survive toggling back and forth between grid and list
   * within a session — a signal created inside ListView itself would reset
   * every time the component unmounts on switching back to grid.
   */
  sort?: ListSort;
  onSortChange?: (s: ListSort) => void;
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

/** Humane relative-time string from an epoch-seconds timestamp. */
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

function absoluteFrom(epochSeconds: number | null | undefined): string | undefined {
  if (epochSeconds == null) return undefined;
  return new Date(epochSeconds * 1000).toLocaleString();
}

interface RowStats {
  h24: Stats | null;
  d7: Stats | null;
  d30: Stats | null;
}

/**
 * Null-safe numeric comparator: nulls sort as smallest, and two nulls are
 * equal (returns 0) rather than the `(a ?? -Infinity) - (b ?? -Infinity)`
 * idiom, which produces `-Infinity - -Infinity === NaN` — a comparator that
 * returns NaN gives Array.prototype.sort undefined (browser-dependent)
 * ordering instead of a stable ascending/descending sort.
 */
function numCompare(a: number | null | undefined, b: number | null | undefined): number {
  const an = a ?? null;
  const bn = b ?? null;
  if (an == null && bn == null) return 0;
  if (an == null) return -1;
  if (bn == null) return 1;
  return an - bn;
}

const HEADERS: { key: Exclude<SortCol, null>; label: string; right?: boolean }[] = [
  { key: "name", label: "Name" },
  { key: "type", label: "Type" },
  { key: "last_checked_at", label: "Last check", right: true },
  { key: "response_time_ms", label: "Response", right: true },
  { key: "uptime_24h", label: "24h", right: true },
  { key: "uptime_7d", label: "7d", right: true },
  { key: "uptime_30d", label: "30d", right: true },
];

/**
 * Dense list view for the dashboard (spec §11.4): one row per monitor with
 * sortable columns, a compact 90-day uptime bar, and the same quick-action
 * menu as the grid's MonitorCard. This is the mode you live in past ~20
 * monitors, where the card grid gets unwieldy to scan.
 */
const ListView: Component<ListViewProps> = (props) => {
  const [localSort, setLocalSort] = createSignal<ListSort>({ col: null, dir: "asc" });
  const sort = () => props.sort ?? localSort();
  const setSort = (s: ListSort) => (props.onSortChange ? props.onSortChange(s) : setLocalSort(s));

  function onHeaderClick(col: Exclude<SortCol, null>) {
    const cur = sort();
    setSort(cur.col === col ? { col, dir: cur.dir === "asc" ? "desc" : "asc" } : { col, dir: "asc" });
  }

  // 24h/7d/30d uptime for every visible row. A per-row createResource (one
  // per <MonitorCard>-style row) would be the more idiomatic Solid pattern,
  // but the sort comparator below needs every row's value available at the
  // table level to sort by those columns — so this fetches them together
  // into one map instead, refetching whenever the visible id set changes.
  // Same total request count (3 ranges × N monitors), just gathered in one
  // resource rather than N independent ones. Fine at P2 scale (a few
  // hundred monitors); would want batching into a single backend endpoint
  // before that.
  const idsKey = createMemo(() => props.monitors.map((m) => m.id).join(","));
  const [statsMap, { refetch: refetchStats }] = createResource(idsKey, async () => {
    const map: Record<number, RowStats> = {};
    await Promise.all(
      props.monitors.map(async (m) => {
        const [h24, d7, d30] = await Promise.all([
          api.getStats(m.id, "24h").catch(() => null),
          api.getStats(m.id, "7d").catch(() => null),
          api.getStats(m.id, "30d").catch(() => null),
        ]);
        map[m.id] = { h24, d7, d30 };
      }),
    );
    return map;
  });

  function uptimeFor(id: number, range: "h24" | "d7" | "d30"): number | null {
    const row = statsMap()?.[id];
    const v = row?.[range]?.uptime_pct;
    return typeof v === "number" ? v : null;
  }

  // Live SSE response_time_ms wins when present; otherwise fall back to the
  // 24h stats average, mirroring MonitorCard.responseMs() so the column
  // isn't blank until the monitor's first scheduled check.
  function responseMsFor(m: any): number | null {
    const live = m.response_time_ms;
    if (typeof live === "number") return live;
    const avg = statsMap()?.[m.id]?.h24?.avg_ms;
    if (typeof avg === "number") return avg;
    return null;
  }

  const sortedMonitors = createMemo(() => {
    const list = props.monitors.slice();
    const s = sort();
    list.sort((a, b) => {
      if (!s.col) {
        const so = (a.sort_order ?? 0) - (b.sort_order ?? 0);
        if (so !== 0) return so;
        return String(a.name ?? "").localeCompare(String(b.name ?? ""));
      }
      let cmp: number;
      switch (s.col) {
        case "name":
          cmp = String(a.name ?? "").localeCompare(String(b.name ?? ""));
          break;
        case "type":
          cmp = String(a.type ?? "").localeCompare(String(b.type ?? ""));
          break;
        case "last_checked_at":
          cmp = numCompare(a.last_checked_at, b.last_checked_at);
          break;
        case "response_time_ms":
          cmp = numCompare(a.response_time_ms, b.response_time_ms);
          break;
        case "uptime_24h":
          cmp = numCompare(uptimeFor(a.id, "h24"), uptimeFor(b.id, "h24"));
          break;
        case "uptime_7d":
          cmp = numCompare(uptimeFor(a.id, "d7"), uptimeFor(b.id, "d7"));
          break;
        case "uptime_30d":
          cmp = numCompare(uptimeFor(a.id, "d30"), uptimeFor(b.id, "d30"));
          break;
        default:
          cmp = 0;
      }
      return s.dir === "asc" ? cmp : -cmp;
    });
    return list;
  });

  const [menuOpenId, setMenuOpenId] = createSignal<number | null>(null);

  function toggleMenu(id: number, e: MouseEvent) {
    e.stopPropagation();
    setMenuOpenId((cur) => {
      const next = cur === id ? null : id;
      if (next !== null) document.addEventListener("click", closeMenu, { once: true });
      return next;
    });
  }
  function closeMenu() {
    setMenuOpenId(null);
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

  function onRowKeyDown(id: number, e: KeyboardEvent) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      props.onOpen(id);
    }
  }

  return (
    <Show
      when={props.monitors.length > 0}
      fallback={<div class="empty-state">No monitors match. Add your first monitor to get started.</div>}
    >
      <div class="list-view">
        <table class="monitor-table">
          <thead>
            <tr>
              <th class="col-status" aria-label="Status" />
              <For each={HEADERS}>
                {(h) => (
                  <th
                    class={h.right ? "sortable align-right" : "sortable"}
                    aria-sort={
                      sort().col === h.key ? (sort().dir === "asc" ? "ascending" : "descending") : "none"
                    }
                  >
                    <button type="button" class="th-sort-btn" onClick={() => onHeaderClick(h.key)}>
                      {h.label}
                      <Show when={sort().col === h.key}>
                        <span class="sort-arrow" aria-hidden="true">
                          {sort().dir === "asc" ? "▲" : "▼"}
                        </span>
                      </Show>
                    </button>
                  </th>
                )}
              </For>
              <th class="col-bar">Uptime</th>
              <th class="col-menu" aria-label="Actions" />
            </tr>
          </thead>
          <tbody>
            <For each={sortedMonitors()}>
              {(m) => (
                <tr
                  role="row"
                  data-monitor-id={m.id}
                  class="list-row"
                  tabindex="0"
                  onClick={() => props.onOpen(m.id)}
                  onKeyDown={[onRowKeyDown, m.id]}
                >
                  <td class="col-status">
                    <span
                      class={`status-dot ${statusClass(m)}`}
                      aria-label={`status: ${STATUS_LABEL[statusClass(m)]}`}
                      title={STATUS_LABEL[statusClass(m)]}
                    />
                  </td>
                  <td class="col-name">{m.name}</td>
                  <td class="col-type">{m.type ?? "http"}</td>
                  <td class="col-lastcheck mono align-right" title={absoluteFrom(m.last_checked_at)}>
                    {relativeFrom(m.last_checked_at)}
                  </td>
                  <td class="mono align-right">
                    <Show when={responseMsFor(m) != null} fallback="—">
                      {Math.round(responseMsFor(m) as number)}
                      <span class="unit">ms</span>
                    </Show>
                  </td>
                  <td class="mono align-right">
                    <Show when={uptimeFor(m.id, "h24") != null} fallback="—">
                      {uptimeFor(m.id, "h24")!.toFixed(2)}%
                    </Show>
                  </td>
                  <td class="mono align-right">
                    <Show when={uptimeFor(m.id, "d7") != null} fallback="—">
                      {uptimeFor(m.id, "d7")!.toFixed(2)}%
                    </Show>
                  </td>
                  <td class="mono align-right">
                    <Show when={uptimeFor(m.id, "d30") != null} fallback="—">
                      {uptimeFor(m.id, "d30")!.toFixed(2)}%
                    </Show>
                  </td>
                  <td class="col-bar">
                    <UptimeBar monitorId={m.id} compact />
                  </td>
                  <td class="col-menu">
                    <button
                      class="card-menu-btn"
                      type="button"
                      aria-label="Monitor actions"
                      aria-haspopup="menu"
                      aria-expanded={menuOpenId() === m.id}
                      onClick={[toggleMenu, m.id]}
                    >
                      &#8942;
                    </button>
                    <Show when={menuOpenId() === m.id}>
                      {/* eslint-disable-next-line */}
                      <div class="card-menu list-menu" role="menu" onClick={(e) => e.stopPropagation()}>
                        <button type="button" role="menuitem" onClick={[runAction, () => api.checkNow(m.id)]}>
                          Check now
                        </button>
                        <Show
                          when={!m.is_paused}
                          fallback={
                            <button
                              type="button"
                              role="menuitem"
                              onClick={[runAction, () => api.resumeMonitor(m.id)]}
                            >
                              Resume
                            </button>
                          }
                        >
                          <button type="button" role="menuitem" onClick={[runAction, () => api.pauseMonitor(m.id)]}>
                            Pause
                          </button>
                        </Show>
                        <button
                          type="button"
                          role="menuitem"
                          class="danger"
                          onClick={[runAction, () => api.deleteMonitor(m.id)]}
                        >
                          Delete
                        </button>
                      </div>
                    </Show>
                  </td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </div>
    </Show>
  );
};

export default ListView;
