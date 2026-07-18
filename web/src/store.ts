import { createStore, reconcile } from "solid-js/store";

export type StoreState = { monitors: any[]; online: boolean };

export type StoreEvent = { event: string; data: any };

/** Pure reducer: given a state and an SSE frame, returns a NEW state. */
export function applyEvent(s: StoreState, ev: StoreEvent): StoreState {
  switch (ev.event) {
    case "snapshot":
      return { monitors: ev.data.monitors, online: ev.data.online };

    case "monitor_updated": {
      const { id, ...patch } = ev.data;
      // The SSE frame carries `checked_at`, but the Monitor model / detail
      // panel read `last_checked_at` — merge it across so "Last checked"
      // updates live instead of staying stale until the next full refresh.
      return {
        ...s,
        monitors: patchMonitor(s.monitors, id, { ...patch, last_checked_at: ev.data.checked_at }),
      };
    }

    case "monitor_transition": {
      return { ...s, monitors: patchMonitor(s.monitors, ev.data.id, { status: ev.data.to }) };
    }

    case "connectivity_changed":
      return { ...s, online: ev.data.online };

    case "incident_opened":
    case "incident_resolved":
      // No monitor-list shape carries incident detail (P1 grid/list; detail
      // panel is Task 16) — nothing to patch here.
      return s;

    default:
      return s;
  }
}

function patchMonitor(monitors: any[], id: number, patch: Record<string, any>): any[] {
  const idx = monitors.findIndex((m) => m.id === id);
  if (idx === -1) return monitors;
  const next = monitors.slice();
  next[idx] = { ...next[idx], ...patch };
  return next;
}

/**
 * Solid store seeded from GET /api/monitors, then kept live via
 * `EventSource("/events")`. Every inbound frame is run through the same
 * pure `applyEvent` reducer used by the tests, so store behavior and test
 * behavior can never drift.
 */
export function createMonitorStore() {
  const [state, setState] = createStore<StoreState>({ monitors: [], online: true });

  async function refresh() {
    try {
      const res = await fetch("/api/monitors");
      if (!res.ok) return;
      const monitors = await res.json();
      setState("monitors", reconcile(monitors, { key: "id" }));
    } catch {
      // Network hiccup on initial load — the SSE snapshot frame (sent
      // immediately on connect) will fill this in a moment later.
    }
  }

  function connect() {
    const es = new EventSource("/events");
    es.onmessage = (msg) => {
      let frame: StoreEvent;
      try {
        frame = JSON.parse(msg.data);
      } catch {
        return;
      }
      const next = applyEvent(state, frame);
      setState("online", next.online);
      setState("monitors", reconcile(next.monitors, { key: "id" }));
    };
    return es;
  }

  refresh();
  const source = connect();

  function monitorById(id: number | null | undefined) {
    if (id == null) return undefined;
    return state.monitors.find((m) => m.id === id);
  }

  return {
    monitors: () => state.monitors,
    online: () => state.online,
    monitorById,
    refresh,
    close: () => source.close(),
  };
}
