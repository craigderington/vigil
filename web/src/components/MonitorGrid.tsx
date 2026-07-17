import { For, Show, type Component } from "solid-js";
import MonitorCard from "./MonitorCard";

export interface MonitorGridProps {
  monitors: any[];
  onOpen: (id: number) => void;
  onChanged?: () => void;
}

const MonitorGrid: Component<MonitorGridProps> = (props) => {
  return (
    <Show
      when={props.monitors.length > 0}
      fallback={<div class="empty-state">No monitors match. Add your first monitor to get started.</div>}
    >
      <div class="monitor-grid">
        <For each={props.monitors}>
          {(m) => <MonitorCard monitor={m} onOpen={props.onOpen} onChanged={props.onChanged} />}
        </For>
      </div>
    </Show>
  );
};

export default MonitorGrid;
