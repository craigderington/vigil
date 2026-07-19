import { createMemo, type Component } from "solid-js";
import { inMaintenance } from "../maintenance_ids";

export type RailView = "dashboard" | "settings" | "incidents" | "maintenance";

/** One consistent monochrome line-icon set (Feather-style, `currentColor`),
 * so the rail never mixes flat glyphs with full-color emoji. */
const ICON_PATHS: Record<string, string> = {
  // grid — dashboard
  dashboard: "M3 3h7v7H3zM14 3h7v7h-7zM14 14h7v7h-7zM3 14h7v7H3z",
  // alert-triangle — incidents
  incidents: "M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0zM12 9v4M12 17h.01",
  // bell — notifications
  notifications: "M18 8A6 6 0 0 0 6 8c0 7-3 9-3 9h18s-3-2-3-9M13.73 21a2 2 0 0 1-3.46 0",
  // wrench — maintenance
  maintenance: "M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z",
  // gear — settings
  settings: "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z",
};

const NavIcon: Component<{ name: string }> = (p) => (
  <svg
    class="rail-icon"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="2"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
  >
    <path d={ICON_PATHS[p.name]} />
  </svg>
);

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
  { icon: "dashboard", label: "Dashboard", key: "dashboard" },
  { icon: "incidents", label: "Incidents", key: "incidents" },
  { icon: "notifications", label: "Notifications", key: "notifications" },
  { icon: "maintenance", label: "Maintenance", key: "maintenance" },
  { icon: "settings", label: "Settings", key: "settings" },
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
              <NavIcon name={item.icon} />
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
