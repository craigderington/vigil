import { createMemo, createSignal, Show, type Component } from "solid-js";
import { createMonitorStore } from "./store";
import Rail from "./components/Rail";
import TopBar from "./components/TopBar";
import ConnectivityBanner from "./components/ConnectivityBanner";
import MonitorGrid from "./components/MonitorGrid";
import DetailPanel from "./components/DetailPanel";

const App: Component = () => {
  const store = createMonitorStore();
  const [query, setQuery] = createSignal("");
  const [statusFilter, setStatusFilter] = createSignal<string | null>(null);

  const filtered = createMemo(() => {
    const q = query().trim().toLowerCase();
    const status = statusFilter();
    return store.monitors().filter((m) => {
      if (status && m.status !== status) return false;
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

  const addMonitor = () => {};

  return (
    <div class="app">
      <Rail monitors={store.monitors()} />
      <div class="app-main">
        <ConnectivityBanner online={store.online()} />
        <TopBar
          query={query()}
          onQueryChange={setQuery}
          statusFilter={statusFilter()}
          onStatusFilterChange={setStatusFilter}
          onAdd={addMonitor}
        />
        <div class="app-content">
          <MonitorGrid monitors={filtered()} onOpen={setOpenMonitorId} onChanged={store.refresh} />
        </div>
      </div>
      <Show when={openMonitor()}>
        <DetailPanel monitor={openMonitor()} onClose={() => setOpenMonitorId(null)} />
      </Show>
    </div>
  );
};

export default App;
