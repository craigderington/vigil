import { createMemo, type Component } from "solid-js";

export interface RailProps {
  monitors: any[];
}

const NAV_ITEMS: { icon: string; label: string }[] = [
  { icon: "▣", label: "Dashboard" },
  { icon: "⚠", label: "Incidents" },
  { icon: "\u{1F514}", label: "Notifications" },
  { icon: "\u{1F6E0}", label: "Maintenance" },
  { icon: "⚙", label: "Settings" },
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
          {NAV_ITEMS.map((item, i) => (
            <button
              type="button"
              class="rail-item"
              aria-label={item.label}
              title={item.label}
              aria-current={i === 0 ? "page" : undefined}
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
