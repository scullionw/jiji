// Theme registry, preference, and resolution. The inline script in app.html
// applies the stored preference before first paint (so the window never
// flashes the wrong palette); this module owns the theme from then on.
//
// The preference is three-part: a mode (system/light/dark) plus one chosen
// palette per scheme, so following the OS switches between *your* dark and
// *your* light theme rather than resetting to defaults.
//
// Persistence is localStorage rather than the Tauri store: the boot script
// needs a synchronous read, and double-bookkeeping two stores isn't worth it.
//
// Themes are a supporter perk: an unregistered copy is pinned to the default
// light palette and the picker is locked (see $lib/license/state.svelte.ts).

import { getCurrentWindow } from "@tauri-apps/api/window";
import { license } from "$lib/license/state.svelte";

export type ThemeMode = "system" | "light" | "dark";
export type ThemeScheme = "light" | "dark";

export interface ThemeDef {
  id: string;
  label: string;
  scheme: ThemeScheme;
  /* Representative colors (bg-1, accent) for picker swatches. */
  swatch: { bg: string; accent: string };
}

// Palettes live in tokens.css under [data-theme="<id>"]. The boot script in
// app.html and scripts/harness/harness.js carry copies of the id lists;
// keep them in sync when adding a theme.
export const themes: ThemeDef[] = [
  { id: "midnight", label: "Midnight", scheme: "dark", swatch: { bg: "#15151a", accent: "#8b8df2" } },
  { id: "graphite", label: "Graphite", scheme: "dark", swatch: { bg: "#181818", accent: "#7fa3cc" } },
  { id: "abyss", label: "Abyss", scheme: "dark", swatch: { bg: "#0d1422", accent: "#45b8e0" } },
  { id: "moss", label: "Moss", scheme: "dark", swatch: { bg: "#131a14", accent: "#7bc788" } },
  { id: "ember", label: "Ember", scheme: "dark", swatch: { bg: "#1a1614", accent: "#e8875a" } },
  { id: "paper", label: "Paper", scheme: "light", swatch: { bg: "#f6f6f9", accent: "#5558d9" } },
  { id: "linen", label: "Linen", scheme: "light", swatch: { bg: "#f6f2ea", accent: "#c05f3c" } },
  { id: "glacier", label: "Glacier", scheme: "light", swatch: { bg: "#f2f6fa", accent: "#2272c8" } },
  { id: "meadow", label: "Meadow", scheme: "light", swatch: { bg: "#f3f6f1", accent: "#2c8050" } },
  { id: "dawn", label: "Dawn", scheme: "light", swatch: { bg: "#f8f2f3", accent: "#b13767" } },
];

const DEFAULT_DARK = "midnight";
const DEFAULT_LIGHT = "paper";
const STORAGE_KEY = "jiji-theme";

export const theme = $state({
  mode: "system" as ThemeMode,
  dark: DEFAULT_DARK,
  light: DEFAULT_LIGHT,
  // The palette currently on screen (a theme id).
  resolved: DEFAULT_DARK,
});

function themeById(id: unknown): ThemeDef | undefined {
  return themes.find((t) => t.id === id);
}

function systemScheme(): ThemeScheme {
  return window.matchMedia("(prefers-color-scheme: light)").matches
    ? "light"
    : "dark";
}

export function resolvedScheme(): ThemeScheme {
  return theme.mode === "system" ? systemScheme() : theme.mode;
}

function apply(animate: boolean): void {
  // Unregistered copies are pinned to the default light palette; registered
  // users get their full chosen preference.
  theme.resolved = license.registered
    ? resolvedScheme() === "light"
      ? theme.light
      : theme.dark
    : DEFAULT_LIGHT;
  const root = document.documentElement;
  const swap = () => {
    root.dataset.theme = theme.resolved;
  };
  // Cross-fade through a View Transition: the old surface is snapshotted
  // once and composited over the new one, so the fade costs the same with
  // a 20k-line diff mounted as with an empty pane. (Transitioning colors
  // on every element instead made switching O(DOM) and visibly janky.)
  // A second switch mid-fade skips the first; that's the behavior we want.
  if (
    animate &&
    !window.matchMedia("(prefers-reduced-motion: reduce)").matches &&
    typeof document.startViewTransition === "function"
  ) {
    document.startViewTransition(swap);
  } else {
    swap();
  }
  // Keep native chrome (overlay traffic lights, context menus) in step.
  // No-op outside Tauri (vite dev in a plain browser).
  try {
    getCurrentWindow()
      .setTheme(
        license.registered
          ? theme.mode === "system"
            ? null
            : theme.mode
          : "light",
      )
      .catch(() => {});
  } catch {
    /* not running under Tauri */
  }
}

function persist(): void {
  localStorage.setItem(
    STORAGE_KEY,
    JSON.stringify({ mode: theme.mode, dark: theme.dark, light: theme.light }),
  );
}

export function initTheme(): void {
  const raw = localStorage.getItem(STORAGE_KEY);
  if (raw?.startsWith("{")) {
    try {
      const stored = JSON.parse(raw);
      if (["system", "light", "dark"].includes(stored.mode)) {
        theme.mode = stored.mode;
      }
      if (themeById(stored.dark)?.scheme === "dark") theme.dark = stored.dark;
      if (themeById(stored.light)?.scheme === "light") {
        theme.light = stored.light;
      }
    } catch {
      /* corrupt preference; fall through to defaults */
    }
  } else if (raw === "light" || raw === "dark" || raw === "system") {
    // Legacy format: a bare mode string from before named palettes.
    theme.mode = raw;
  }
  window
    .matchMedia("(prefers-color-scheme: light)")
    .addEventListener("change", () => {
      if (theme.mode === "system") apply(true);
    });
  apply(false);
}

export function setMode(mode: ThemeMode): void {
  if (!license.registered) return;
  theme.mode = mode;
  persist();
  apply(true);
}

export function selectTheme(id: string): void {
  if (!license.registered) return;
  const def = themeById(id);
  if (!def) return;
  theme[def.scheme] = id;
  // Picking a palette you can't currently see would be a dead click;
  // follow it with the mode when the schemes differ.
  if (resolvedScheme() !== def.scheme) theme.mode = def.scheme;
  persist();
  apply(true);
}

/** Re-apply the active theme — called when registration status flips. */
export function refreshTheme(): void {
  apply(true);
}
