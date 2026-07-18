import { For, Show, type Component } from "solid-js";

export interface TopBarProps {
  query: string;
  onQueryChange: (q: string) => void;
  statusFilter: string | null;
  onStatusFilterChange: (s: string | null) => void;
  onAdd: () => void;
  /** Grid⇄list toggle (spec §11.2). Optional so existing callers/tests that
   *  don't care about layout keep working unchanged. */
  layout?: "grid" | "list";
  onLayoutChange?: (l: "grid" | "list") => void;
}

const STATUS_CHIPS = ["up", "down", "degraded", "paused"];

const TopBar: Component<TopBarProps> = (props) => {
  return (
    <div class="topbar">
      <input
        class="search-input"
        type="search"
        placeholder="Search monitors…"
        aria-label="Search monitors"
        value={props.query}
        onInput={(e) => props.onQueryChange(e.currentTarget.value)}
      />

      <div class="filter-chips" role="group" aria-label="Filter by status">
        <For each={STATUS_CHIPS}>
          {(s) => (
            <button
              type="button"
              class="chip"
              aria-pressed={props.statusFilter === s}
              onClick={() => props.onStatusFilterChange(props.statusFilter === s ? null : s)}
            >
              {s}
            </button>
          )}
        </For>
      </div>

      <div class="spacer" />

      <Show when={props.onLayoutChange}>
        <div class="layout-toggle" role="group" aria-label="Grid or list view">
          <button
            type="button"
            class="layout-btn"
            aria-pressed={(props.layout ?? "grid") === "grid"}
            aria-label="Grid view"
            title="Grid view"
            onClick={() => props.onLayoutChange?.("grid")}
          >
            ▦
          </button>
          <button
            type="button"
            class="layout-btn"
            aria-pressed={props.layout === "list"}
            aria-label="List view"
            title="List view"
            onClick={() => props.onLayoutChange?.("list")}
          >
            ☰
          </button>
        </div>
      </Show>

      <button type="button" class="btn-accent" onClick={props.onAdd}>
        + Add monitor
      </button>
    </div>
  );
};

export default TopBar;
