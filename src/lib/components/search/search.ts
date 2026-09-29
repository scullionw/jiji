// Pure shaping for the history search overlay: how one backend result
// renders as rows and status text. No Svelte.

import type { HistorySearch } from "$lib/bindings/HistorySearch";
import type { SearchFileHit } from "$lib/bindings/SearchFileHit";

/// A query long enough to search. One character matches half of history
/// and costs a full walk; two is where results start meaning something.
export const MIN_QUERY_LENGTH = 2;

/// One file hit's compact label: the occurrence delta the change caused
/// ("+2" introduced two, "−1" removed one), or "name" when only the file's
/// path matched the query.
export function fileHitLabel(hit: SearchFileHit): {
  text: string;
  tone: "add" | "del" | "name";
} {
  if (hit.added > hit.removed)
    return { text: `+${hit.added - hit.removed}`, tone: "add" };
  if (hit.removed > hit.added)
    return { text: `−${hit.removed - hit.added}`, tone: "del" };
  return { text: "name", tone: "name" };
}

/// The footer line under a finished search: how much matched, how far the
/// walk got, and whether it stopped early.
export function searchStatus(result: HistorySearch): string {
  const count = result.hits.length;
  const matches =
    count === 1 ? "1 matching change" : `${count} matching changes`;
  const scanned = `searched ${result.scanned} change${
    result.scanned === 1 ? "" : "s"
  }`;
  const capped = result.truncated
    ? " · stopped early, older history unsearched"
    : "";
  return `${matches} · ${scanned}${capped}`;
}
