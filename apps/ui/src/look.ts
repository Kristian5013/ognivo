// How the GUI looks, as the user sets it: the theme, the accent, the scale, the density and
// the motion. Kept in the page's storage and put on <html> as attributes and variables,
// which index.css reads; the scale is Electron's zoom where there is one.

export type Theme = "system" | "dark" | "light";
export type Accent = "ember" | "amber" | "rose" | "violet" | "sky" | "jade";
export type Density = "comfortable" | "compact";
export type Motion = "system" | "full" | "reduced";

export interface Look {
  theme: Theme;
  accent: Accent;
  /** Of the default size, from 0.8 to 1.4. */
  scale: number;
  density: Density;
  motion: Motion;
}

/** Each accent in the two themes, at the contrast each needs. */
export const ACCENTS: Record<Accent, { name: string; dark: string; light: string }> = {
  ember: { name: "Ember", dark: "#f0883e", light: "#bc4c00" },
  amber: { name: "Amber", dark: "#e3b341", light: "#9a6700" },
  rose: { name: "Rose", dark: "#f778ba", light: "#bf3989" },
  violet: { name: "Violet", dark: "#a371f7", light: "#8250df" },
  sky: { name: "Sky", dark: "#58a6ff", light: "#0969da" },
  jade: { name: "Jade", dark: "#3fb950", light: "#1a7f37" },
};

export const SCALES = [0.9, 1, 1.1, 1.2, 1.3];

export const DEFAULT_LOOK: Look = { theme: "system", accent: "ember", scale: 1, density: "comfortable", motion: "system" };

const KEY = "ognivo.look";

function valid(l: Partial<Look>): Look {
  const d = DEFAULT_LOOK;
  return {
    theme: l.theme === "dark" || l.theme === "light" ? l.theme : d.theme,
    accent: l.accent && l.accent in ACCENTS ? l.accent : d.accent,
    scale: typeof l.scale === "number" && l.scale >= 0.8 && l.scale <= 1.4 ? l.scale : d.scale,
    density: l.density === "compact" ? "compact" : d.density,
    motion: l.motion === "full" || l.motion === "reduced" ? l.motion : d.motion,
  };
}

function load(): Look {
  try {
    return valid(JSON.parse(localStorage.getItem(KEY) ?? "{}") as Partial<Look>);
  } catch {
    return DEFAULT_LOOK;
  }
}

let current = DEFAULT_LOOK;
const listeners = new Set<() => void>();
let unfollow = () => {};

/** Puts `look` on the document, following the system's theme while it says `system`. */
function apply(look: Look): void {
  unfollow();
  const root = document.documentElement;
  const dark = typeof matchMedia === "function" ? matchMedia("(prefers-color-scheme: dark)") : undefined;
  const set = () => {
    const isDark = look.theme === "dark" || (look.theme === "system" && (dark?.matches ?? true));
    root.dataset.theme = isDark ? "dark" : "light";
    root.style.setProperty("--spark", ACCENTS[look.accent][isDark ? "dark" : "light"]);
  };
  set();
  root.dataset.density = look.density;
  root.dataset.motion = look.motion;
  if (window.ognivo?.zoom) {
    window.ognivo.zoom(look.scale);
    root.style.removeProperty("zoom");
  } else {
    root.style.setProperty("zoom", String(look.scale));
  }
  if (look.theme === "system" && dark) {
    dark.addEventListener("change", set);
    unfollow = () => dark.removeEventListener("change", set);
  } else {
    unfollow = () => {};
  }
}

/** Reads the saved look and puts it on the document: before the first render. */
export function startLook(): void {
  current = load();
  apply(current);
}

export function look(): Look {
  return current;
}

export function setLook(next: Look): void {
  current = valid(next);
  try {
    localStorage.setItem(KEY, JSON.stringify(current));
  } catch {
    // a private window: the look lasts as long as the page
  }
  apply(current);
  for (const l of listeners) l();
}

export function subscribeLook(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}
