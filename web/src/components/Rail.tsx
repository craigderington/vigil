import { createMemo, type Component } from "solid-js";

export type RailView = "dashboard" | "settings";

export interface RailProps {
  monitors: any[];
  /** Which top-level view is active; drives `aria-current`. Defaults to "dashboard". */
  activeView?: RailView;
  /** Fired with the clicked item's key. Items without a wired screen yet
   * (Incidents/Notifications/Maintenance) still fire so a future screen can
   * hook in; App.tsx currently routes anything but "settings" back to the
   * dashboard grid. */
  onNavigate?: (key: RailView | "incidents" | "notifications" | "maintenance") => void;
}

const NAV_ITEMS: { icon: string; label: string; key: RailView | "incidents" | "notifications" | "maintenance" }[] = [
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
      paused = 0;
    for (const m of props.monitors) {
      if (m.is_paused || m.status === "paused") paused++;
      else if (m.status === "up") up++;
      else if (m.status === "down") down++;
    }
    return { up, down, paused };
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
      </div>
    </nav>
  );
};

export default Rail;
