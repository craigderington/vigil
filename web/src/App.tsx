import { createMemo, createSignal, Match, Show, Switch, type Component } from "solid-js";
import { createMonitorStore } from "./store";
import * as api from "./api";
import { inMaintenance } from "./maintenance_ids";
import Rail, { type RailView } from "./components/Rail";
import TopBar from "./components/TopBar";
import ConnectivityBanner from "./components/ConnectivityBanner";
import MonitorGrid from "./components/MonitorGrid";
import ListView from "./components/ListView";
import DetailPanel from "./components/DetailPanel";
import MonitorForm from "./components/MonitorForm";
import Settings from "./components/Settings";
import Incidents from "./components/Incidents";
import Maintenance from "./components/Maintenance";

const App: Component = () => {
  const store = createMonitorStore();
  const [query, setQuery] = createSignal("");
  const [statusFilter, setStatusFilter] = createSignal<string | null>(null);

  // Top-level view. "dashboard", "settings", "incidents", and "maintenance"
  // have real screens; Rail still fires onNavigate for Notifications so a
  // future screen can hook in, but until then that click just returns to
  // the dashboard grid — see Rail.tsx.
  const [view, setView] = createSignal<RailView>("dashboard");

  const filtered = createMemo(() => {
    const q = query().trim().toLowerCase();
    const status = statusFilter();
    return store.monitors().filter((m) => {
      // `monitor.status` is never "maintenance" (it's a client-side overlay
      // — see maintenance_ids.ts), so the maintenance chip can't use the
      // plain equality check below; it would filter to zero every time.
      if (status) {
        if (status === "maintenance") {
          if (!inMaintenance(m.id)) return false;
        } else if (m.status !== status) {
          return false;
        }
      }
      if (!q) return true;
      const haystack = `${m.name ?? ""} ${m.url ?? ""} ${m.host ?? ""}`.toLowerCase();
      return haystack.includes(q);
    });
  });

  const [openMonitorId, setOpenMonitorId] = createSignal<number | null>(null);
  // Re-derived from the live store on every read, so SSE updates to the
  // open monitor (status flips, new response_time_ms, etc.) are reflected
  // in the panel without a separate fetch or a stale snapshot.
  const openMonitor = createMemo(() => store.monitorById(openMonitorId()));

  // Add/Edit monitor form. `formMonitor() === undefined` means ADD mode;
  // any other value (including a monitor object) means EDIT mode for that
  // monitor. `formOpen` is a separate signal so "Add" (no monitor) is
  // distinguishable from "form closed".
  const [formOpen, setFormOpen] = createSignal(false);
  const [formMonitor, setFormMonitor] = createSignal<any | undefined>(undefined);

  const addMonitor = () => {
    setFormMonitor(undefined);
    setFormOpen(true);
  };
  const editMonitor = (monitor: any) => {
    setFormMonitor(monitor);
    setFormOpen(true);
    setOpenMonitorId(null);
  };
  const closeForm = () => setFormOpen(false);
  const onFormSaved = () => {
    store.refresh();
    setFormOpen(false);
  };

  return (
    <div class="app">
      <Rail
        monitors={store.monitors()}
        activeView={view()}
        onNavigate={(key) =>
          setView(
            key === "settings" || key === "incidents" || key === "maintenance" ? key : "dashboard",
          )
        }
      />
      <div class="app-main">
        <Switch
          fallback={
            <>
              <ConnectivityBanner online={store.online()} />
              <TopBar
                query={query()}
                onQueryChange={setQuery}
                statusFilter={statusFilter()}
                onStatusFilterChange={setStatusFilter}
                onAdd={addMonitor}
                layout={store.layout()}
                onLayoutChange={store.setLayout}
              />
              <div class="app-content">
                <Show
                  when={store.layout() === "list"}
                  fallback={
                    <MonitorGrid monitors={filtered()} onOpen={setOpenMonitorId} onChanged={store.refresh} />
                  }
                >
                  <ListView
                    monitors={filtered()}
                    onOpen={setOpenMonitorId}
                    onChanged={store.refresh}
                    sort={store.sort()}
                    onSortChange={store.setSort}
                  />
                </Show>
              </div>
            </>
          }
        >
          <Match when={view() === "settings"}>
            <div class="app-content">
              <Settings />
            </div>
          </Match>
          <Match when={view() === "incidents"}>
            <div class="app-content">
              <Incidents />
            </div>
          </Match>
          <Match when={view() === "maintenance"}>
            <div class="app-content">
              <Maintenance />
            </div>
          </Match>
        </Switch>
      </div>
      <Show when={openMonitor()}>
        <DetailPanel
          monitor={openMonitor()}
          onClose={() => setOpenMonitorId(null)}
          onChanged={store.refresh}
          onEdit={editMonitor}
          certVersion={store.certVersion}
        />
      </Show>
      <Show when={formOpen()}>
        <MonitorForm api={api} monitor={formMonitor()} onSaved={onFormSaved} onClose={closeForm} />
      </Show>
    </div>
  );
};

export default App;
