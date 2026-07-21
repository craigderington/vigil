import { createSignal, createRenderEffect, on, onCleanup, type Accessor } from "solid-js";

/** Ease-out cubic on [0,1]. */
export function easeOutCubic(t: number): number { const u = 1 - t; return 1 - u * u * u; }

// NOTE: both primitives use `createRenderEffect`, NOT `createEffect`. A
// `createEffect`'s FIRST run is deferred (queued) past the synchronous setup —
// which both breaks the synchronous unit tests (the value/mount state isn't set
// yet when read) AND, for the exit transition, can swallow the very first
// open→close transition. `createRenderEffect` runs synchronously at creation and
// re-runs synchronously on dependency change, so init state is correct
// immediately (and the count-up has no one-frame null before paint).

function prefersReducedMotion(): boolean {
  return typeof window !== "undefined" && !!window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
}

/** Tweens the DISPLAYED number toward `target` over `ms` (ease-out) via rAF.
 *  Reduced-motion (or an unchanged/absent target) snaps to the value. A `null`
 *  target renders `null` (the caller shows a placeholder). Must run inside a
 *  reactive root (a component/`createRoot`). */
export function createCountUp(target: Accessor<number | null>, ms = 320): Accessor<number | null> {
  const [display, setDisplay] = createSignal<number | null>(null);
  let raf: number | undefined;
  createRenderEffect(on(target, (to) => {
    if (raf) cancelAnimationFrame(raf);
    if (to == null) { setDisplay(null); return; }
    const cur = display();
    const from = typeof cur === "number" ? cur : 0;
    if (prefersReducedMotion() || from === to) { setDisplay(to); return; }
    const start = performance.now();
    const step = (now: number) => {
      const t = Math.min(1, (now - start) / ms);
      setDisplay(t >= 1 ? to : from + (to - from) * easeOutCubic(t));
      if (t < 1) raf = requestAnimationFrame(step);
    };
    raf = requestAnimationFrame(step);
  }));
  onCleanup(() => { if (raf) cancelAnimationFrame(raf); });
  return display;
}

/** Delayed-unmount lifecycle for a slide-over. `mounted()` stays true through
 *  the close animation; `closing()` is true during it (drives a `.closing`
 *  CSS class). Reduced-motion (or ms 0) unmounts instantly. Must run inside a
 *  reactive root. */
export function createExitTransition(open: Accessor<boolean>, ms = 260): { mounted: Accessor<boolean>; closing: Accessor<boolean> } {
  const [mounted, setMounted] = createSignal(open());
  const [closing, setClosing] = createSignal(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  createRenderEffect(on(open, (isOpen) => {
    clearTimeout(timer);
    if (isOpen) { setClosing(false); setMounted(true); return; }
    if (!mounted()) return;   // on init with open=false → nothing to close
    if (prefersReducedMotion() || ms === 0) { setClosing(false); setMounted(false); return; }
    setClosing(true);
    timer = setTimeout(() => { setClosing(false); setMounted(false); }, ms);
  }));
  onCleanup(() => clearTimeout(timer));
  return { mounted, closing };
}
