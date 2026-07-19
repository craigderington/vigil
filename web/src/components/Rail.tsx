import { createMemo, type Component } from "solid-js";
import { inMaintenance } from "../maintenance_ids";

export type RailView = "dashboard" | "settings" | "incidents" | "maintenance";

export interface RailProps {
  monitors: any[];
  /** Which top-level view is active; drives `aria-current`. Defaults to "dashboard". */
  activeView?: RailView;
  /** Fired with the clicked item's key. Notifications has no screen yet, so
   * it still fires so a future screen can hook in; App.tsx routes anything
   * but "settings"/"incidents"/"maintenance" back to the dashboard grid. */
  onNavigate?: (key: RailView | "notifications") => void;
}

const NAV_ITEMS: { icon: string; label: string; key: RailView | "notifications" }[] = [
  { icon: "▣", label: "Dashboard", key: "dashboard" },
  { icon: "⚠", label: "Incidents", key: "incidents" },
  { icon: "\u{1F514}", label: "Notifications", key: "notifications" },
  { icon: "\u{1F6E0}", label: "Maintenance", key: "maintenance" },
  { icon: "⚙", label: "Settings", key: "settings" },
];

const Rail: Component<RailProps> = (props) => {
  const summary = createMemo(() => {
    let up = 0,
      down = 0,
      paused = 0,
      maintenance = 0;
    for (const m of props.monitors) {
      // Precedence: paused > maintenance > real status (M7) — a paused
      // monitor counts as paused even if it also falls inside an active
      // maintenance window.
      if (m.is_paused || m.status === "paused") paused++;
      else if (inMaintenance(m.id)) maintenance++;
      else if (m.status === "up") up++;
      else if (m.status === "down") down++;
    }
    return { up, down, paused, maintenance };
  });

  return (
    <nav class="rail" aria-label="Primary">
      <div class="rail-top">
        <div class="rail-mark" aria-hidden="true">
          V
        </div>
        <div class="rail-nav">
          {NAV_ITEMS.map((item) => (
            <button
              type="button"
              class="rail-item"
              aria-label={item.label}
              title={item.label}
              aria-current={(props.activeView ?? "dashboard") === item.key ? "page" : undefined}
              onClick={() => props.onNavigate?.(item.key)}
            >
              {item.icon}
            </button>
          ))}
        </div>
      </div>

      <div class="rail-summary mono" aria-live="polite">
        <span class="stat up">{summary().up}&#9650;</span>
        <span class={`stat down ${summary().down > 0 ? "pulsing" : ""}`}>{summary().down}&#9660;</span>
        <span class="stat paused">{summary().paused}&#10074;&#10074;</span>
        <span class="stat maintenance">{summary().maintenance}&#128736;</span>
      </div>
    </nav>
  );
};

export default Rail;
