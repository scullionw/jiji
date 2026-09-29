<script lang="ts">
  // The bookmarks manager: every bookmark the repo knows — local work and
  // each remote's branches (teammates' work included) — searchable, with
  // track/push/delete actions and a merged-work cleanup sweep. The
  // GitKraken lesson: seeing remote refs beside local ones is how a team
  // knows what exists and what to throw away.

  import type { BookmarkState } from "$lib/bindings/BookmarkState";
  import EmptyState from "$lib/components/ui/EmptyState.svelte";
  import Icon from "$lib/components/ui/Icon.svelte";
  import SectionHeader from "$lib/components/ui/SectionHeader.svelte";
  import * as api from "$lib/api";
  import { app } from "$lib/state/app.svelte";
  import {
    deleteBookmark,
    deleteRemoteBookmark,
    jumpToChange,
    pushBookmark,
    trackBookmark,
    untrackBookmark,
  } from "$lib/state/actions";
  import { fetchUpstreamNow, upstream } from "$lib/state/upstream.svelte";
  import { fromNow } from "$lib/time";
  import BookmarkRow from "./BookmarkRow.svelte";
  import { bookmarkKey, groupBookmarks, mergedBookmarks } from "./bookmarks";

  const snapshot = $derived(app.snapshot!);
  let query = $state("");
  const groups = $derived(groupBookmarks(snapshot.bookmarks, query));
  const merged = $derived(mergedBookmarks(snapshot.bookmarks));
  const drawnIds = $derived(new Set(snapshot.nodes.map((n) => n.id)));
  const hasRemotes = $derived(snapshot.gitRemotes.length > 0);
  const empty = $derived(snapshot.bookmarks.length === 0);
  const noMatch = $derived(
    !empty && groups.local.length === 0 && groups.remotes.length === 0,
  );

  let busy = $state(false);
  let error = $state<string | null>(null);

  // Every row action shares this shape: one at a time, errors surface in
  // the view's banner (rows have no room for a story), success clears it.
  async function run(action: () => Promise<unknown>): Promise<void> {
    if (busy) return;
    busy = true;
    error = null;
    try {
      await action();
    } catch (err) {
      error = api.errorMessage(err);
    } finally {
      busy = false;
    }
  }

  // ── Cleanup sweep ──
  let cleanupOpen = $state(false);
  let checked = $state<Set<string>>(new Set());
  let alsoRemote = $state(false);

  function openCleanup() {
    checked = new Set(merged.map(bookmarkKey));
    alsoRemote = false;
    cleanupOpen = true;
  }

  function toggle(key: string) {
    const next = new Set(checked);
    if (!next.delete(key)) next.add(key);
    checked = next;
  }

  const checkedCount = $derived(
    merged.filter((b) => checked.has(bookmarkKey(b))).length,
  );

  async function runCleanup() {
    const targets = merged.filter((b) => checked.has(bookmarkKey(b)));
    await run(async () => {
      for (const bookmark of targets) {
        if (!bookmark.isLocal) {
          await deleteRemoteBookmark(bookmark.name, bookmark.remote!);
        } else if (alsoRemote && bookmark.tracked && bookmark.remote) {
          await deleteRemoteBookmark(bookmark.name, bookmark.remote);
        } else {
          await deleteBookmark(bookmark.name);
        }
      }
    });
    if (!error) cleanupOpen = false;
  }

  function rowActions(bookmark: BookmarkState) {
    if (!bookmark.isLocal) {
      return {
        ontrack: bookmark.tracked
          ? undefined
          : () => void run(() => trackBookmark(bookmark.name, bookmark.remote!)),
        ondeleteremote: () =>
          void run(() => deleteRemoteBookmark(bookmark.name, bookmark.remote!)),
      };
    }
    if (bookmark.isTrunk) {
      return {
        onpush: hasRemotes
          ? () =>
              void run(() =>
                pushBookmark(bookmark.name, bookmark.remote ?? undefined),
              )
          : undefined,
      };
    }
    return {
      onpush: hasRemotes
        ? () =>
            void run(() =>
              pushBookmark(bookmark.name, bookmark.remote ?? undefined),
            )
        : undefined,
      onuntrack:
        bookmark.tracked && bookmark.remote
          ? () =>
              void run(() => untrackBookmark(bookmark.name, bookmark.remote!))
          : undefined,
      ondelete: () => void run(() => deleteBookmark(bookmark.name)),
      ondeleteremote:
        bookmark.tracked && bookmark.remote
          ? () =>
              void run(() =>
                deleteRemoteBookmark(bookmark.name, bookmark.remote!),
              )
          : undefined,
    };
  }
</script>

{#if empty}
  <EmptyState
    icon="bookmark"
    title="No bookmarks"
    body="Bookmarks are jj's branch names — yours locally, and every branch on the remotes after a fetch. Create one from a change's header, or fetch to see what the team has pushed."
    hint="jj bookmark create"
  />
{:else}
  <div class="view">
    <div class="column">
      <SectionHeader
        icon="bookmark"
        title="Bookmarks"
        description="Every bookmark this repo knows: your local ones and each remote's branches — what the team is working on, and what is ready to be cleaned up."
      />

      <div class="toolbar">
        <label class="search">
          <Icon name="search" size={13} />
          <input
            type="text"
            placeholder="Filter by name, author, or description"
            bind:value={query}
          />
        </label>
        {#if hasRemotes}
          <button
            class="btn fetch"
            disabled={upstream.checking}
            title="jj git fetch — refresh what the remotes have"
            onclick={() => void fetchUpstreamNow()}
          >
            <Icon name="refresh" size={12} />
            {upstream.checking
              ? "Fetching…"
              : upstream.lastChecked
                ? `Fetched ${fromNow(upstream.lastChecked)}`
                : "Fetch now"}
          </button>
        {/if}
        {#if merged.length > 0}
          <button class="btn cleanup-btn" disabled={busy} onclick={openCleanup}>
            <Icon name="trash" size={12} />
            Clean up merged ({merged.length})
          </button>
        {/if}
      </div>

      {#if error}
        <div class="error">
          <span class="truncate" title={error}>{error}</span>
          <button onclick={() => (error = null)} aria-label="Dismiss">
            <Icon name="close" size={12} />
          </button>
        </div>
      {/if}

      {#if cleanupOpen}
        <!-- The consequence-stating card: exactly what a sweep will delete,
             opt-out per row, remote deletion opt-in. -->
        <section class="cleanup">
          <p class="cleanup-title">
            These bookmarks point at work that is already part of
            {snapshot.trunkBookmark || "trunk"} — deleting them loses nothing.
          </p>
          {#each merged as bookmark (bookmarkKey(bookmark))}
            <label class="cleanup-row">
              <input
                type="checkbox"
                checked={checked.has(bookmarkKey(bookmark))}
                onchange={() => toggle(bookmarkKey(bookmark))}
              />
              <span class="mono cleanup-name truncate">
                {bookmark.isLocal
                  ? bookmark.name
                  : `${bookmark.name}@${bookmark.remote}`}
              </span>
              <span class="cleanup-kind">
                {bookmark.isLocal
                  ? bookmark.tracked
                    ? "local, tracked"
                    : "local only"
                  : "remote only — deletes on the remote"}
              </span>
            </label>
          {/each}
          {#if merged.some((b) => b.isLocal && b.tracked)}
            <label class="cleanup-row also">
              <input type="checkbox" bind:checked={alsoRemote} />
              <span>Also delete tracked ones on their remote</span>
            </label>
          {/if}
          <div class="cleanup-actions">
            <button
              class="btn danger solid confirm-delete"
              disabled={busy || checkedCount === 0}
              onclick={() => void runCleanup()}
            >
              Delete {checkedCount} bookmark{checkedCount === 1 ? "" : "s"}
            </button>
            <button class="btn cancel" onclick={() => (cleanupOpen = false)}>
              Cancel
            </button>
          </div>
        </section>
      {/if}

      {#if noMatch}
        <p class="no-match">Nothing matches “{query}”.</p>
      {/if}

      {#if groups.local.length > 0}
        <section class="group">
          <div class="group-head">
            <span class="group-label">Local</span>
            <span class="group-count mono">{groups.local.length}</span>
          </div>
          {#each groups.local as bookmark (bookmarkKey(bookmark))}
            <BookmarkRow
              {bookmark}
              {busy}
              canJump={drawnIds.has(bookmark.target)}
              onjump={() => jumpToChange(bookmark.target)}
              {...rowActions(bookmark)}
            />
          {/each}
        </section>
      {/if}

      {#each groups.remotes as group (group.remote)}
        <section class="group">
          <div class="group-head">
            <span class="group-label">{group.remote}</span>
            <span class="group-count mono">{group.bookmarks.length}</span>
          </div>
          <p class="group-blurb">
            Branches on {group.remote} with no local bookmark — fetched, not
            tracked. Track one to work with it; jump via the Team view.
          </p>
          {#each group.bookmarks as bookmark (bookmarkKey(bookmark))}
            <BookmarkRow
              {bookmark}
              {busy}
              canJump={drawnIds.has(bookmark.target)}
              onjump={() => jumpToChange(bookmark.target)}
              {...rowActions(bookmark)}
            />
          {/each}
        </section>
      {/each}
    </div>
  </div>
{/if}

<style>
  .view {
    height: 100%;
    overflow-y: auto;
  }

  .column {
    max-width: 800px;
    margin-inline: auto;
    padding: var(--sp-6) var(--sp-6) var(--sp-8);
  }

  .toolbar {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin-bottom: var(--sp-4);
  }

  .search {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: 28px;
    padding: 0 var(--sp-2);
    background: var(--clr-bg-well);
    border: 1px solid var(--clr-border-1);
    border-radius: var(--radius-m);
    color: var(--clr-text-3);
    transition: border-color var(--t-fast) var(--ease-out);
  }

  .search:focus-within {
    border-color: var(--clr-accent);
  }

  .search input {
    flex: 1;
    min-width: 0;
    font-size: var(--text-s);
    color: var(--clr-text-1);
    background: transparent;
    border: none;
    outline: none;
  }

  .search input::placeholder {
    color: var(--clr-text-3);
  }

  .fetch,
  .cleanup-btn {
    height: 28px;
  }

  .cleanup-btn:hover:not(:disabled) {
    color: var(--clr-danger);
  }

  .error {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin-bottom: var(--sp-3);
    padding: 6px var(--sp-3);
    font-size: var(--text-s);
    color: var(--clr-danger);
    background: color-mix(in srgb, var(--clr-danger) 8%, transparent);
    border-radius: var(--radius-m);
  }

  .error span {
    flex: 1;
    min-width: 0;
  }

  .error button {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    color: var(--clr-danger);
  }

  .cleanup {
    margin-bottom: var(--sp-5);
    padding: var(--sp-3) var(--sp-4);
    background: var(--clr-bg-well);
    border: 1px solid var(--clr-border-2);
    border-radius: var(--radius-l);
  }

  .cleanup-title {
    font-size: var(--text-s);
    color: var(--clr-text-2);
    margin-bottom: var(--sp-2);
  }

  .cleanup-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: 3px 0;
    font-size: var(--text-s);
    color: var(--clr-text-1);
    cursor: pointer;
  }

  .cleanup-row input {
    accent-color: var(--clr-accent);
  }

  .cleanup-row.also {
    margin-top: var(--sp-2);
    padding-top: var(--sp-2);
    border-top: 1px solid var(--clr-border-2);
    color: var(--clr-text-2);
  }

  .cleanup-name {
    max-width: 45%;
  }

  .cleanup-kind {
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .cleanup-actions {
    display: flex;
    justify-content: flex-end;
    flex-direction: row-reverse;
    gap: var(--sp-2);
    margin-top: var(--sp-3);
  }

  .no-match {
    font-size: var(--text-s);
    color: var(--clr-text-3);
    padding: var(--sp-4) 0;
  }

  .group {
    margin-bottom: var(--sp-5);
  }

  .group-head {
    position: sticky;
    top: 0;
    z-index: 2;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-3) var(--sp-2) var(--sp-1);
    background: var(--clr-bg-1);
    border-bottom: 1px solid var(--clr-border-2);
  }

  .group-label {
    font-size: var(--text-s);
    font-weight: 600;
    color: var(--clr-text-2);
  }

  .group-count {
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .group-blurb {
    font-size: var(--text-s);
    color: var(--clr-text-3);
    margin: var(--sp-2) 0;
    padding: 0 var(--sp-2);
    max-width: 58em;
  }
</style>
