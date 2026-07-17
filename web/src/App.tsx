import { createMemo, createSignal, type Component } from "solid-js";
import { createMonitorStore } from "./store";
import Rail from "./components/Rail";
import TopBar from "./components/TopBar";
import ConnectivityBanner from "./components/ConnectivityBanner";
import MonitorGrid from "./components/MonitorGrid";

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

  // Detail panel lands in Task 16; for now opening a monitor is a no-op.
  const openMonitor = (_id: number) => {};
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
          <MonitorGrid monitors={filtered()} onOpen={openMonitor} onChanged={store.refresh} />
        </div>
      </div>
    </div>
  );
};

export default App;
