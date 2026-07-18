import { createEffect, createResource, createSignal, For, on, onCleanup, Show, type Component } from "solid-js";
// Type-only import: erased at compile time, so it never pulls uPlot's
// runtime module into the graph. uPlot's module has TOP-LEVEL side effects
// (it calls `matchMedia` at import time to track devicePixelRatio changes),
// which throws under jsdom (no `matchMedia`). The actual class is loaded
// lazily via dynamic `import("uplot")`, only once the jsdom guard below
// confirms we're in a real, sized browser layout — so test runs never
// import the module at all.
import type UPlot from "uplot";
import "uplot/dist/uPlot.min.css";
import * as api from "../api";
import type { Incident, SeriesPoint, StatsRange } from "../api";

export interface ResponseChartProps {
  monitorId: number;
}

const RANGES: StatsRange[] = ["24h", "7d"];

/** Width in seconds of a given chart range preset. */
function rangeSeconds(range: string): number {
  return range === "7d" ? 7 * 86400 : 86400;
}

function prefersReducedMotion(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}

/** An incident span clipped to `[from, to]`, as a fraction (0..1) of that window. */
interface ShadeBand {
  id: number;
  leftPct: number;
  widthPct: number;
}

function bandsFor(incidents: Incident[], from: number, to: number): ShadeBand[] {
  const span = to - from;
  if (span <= 0) return [];
  const out: ShadeBand[] = [];
  for (const inc of incidents) {
    const start = Math.max(inc.started_at, from);
    const end = Math.min(inc.resolved_at ?? to, to);
    if (end <= start) continue;
    out.push({
      id: inc.id,
      leftPct: ((start - from) / span) * 100,
      widthPct: ((end - start) / span) * 100,
    });
  }
  return out;
}

/**
 * Response-time chart (§11.6 #4): ms over time via uPlot, with a range
 * selector and incident spans shaded beneath the curve. Incident shading is
 * a simple absolutely-positioned overlay div per span (clipped to the
 * visible range, open incidents clipped to "now") rather than a uPlot draw
 * hook — simpler to reason about and safe under jsdom, where uPlot itself
 * is never asked to lay out a zero-size canvas (see the guard below).
 */
const ResponseChart: Component<ResponseChartProps> = (props) => {
  const [range, setRange] = createSignal<StatsRange>("24h");

  const [series] = createResource(
    () => [props.monitorId, range()] as const,
    ([id, r]) => api.getSeries(id, r).catch(() => [] as SeriesPoint[]),
  );

  const [incidents] = createResource(
    () => [props.monitorId, range()] as const,
    ([id, r]) => api.getIncidents(r, id).catch(() => [] as Incident[]),
  );

  let containerRef: HTMLDivElement | undefined;
  let plot: UPlot | undefined;

  function destroyPlot() {
    plot?.destroy();
    plot = undefined;
  }

  // The shading (and chart axis, below) window is derived from the
  // *selected range*, not from the data that happened to survive — the
  // backend omits empty buckets, so deriving `from`/`to` from
  // data[0]/data[last] would clip away an incident that occurred before the
  // first surviving point (e.g. a check gap at the start of the window)
  // even though it's within the selected 24h/7d range.
  const windowRange = () => {
    const nowSecs = Math.floor(Date.now() / 1000);
    return { from: nowSecs - rangeSeconds(range()), to: nowSecs };
  };

  createEffect(
    on([series, () => range()], () => {
      destroyPlot();

      const data = series() ?? [];
      // jsdom guard: uPlot needs a real, sized layout. In tests clientWidth
      // is 0 and uPlot can throw trying to lay out a zero-width canvas —
      // never let that happen. Render the empty-state instead, and never
      // even load the uPlot module (see the import comment above).
      if (
        typeof window === "undefined" ||
        !containerRef ||
        containerRef.clientWidth <= 0 ||
        data.length === 0
      ) {
        return;
      }

      const target = containerRef;
      let cancelled = false;
      onCleanup(() => {
        cancelled = true;
      });

      import("uplot").then(({ default: UPlotCtor }) => {
        // The effect may have re-run (range/series changed) or the
        // component may have unmounted while this dynamic import was in
        // flight; bail out rather than mounting a stale/orphaned chart.
        if (cancelled || !target.isConnected) return;

        const xs = data.map((p) => p.t);
        const ys = data.map((p) => (p.ms == null ? null : p.ms));
        const w = windowRange();

        // uPlot has no global "animate" switch; the one motion knob it
        // exposes here is the hover cursor, which we drop entirely under
        // prefers-reduced-motion (the chart itself is already static/non-
        // animated — there's no entrance transition to disable).
        const opts: UPlot.Options = {
          width: target.clientWidth,
          height: 200,
          cursor: prefersReducedMotion() ? { show: false } : { points: { size: 6 } },
          // Pin the x-axis to the selected range window (not the sparse
          // data bounds) so the axis is consistent with the incident
          // shading overlay above.
          scales: { x: { min: w.from, max: w.to } },
          series: [
            {},
            {
              label: "ms",
              stroke: "#3FC8E4",
              fill: "rgba(63,200,228,0.14)",
              width: 2,
              points: { show: false },
            },
          ],
          axes: [
            { stroke: "#5F6C86", grid: { stroke: "#161435" } },
            { stroke: "#5F6C86", grid: { stroke: "#161435" } },
          ],
          legend: { show: false },
        };

        destroyPlot();
        plot = new UPlotCtor(opts, [xs, ys] as unknown as UPlot.AlignedData, target);
      });
    }),
  );

  onCleanup(destroyPlot);

  const shadeBands = () => {
    const w = windowRange();
    return bandsFor(incidents() ?? [], w.from, w.to);
  };

  return (
    <section class="detail-section response-chart">
      <div class="detail-section-head">
        <h3 class="detail-section-h">Response time</h3>
        <div class="range-toggle" role="group" aria-label="Chart range">
          <For each={RANGES}>
            {(r) => (
              <button
                type="button"
                class="chip"
                aria-pressed={range() === r}
                onClick={() => setRange(r)}
              >
                {r}
              </button>
            )}
          </For>
        </div>
      </div>

      <Show
        when={(series()?.length ?? 0) > 0}
        fallback={<div class="response-chart-empty">No response-time data yet</div>}
      >
        <div class="response-chart-wrap">
          <div class="response-chart-canvas" ref={containerRef} />
          <div class="response-chart-bands" aria-hidden="true">
            <For each={shadeBands()}>
              {(band) => (
                <div
                  class="response-chart-band"
                  data-testid="incident-band"
                  style={{ left: `${band.leftPct}%`, width: `${band.widthPct}%` }}
                />
              )}
            </For>
          </div>
        </div>
      </Show>
    </section>
  );
};

export default ResponseChart;
