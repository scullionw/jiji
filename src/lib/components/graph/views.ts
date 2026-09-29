// The workbench's view lenses: which slice of the repo the snapshot
// renders. Pure data and persistence helpers — no Svelte. The lens itself
// is computed by the backend (`SnapshotView`); this module owns how the
// picker presents the presets and how a choice sticks per repo.

import type { SnapshotView } from "$lib/bindings/SnapshotView";

export interface ViewPreset {
  view: SnapshotView;
  label: string;
  /** One line under the label in the picker. */
  description: string;
}

/** The fixed presets, picker order. Custom is not listed — it is the
 * picker's revset input, not a row. */
export const VIEW_PRESETS: ViewPreset[] = [
  {
    view: { kind: "workstreams" },
    label: "Workstreams",
    description: "Your mutable work on the bases it sits on",
  },
  {
    view: { kind: "team" },
    label: "Team",
    description: "Yours plus everyone's branches, local and remote",
  },
  {
    view: { kind: "recent" },
    label: "Recent",
    description: "All activity from the last two weeks",
  },
  {
    view: { kind: "mine" },
    label: "Mine",
    description: "Everything you authored, including landed work",
  },
  {
    view: { kind: "trunk" },
    label: "Trunk history",
    description: "What landed, newest first",
  },
  {
    view: { kind: "all" },
    label: "All history",
    description: "The entire visible history",
  },
];

/** The picker/trigger label for a lens, including custom. */
export function viewLabel(view: SnapshotView): string {
  if (view.kind === "custom") return "Custom revset";
  return (
    VIEW_PRESETS.find((preset) => preset.view.kind === view.kind)?.label ??
    "Workstreams"
  );
}

export function sameView(a: SnapshotView, b: SnapshotView): boolean {
  if (a.kind !== b.kind) return false;
  return a.kind !== "custom" || b.kind !== "custom" || a.revset === b.revset;
}

/** Parses a persisted lens, refusing anything that is not a valid
 * `SnapshotView` shape (older builds, hand-edited storage). */
export function parseStoredView(text: string | null): SnapshotView | null {
  if (!text) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const view = parsed as { kind?: unknown; revset?: unknown };
  if (view.kind === "custom") {
    return typeof view.revset === "string" && view.revset.trim() !== ""
      ? { kind: "custom", revset: view.revset }
      : null;
  }
  const known = VIEW_PRESETS.some((preset) => preset.view.kind === view.kind);
  return known ? ({ kind: view.kind } as SnapshotView) : null;
}

/** A named custom revset — a user-made lens preset, persisted per repo. */
export interface SavedView {
  name: string;
  revset: string;
}

/** Parses a persisted saved-view list, dropping anything malformed
 * (older builds, hand-edited storage) and later duplicates of a name. */
export function parseSavedViews(value: unknown): SavedView[] {
  if (!Array.isArray(value)) return [];
  const views: SavedView[] = [];
  const seen = new Set<string>();
  for (const entry of value) {
    if (typeof entry !== "object" || entry === null) continue;
    const { name, revset } = entry as { name?: unknown; revset?: unknown };
    if (typeof name !== "string" || typeof revset !== "string") continue;
    const cleanName = name.trim();
    const cleanRevset = revset.trim();
    if (cleanName === "" || cleanRevset === "" || seen.has(cleanName)) continue;
    seen.add(cleanName);
    views.push({ name: cleanName, revset: cleanRevset });
  }
  return views;
}

/** Saves a view. A new name appends (list order is save order); saving an
 * existing name replaces that view's revset in place. Refuses (null) when
 * either field trims to nothing. */
export function upsertSavedView(
  views: SavedView[],
  name: string,
  revset: string,
): SavedView[] | null {
  const cleanName = name.trim();
  const cleanRevset = revset.trim();
  if (cleanName === "" || cleanRevset === "") return null;
  const saved = { name: cleanName, revset: cleanRevset };
  const existing = views.findIndex((view) => view.name === cleanName);
  if (existing < 0) return [...views, saved];
  return views.map((view, index) => (index === existing ? saved : view));
}

/** Renames a view. Refuses (null) when the target is gone, the new name
 * trims to nothing, or another view already owns the new name. */
export function renameSavedView(
  views: SavedView[],
  name: string,
  newName: string,
): SavedView[] | null {
  const cleanName = newName.trim();
  if (cleanName === "" || !views.some((view) => view.name === name))
    return null;
  if (cleanName !== name && views.some((view) => view.name === cleanName))
    return null;
  return views.map((view) =>
    view.name === name ? { ...view, name: cleanName } : view,
  );
}

export function deleteSavedView(views: SavedView[], name: string): SavedView[] {
  return views.filter((view) => view.name !== name);
}

// The lens choice is a per-repo reading preference, like the workbench view
// mode — it survives restarts but never leaves the machine.
const VIEW_STORE_PREFIX = "jiji.workbench.lens:";

export function persistedView(repoPath: string): SnapshotView {
  try {
    return (
      parseStoredView(localStorage.getItem(VIEW_STORE_PREFIX + repoPath)) ?? {
        kind: "workstreams",
      }
    );
  } catch {
    return { kind: "workstreams" };
  }
}

export function persistView(repoPath: string, view: SnapshotView): void {
  try {
    if (view.kind === "workstreams") {
      localStorage.removeItem(VIEW_STORE_PREFIX + repoPath);
    } else {
      localStorage.setItem(VIEW_STORE_PREFIX + repoPath, JSON.stringify(view));
    }
  } catch {
    // Storage full or unavailable only loses the stickiness.
  }
}
