// The diff currently on screen, published for the context menu: resolving
// a right-click inside the diff pane needs the clicked file's hunks (patch
// copy, line lookup) without re-fetching anything. DiffView owns writes.

import type { ChangeDiff } from "$lib/bindings/ChangeDiff";

// A class so the field can be raw, like DiffView's own copy: diffs are
// immutable once fetched, and nothing gains from deep-proxying them.
class CurrentDiff {
  diff = $state.raw<ChangeDiff | null>(null);
}

export const currentDiff = new CurrentDiff();
