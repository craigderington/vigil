/** Accent theming (P4.6). The `appearance.accent` setting is backend-plumbed;
 *  this module applies a chosen preset to the CSS vars + persists it locally
 *  for a no-flash boot. Accents are all NON-status colors (spec §11.1). */
export type AccentId = "cyan" | "the-open-yellow" | "magenta" | "teal";

export interface AccentPreset {
  id: AccentId;
  name: string;
  accent: string;
  accentWeak: string;
  focusRing: string;
  accentFg: string; // contrast-correct foreground for text on an accent background (.btn-accent)
}

export const ACCENT_PRESETS: AccentPreset[] = [
  { id: "cyan",            name: "Cyan",    accent: "#3FC8E4", accentWeak: "rgba(63,200,228,0.14)",  focusRing: "#3FC8E4", accentFg: "#04222b" },
  { id: "the-open-yellow", name: "Yellow",  accent: "#FFBA00", accentWeak: "rgba(255,186,0,0.14)",   focusRing: "#FFBA00", accentFg: "#241a02" },
  { id: "magenta",         name: "Magenta", accent: "#E879C9", accentWeak: "rgba(232,121,201,0.16)", focusRing: "#E879C9", accentFg: "#2a0a22" },
  { id: "teal",            name: "Teal",    accent: "#2DD4BF", accentWeak: "rgba(45,212,191,0.16)",  focusRing: "#2DD4BF", accentFg: "#04231f" },
];

export const DEFAULT_ACCENT: AccentId = "cyan";
const KEY = "vigil.accent";

/** Tolerant resolver: a preset id, a legacy/default accent hex (e.g. "#3FC8E4"),
 *  or anything unknown -> DEFAULT_ACCENT. Case-insensitive on the hex. */
export function presetById(v: string | null | undefined): AccentPreset {
  if (v) {
    const byId = ACCENT_PRESETS.find((p) => p.id === v);
    if (byId) return byId;
    const byHex = ACCENT_PRESETS.find((p) => p.accent.toLowerCase() === v.toLowerCase());
    if (byHex) return byHex;
  }
  return ACCENT_PRESETS.find((p) => p.id === DEFAULT_ACCENT)!;
}

export function applyAccent(v: string): void {
  const p = presetById(v);
  const s = document.documentElement.style;
  s.setProperty("--accent", p.accent);
  s.setProperty("--accent-weak", p.accentWeak);
  s.setProperty("--focus-ring", p.focusRing);
  s.setProperty("--accent-fg", p.accentFg);
  try { localStorage.setItem(KEY, p.id); } catch { /* private mode / disabled storage */ }
}

export function storedAccentId(): AccentId {
  let raw: string | null = null;
  try { raw = localStorage.getItem(KEY); } catch { /* ignore */ }
  return presetById(raw).id;
}
