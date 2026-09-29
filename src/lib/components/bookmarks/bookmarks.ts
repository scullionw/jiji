// Shapes RepoSnapshot.bookmarks into the bookmarks manager's model: a
// Local group plus one group per remote (remote-only refs — teammates'
// work and leftovers), searchable, cleanup candidates surfaced. Pure data —
// no Svelte.

import type { BookmarkState } from "$lib/bindings/BookmarkState";

export interface BookmarkGroups {
  local: BookmarkState[];
  remotes: { remote: string; bookmarks: BookmarkState[] }[];
}

/** Well-known remotes first, the rest alphabetical — the same preference
 * order the backend uses to pick a local bookmark's tracked remote. */
function remoteRank(remote: string): number {
  if (remote === "origin") return 0;
  if (remote === "upstream") return 1;
  return 2;
}

function matches(bookmark: BookmarkState, tokens: string[]): boolean {
  const haystack = [
    bookmark.name,
    bookmark.remote ?? "",
    bookmark.targetDescription,
    bookmark.targetAuthor,
    bookmark.target,
  ]
    .join(" ")
    .toLowerCase();
  return tokens.every((token) => haystack.includes(token));
}

function byRecency(a: BookmarkState, b: BookmarkState): number {
  // Trunk pins first; the rest read newest-first, like the graph. Missing
  // timestamps sink to the bottom.
  if (a.isTrunk !== b.isTrunk) return Number(b.isTrunk) - Number(a.isTrunk);
  const at = Date.parse(a.targetTimestamp) || 0;
  const bt = Date.parse(b.targetTimestamp) || 0;
  if (at !== bt) return bt - at;
  return a.name.localeCompare(b.name);
}

/** The manager's groups for one snapshot, filtered by the search query. */
export function groupBookmarks(
  bookmarks: BookmarkState[],
  query: string,
): BookmarkGroups {
  const tokens = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const shown =
    tokens.length === 0
      ? bookmarks
      : bookmarks.filter((b) => matches(b, tokens));

  const local = shown.filter((b) => b.isLocal).sort(byRecency);

  const byRemote = new Map<string, BookmarkState[]>();
  for (const bookmark of shown) {
    if (bookmark.isLocal || !bookmark.remote) continue;
    const list = byRemote.get(bookmark.remote) ?? [];
    list.push(bookmark);
    byRemote.set(bookmark.remote, list);
  }
  const remotes = [...byRemote.entries()]
    .sort(
      ([a], [b]) =>
        remoteRank(a) - remoteRank(b) || a.localeCompare(b),
    )
    .map(([remote, list]) => ({ remote, bookmarks: list.sort(byRecency) }));

  return { local, remotes };
}

/** Cleanup candidates: every entry whose target is already trunk ancestry.
 * Locals first (deleting them is the common sweep), then remote-only refs. */
export function mergedBookmarks(bookmarks: BookmarkState[]): BookmarkState[] {
  return bookmarks
    .filter((b) => b.isMerged)
    .sort(
      (a, b) => Number(b.isLocal) - Number(a.isLocal) || byRecency(a, b),
    );
}

/** A stable render key: local and remote-only entries can share a name. */
export function bookmarkKey(bookmark: BookmarkState): string {
  return bookmark.isLocal
    ? `local:${bookmark.name}`
    : `${bookmark.remote}:${bookmark.name}`;
}
