// How big a change row draws: "compact" is the jj-native one-line row,
// "classic" the taller two-line row of traditional Git clients. A reading
// preference like the workbench view mode — it persists across sessions
// and never leaves the machine. A shared session (like dnd's drag) so
// every surface that renders rows reads the same answer without threading
// a prop through both view modes.

import type { RowDensity } from "./graph";

const DENSITY_KEY = "jiji.workbench.density";

function storedDensity(): RowDensity {
  try {
    return localStorage.getItem(DENSITY_KEY) === "classic"
      ? "classic"
      : "compact";
  } catch {
    return "compact";
  }
}

export const density = $state({ mode: storedDensity() });

export function setDensity(mode: RowDensity): void {
  density.mode = mode;
  try {
    localStorage.setItem(DENSITY_KEY, mode);
  } catch {
    // Storage full or unavailable only loses the stickiness.
  }
}

export function toggleDensity(): void {
  setDensity(density.mode === "classic" ? "compact" : "classic");
}
