import { createResource, For, Show, type Component } from "solid-js";
import * as api from "../api";
import type { Bar } from "../api";

export interface UptimeBarProps {
  monitorId: number;
  days?: number;
  compact?: boolean;
}

type Tier = "up" | "degraded" | "down" | "nodata";

function tierOf(bar: Bar): Tier {
  if (!bar.has_data) return "nodata";
  if (bar.uptime_pct >= 99.99) return "up";
  if (bar.uptime_pct >= 50) return "degraded";
  return "down";
}

function tooltipFor(bar: Bar): string {
  return `${bar.day} · ${bar.uptime_pct.toFixed(2)}% · ${bar.incidents} incident(s) · ${bar.down_seconds}s down`;
}

/**
 * The signature 90-day uptime bar (spec §11.5). One thin rounded segment per
 * day, colored by that day's rollup tier. `compact` renders a smaller strip
 * of the most recent ~45 days for the monitor card, with no labels/legend;
 * the full (panel) form renders up to `days` (default 90) segments with end
 * labels and a legend row.
 */
const UptimeBar: Component<UptimeBarProps> = (props) => {
  const totalDays = () => props.days ?? 90;

  const [bars] = createResource(
    () => [props.monitorId, totalDays()] as const,
    ([id, days]) => api.getBars(id, days).catch(() => [] as Bar[]),
  );

  const visibleBars = () => {
    const all = bars() ?? [];
    if (!props.compact) return all;
    const compactCount = 45;
    return all.length > compactCount ? all.slice(all.length - compactCount) : all;
  };

  return (
    <div class={`uptime-bar ${props.compact ? "compact" : "full"}`}>
      <div class="uptime-bar-row">
        <Show when={!props.compact}>
          <span class="uptime-bar-endlabel">90 days ago</span>
        </Show>
        <div class="uptime-bar-track">
          <Show
            when={visibleBars().length > 0}
            fallback={<div class="uptime-bar-empty" aria-hidden="true" />}
          >
            <For each={visibleBars()}>
              {(bar) => (
                <span
                  class={`uptime-segment tier-${tierOf(bar)}`}
                  data-tier={tierOf(bar)}
                  data-testid="uptime-segment"
                  title={tooltipFor(bar)}
                />
              )}
            </For>
          </Show>
        </div>
        <Show when={!props.compact}>
          <span class="uptime-bar-endlabel">Today</span>
        </Show>
      </div>
      <Show when={!props.compact}>
        <div class="uptime-bar-legend">
          <span class="legend-item"><span class="legend-swatch tier-up" /> Up</span>
          <span class="legend-item"><span class="legend-swatch tier-degraded" /> Degraded</span>
          <span class="legend-item"><span class="legend-swatch tier-down" /> Down</span>
          <span class="legend-item"><span class="legend-swatch tier-nodata" /> No data</span>
        </div>
      </Show>
    </div>
  );
};

export default UptimeBar;
