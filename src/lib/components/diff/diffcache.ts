// In-memory cache and prefetch for the diff surface's content fetches.
//
// A diff is immutable for a given repo operation: every mutation,
// working-copy snapshot, or git import records a new operation, so entries
// keyed by (repo path, operation id, comparison) never go stale — a new operation
// simply keys fresh entries and old ones age out of the LRU. This is what
// makes revisiting a selection instant, and lets snapshot republishes that
// record no operation (view switches, converged refreshes) skip the
// backend round trip entirely.

import type { ChangeDiff } from "$lib/bindings/ChangeDiff";
import * as api from "$lib/api";

// Diffs are budgeted by the backend (line-capped hunks, a change-wide cap
// on inlined image bytes), so entries are bounded; this is comfortably
// more than a screenful of arrow-walking without hoarding an old
// operation's hunks and previews.
const MAX_ENTRIES = 16;

// Insertion order doubles as LRU order: hits re-insert.
const cache = new Map<string, ChangeDiff>();
// Single flight per key: the surface's own fetch and a neighbor prefetch
// of the same comparison share one backend call.
const inflight = new Map<string, Promise<ChangeDiff>>();

function cacheKey(repoPath: string, opId: string, from: string | null, id: string): string {
  return JSON.stringify([repoPath, opId, from, id]);
}

function store(key: string, diff: ChangeDiff): void {
  cache.delete(key);
  cache.set(key, diff);
  for (const oldest of cache.keys()) {
    if (cache.size <= MAX_ENTRIES) break;
    cache.delete(oldest);
  }
}

/// The cached diff for one comparison at one operation, if it is already
/// here — the synchronous path that makes a switch render in the same
/// frame as the click.
export function cachedDiff(
  repoPath: string,
  opId: string,
  from: string | null,
  id: string,
): ChangeDiff | undefined {
  const key = cacheKey(repoPath, opId, from, id);
  const hit = cache.get(key);
  if (hit) store(key, hit);
  return hit;
}

/// Fetch one comparison's diff through the cache. Successful responses are
/// cached even if the caller has moved on — walking back is then instant.
export function fetchDiff(
  repoPath: string,
  opId: string,
  from: string | null,
  id: string,
): Promise<ChangeDiff> {
  const key = cacheKey(repoPath, opId, from, id);
  const hit = cache.get(key);
  if (hit) {
    store(key, hit);
    return Promise.resolve(hit);
  }
  let pending = inflight.get(key);
  if (!pending) {
    pending = (from ? api.compareDiff(from, id, repoPath) : api.changeDiff(id, repoPath)).then(
      (diff) => {
        inflight.delete(key);
        store(key, diff);
        return diff;
      },
      (error) => {
        inflight.delete(key);
        throw error;
      },
    );
    inflight.set(key, pending);
  }
  return pending;
}

/// Warm the cache for a comparison the user is likely to select next.
/// Failures are dropped — the real fetch will surface them if the user
/// actually goes there.
export function prefetchDiff(repoPath: string, opId: string, from: string | null, id: string): void {
  const key = cacheKey(repoPath, opId, from, id);
  if (cache.has(key) || inflight.has(key)) return;
  fetchDiff(repoPath, opId, from, id).catch(() => {});
}
