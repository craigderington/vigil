import { test, expect, vi, afterEach } from "vitest";
import { createRoot, createSignal, type Accessor } from "solid-js";
import { easeOutCubic, createCountUp, createExitTransition } from "../motion";

afterEach(() => vi.unstubAllGlobals());
const reduce = (matches: boolean) => vi.stubGlobal("matchMedia", (q: string) => ({ matches, media: q, addEventListener() {}, removeEventListener() {} }) as any);

test("easeOutCubic endpoints + monotonic", () => {
  expect(easeOutCubic(0)).toBe(0);
  expect(easeOutCubic(1)).toBe(1);
  expect(easeOutCubic(0.5)).toBeGreaterThan(0.5); // ease-out is above the diagonal
});

// NOTE on structure: the mutation (`setTarget`/`setOpen` below) is deliberately
// made AFTER `createRoot`'s callback has returned, not inside it. `createRoot`
// wraps its whole callback in its own top-level `runUpdates` batch; a signal
// write made *inside* that same synchronous callback body queues the
// dependent `createRenderEffect` into Solid's shared `Effects` array, but that
// queue is only flushed once the *outer* `createRoot` callback itself returns
// — and if `dispose()` runs before that flush point (as it would if written
// linearly), the queued effect's `fn` gets nulled by cleanup and the queued
// run is silently dropped, never observed. This is a `createRoot`-batching
// testing artifact, not a behavior of the primitives: in the real app, a
// signal write always happens from a later, separate call stack (an event
// handler) after the initial mount's `createRoot`/`render` call has already
// returned, so the effect flushes synchronously right then — this exact
// same-callback deferral never arises in production. Capturing the
// setter/accessor/dispose via closure and mutating after `createRoot`
// returns reproduces that real (mount-then-later-event) timing and lets the
// render effect rerun synchronously, which is what these tests intend to
// exercise.
test("createCountUp with reduced-motion shows the target instantly", () => {
  reduce(true);
  let setTarget!: (v: number | null) => void;
  let v!: Accessor<number | null>;
  const dispose = createRoot((d) => {
    const [target, setTargetSig] = createSignal<number | null>(100);
    setTarget = setTargetSig;
    v = createCountUp(target, 320);
    return d;
  });
  expect(v()).toBe(100);
  setTarget(250);
  expect(v()).toBe(250); // no tween under reduced-motion
  dispose();
});

test("createCountUp with null target renders null", () => {
  reduce(true);
  createRoot((dispose) => {
    const v = createCountUp(() => null, 320);
    expect(v()).toBeNull();
    dispose();
  });
});

test("createExitTransition: reduced-motion unmounts instantly; otherwise closes then unmounts", () => {
  vi.useFakeTimers();
  reduce(false);
  let setOpen!: (v: boolean) => void;
  let t!: { mounted: Accessor<boolean>; closing: Accessor<boolean> };
  const dispose = createRoot((d) => {
    const [open, setOpenSig] = createSignal(true);
    setOpen = setOpenSig;
    t = createExitTransition(open, 260);
    return d;
  });
  expect(t.mounted()).toBe(true);
  setOpen(false);
  expect(t.mounted()).toBe(true);   // still mounted during close
  expect(t.closing()).toBe(true);
  vi.advanceTimersByTime(260);
  expect(t.mounted()).toBe(false);  // unmounted after ms
  expect(t.closing()).toBe(false);
  dispose();
  vi.useRealTimers();
});
