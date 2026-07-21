import { test, expect, beforeEach } from "vitest";
import { presetById, applyAccent, storedAccentId, ACCENT_PRESETS, DEFAULT_ACCENT } from "../accent";

beforeEach(() => { localStorage.clear(); document.documentElement.removeAttribute("style"); });

test("presetById resolves id, legacy hex, and unknown->default", () => {
  expect(presetById("the-open-yellow").id).toBe("the-open-yellow");
  expect(presetById("#3FC8E4").id).toBe("cyan");        // legacy/default hex
  expect(presetById("nonsense").id).toBe(DEFAULT_ACCENT);
  expect(presetById(null).id).toBe(DEFAULT_ACCENT);
});

test("applyAccent sets all four CSS vars and persists the id", () => {
  applyAccent("the-open-yellow");
  const s = document.documentElement.style;
  const p = ACCENT_PRESETS.find((x) => x.id === "the-open-yellow")!;
  expect(s.getPropertyValue("--accent").trim()).toBe(p.accent);
  expect(s.getPropertyValue("--accent-weak").trim()).toBe(p.accentWeak);
  expect(s.getPropertyValue("--focus-ring").trim()).toBe(p.focusRing);
  expect(s.getPropertyValue("--accent-fg").trim()).toBe(p.accentFg);
  expect(localStorage.getItem("vigil.accent")).toBe("the-open-yellow");
  expect(storedAccentId()).toBe("the-open-yellow");
});

test("storedAccentId defaults when nothing stored", () => {
  expect(storedAccentId()).toBe(DEFAULT_ACCENT);
});
