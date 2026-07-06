<script lang="ts">
  import { tick } from "svelte";
  import Icon from "$lib/components/ui/Icon.svelte";
  import type { BookmarkState } from "$lib/bindings/BookmarkState";
  import { errorMessage } from "$lib/api";
  import { stackPosition, SYNC_LABEL } from "$lib/components/inspector/inspect";
  import { resolveFocusedWorkstream } from "$lib/components/graph/graph";
  import { deleteBookmark, renameBookmark } from "$lib/state/actions";
  import { app } from "$lib/state/app.svelte";

  // Parent renders this overview only when a snapshot exists and nothing
  // is selected; a selection puts the diff surface in this pane instead.
  const snapshot = $derived(app.snapshot!);
  const focused = $derived(
    resolveFocusedWorkstream(snapshot, app.focusedWorkstreamId),
  );
  const workspace = $derived(
    snapshot.workspaces.find((w) => w.isCurrent) ?? snapshot.workspaces[0],
  );
  const trunk = $derived(snapshot.bookmarks.find((b) => b.isTrunk));

  // Jumping to a workstream selects its head, which can be in another
  // lane; refocus so the selection is actually visible in the graph.
  function jumpTo(id: string) {
    const owner = stackPosition(snapshot, id)?.workstream;
    if (owner && owner.id !== focused?.id) {
      app.focusedWorkstreamId = owner.id;
    }
    app.selectedNodeId = id;
  }

  const hasNode = (id: string) => snapshot.nodes.some((n) => n.id === id);

  // Inline bookmark management on the list: rename swaps the row for an
  // input, delete for a one-line confirm stating what happens remotely.
  // One row edits at a time; errors render under the list.
  let bmEdit = $state<{ name: string; mode: "rename" | "delete" } | null>(null);
  let bmDraft = $state("");
  let bmError = $state<string | null>(null);
  let bmBusy = $state(false);
  let bmInput = $state<HTMLInputElement | undefined>();

  function startRename(bookmark: BookmarkState) {
    bmEdit = { name: bookmark.name, mode: "rename" };
    bmDraft = bookmark.name;
    bmError = null;
    tick().then(() => bmInput?.focus());
  }

  function startDelete(bookmark: BookmarkState) {
    bmEdit = { name: bookmark.name, mode: "delete" };
    bmError = null;
  }

  function cancelEdit() {
    bmEdit = null;
    bmError = null;
  }

  async function runEdit(action: () => Promise<unknown>) {
    if (bmBusy) return;
    bmBusy = true;
    bmError = null;
    try {
      await action();
      bmEdit = null;
    } catch (error) {
      bmError = errorMessage(error);
    } finally {
      bmBusy = false;
    }
  }

  function saveRename() {
    const edit = bmEdit;
    const next = bmDraft.trim();
    if (!edit || !next || next === edit.name) return;
    runEdit(() => renameBookmark(edit.name, next));
  }

  function confirmDelete() {
    const edit = bmEdit;
    if (!edit) return;
    runEdit(() => deleteBookmark(edit.name));
  }

  function onEditKeydown(event: KeyboardEvent) {
    if (event.key === "Enter") {
      event.preventDefault();
      saveRename();
    } else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      cancelEdit();
    }
  }
</script>

<div class="inspector">
  <!-- The repo's identity card: name, path, and the standing facts that
       used to be a label/value table. One glance, no dead rows. -->
  <header class="repo-head">
    <span class="repo-mark"><Icon name="folder" size={15} /></span>
    <div class="repo-id">
      <h3 class="truncate">{snapshot.repoName}</h3>
      <span
        class="repo-path mono selectable truncate"
        title={snapshot.repoPath}
      >
        {snapshot.repoPath}
      </span>
    </div>
    <div class="facts">
      {#if trunk}
        <span class="fact" title="Trunk target">
          <Icon name="branch" size={11} />
          <span class="mono">
            {trunk.name}{trunk.remote ? `@${trunk.remote}` : ""}
          </span>
        </span>
      {/if}
      {#if workspace}
        <span class="fact" title="Current workspace">
          <Icon name="workspaces" size={11} />
          {workspace.name}
        </span>
      {/if}
      <span class="fact" title="Backend">
        <span class="mono">{snapshot.backend}</span>
      </span>
    </div>
  </header>

  <div class="cards">
    {#if snapshot.workstreams.length > 0}
      <section class="card">
        <h4>
          Workstreams
          <span class="count mono">{snapshot.workstreams.length}</span>
        </h4>
        <div class="ws-list">
          {#each snapshot.workstreams as ws (ws.id)}
            <button
              class="ws-row"
              class:current={ws.id === focused?.id}
              onclick={() => ws.nodeIds.length > 0 && jumpTo(ws.nodeIds[0])}
            >
              <i class="ws-dot" class:active={ws.isActive}></i>
              <span class="ws-title truncate">{ws.title}</span>
              {#if ws.bookmark}
                <span class="chip truncate">
                  <Icon name="bookmark" size={10} />
                  {ws.bookmark}
                </span>
              {/if}
              <span class="ws-meta">
                {ws.nodeIds.length}{#if ws.behindTrunk > 0}<span
                    class="ws-behind"
                  >
                    ↓{ws.behindTrunk}</span
                  >{/if}
              </span>
            </button>
          {/each}
        </div>
      </section>
    {/if}

    <section class="card">
      <h4>
        Bookmarks
        <span class="count mono">{snapshot.bookmarks.length}</span>
      </h4>
      {#each snapshot.bookmarks as bookmark (bookmark.name)}
        {@const sync = SYNC_LABEL[bookmark.sync] ?? {
          text: bookmark.sync,
          tone: "muted",
        }}
        {#if bmEdit?.name === bookmark.name && bmEdit.mode === "rename"}
          <div class="row bm-editing">
            <input
              class="bm-input mono"
              bind:this={bmInput}
              bind:value={bmDraft}
              spellcheck="false"
              disabled={bmBusy}
              onkeydown={onEditKeydown}
            />
            <button
              class="bm-go"
              onclick={saveRename}
              disabled={bmBusy || !bmDraft.trim() || bmDraft.trim() === bookmark.name}
            >
              Rename
            </button>
            <button class="bm-cancel" onclick={cancelEdit} disabled={bmBusy}>Cancel</button>
          </div>
        {:else if bmEdit?.name === bookmark.name && bmEdit.mode === "delete"}
          <div class="row bm-editing">
            <span class="bm-question truncate">
              Delete {bookmark.name}?
              {bookmark.remote
                ? `Removed from ${bookmark.remote} on the next push.`
                : "It only exists locally."}
            </span>
            <button class="bm-go danger" onclick={confirmDelete} disabled={bmBusy}>
              Delete
            </button>
            <button class="bm-cancel" onclick={cancelEdit} disabled={bmBusy}>Cancel</button>
          </div>
        {:else}
          <div class="row bm-row">
            {#if hasNode(bookmark.target)}
              <button
                class="bm-name"
                title="Go to {bookmark.target}"
                onclick={() => jumpTo(bookmark.target)}
              >
                <Icon name="bookmark" size={11} />
                <span class="value truncate">{bookmark.name}</span>
              </button>
            {:else}
              <span class="bm-name">
                <Icon name="bookmark" size={11} />
                <span class="value truncate">{bookmark.name}</span>
              </span>
            {/if}
            <span class="sync {sync.tone}">{sync.text}</span>
            {#if bookmark.isLocal && !bookmark.isTrunk}
              <span class="bm-actions">
                <button
                  class="bm-icon"
                  title="Rename {bookmark.name}"
                  onclick={() => startRename(bookmark)}
                >
                  <Icon name="edit" size={11} />
                </button>
                <button
                  class="bm-icon danger"
                  title="Delete {bookmark.name}"
                  onclick={() => startDelete(bookmark)}
                >
                  <Icon name="trash" size={11} />
                </button>
              </span>
            {/if}
          </div>
        {/if}
      {/each}
      {#if bmError}
        <p class="bm-error" title={bmError}>{bmError}</p>
      {/if}
    </section>
  </div>

  <p class="select-hint">
    <span class="kbd">↑</span><span class="kbd">↓</span>
    Select a change in the graph to review its diff
  </p>
</div>

<style>
  /* The repo overview lives in the wide pane only while nothing is
     selected; cap the column so it reads well at full width. */
  .inspector {
    padding: var(--sp-5) var(--sp-6) var(--sp-6);
    max-width: 880px;
    margin-inline: auto;
    min-height: 100%;
    display: flex;
    flex-direction: column;
  }

  .repo-head {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    margin-bottom: var(--sp-5);
    min-width: 0;
  }

  .repo-mark {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    border-radius: var(--radius-m);
    background: var(--clr-accent-dim);
    border: 1px solid color-mix(in srgb, var(--clr-accent) 22%, transparent);
    color: var(--clr-accent-strong);
  }

  .repo-id {
    min-width: 0;
    flex: 1;
  }

  .repo-id h3 {
    font-size: var(--text-l);
    font-weight: 650;
    letter-spacing: -0.01em;
    color: var(--clr-text-1);
  }

  .repo-path {
    display: block;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .facts {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: var(--sp-1);
    flex-shrink: 0;
  }

  .fact {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 22px;
    padding: 0 var(--sp-2);
    font-size: var(--text-xs);
    border-radius: 999px;
    background: var(--clr-bg-2);
    border: 1px solid var(--clr-border-2);
    color: var(--clr-text-2);
  }

  .fact :global(svg) {
    color: var(--clr-text-3);
  }

  /* Cards side by side when the pane is wide, stacked when the diff
     split squeezes it. */
  .cards {
    display: grid;
    grid-template-columns: 1fr;
    gap: var(--sp-4);
    align-items: start;
  }

  @container detail-pane (min-width: 780px) {
    .cards {
      grid-template-columns: 1fr 1fr;
    }
  }

  .card {
    background: var(--clr-bg-2);
    border: 1px solid var(--clr-border-2);
    border-radius: var(--radius-l);
    box-shadow: var(--shadow-1);
    padding: var(--sp-3) var(--sp-4) var(--sp-4);
    min-width: 0;
  }

  h4 {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-xs);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: var(--clr-text-3);
    padding-bottom: var(--sp-2);
    margin-bottom: var(--sp-2);
    border-bottom: 1px solid var(--clr-border-2);
  }

  .count {
    font-size: var(--text-xs);
    padding: 0 6px;
    border-radius: 999px;
    background: var(--clr-bg-3);
    border: 1px solid var(--clr-border-2);
    color: var(--clr-text-2);
  }

  .ws-list {
    display: flex;
    flex-direction: column;
  }

  .ws-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    min-width: 0;
    text-align: left;
    padding: 5px var(--sp-2);
    margin: 0 calc(-1 * var(--sp-2));
    border-radius: var(--radius-m);
    transition: background var(--t-fast) var(--ease-out);
  }

  .ws-row:hover {
    background: var(--clr-bg-hover);
  }

  .ws-row.current {
    background: color-mix(in srgb, var(--clr-accent) 8%, transparent);
  }

  .ws-dot {
    flex-shrink: 0;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    border: 1.5px solid color-mix(in srgb, var(--clr-accent) 45%, var(--clr-text-3));
  }

  .ws-dot.active {
    border-color: var(--clr-working-copy);
    background: var(--clr-working-copy);
    box-shadow: 0 0 6px color-mix(in srgb, var(--clr-working-copy) 50%, transparent);
  }

  .ws-title {
    font-size: var(--text-m);
    font-weight: 500;
    color: var(--clr-text-1);
    min-width: 0;
    flex: 1;
  }

  .ws-behind {
    color: var(--clr-warn);
  }

  .chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
    max-width: 11em;
    font-size: var(--text-xs);
    border-radius: 999px;
    padding: 1px 8px;
    background: var(--clr-bg-3);
    color: var(--clr-text-2);
    border: 1px solid var(--clr-border-2);
  }

  .ws-meta {
    flex-shrink: 0;
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: 4px 0;
    font-size: var(--text-m);
    min-width: 0;
  }

  .row.bm-row + .row.bm-row {
    border-top: 1px solid
      color-mix(in srgb, var(--clr-border-2) 55%, transparent);
  }

  .value {
    color: var(--clr-text-1);
    min-width: 0;
  }

  .mono {
    font-size: var(--text-s);
  }

  .bm-name {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--clr-text-3);
    flex: 1;
    min-width: 0;
    text-align: left;
  }

  button.bm-name:hover .value {
    color: var(--clr-accent-strong);
  }

  /* Rename/delete reveal on row hover, like quiet contextual controls. */
  .bm-actions {
    display: inline-flex;
    gap: 2px;
    flex-shrink: 0;
    opacity: 0;
    transition: opacity var(--t-fast) var(--ease-out);
  }

  .bm-row:hover .bm-actions,
  .bm-actions:focus-within {
    opacity: 1;
  }

  .bm-icon {
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    border-radius: var(--radius-s);
    color: var(--clr-text-3);
    transition: all var(--t-fast) var(--ease-out);
  }

  .bm-icon:hover {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .bm-icon.danger:hover {
    background: color-mix(in srgb, var(--clr-danger) 12%, transparent);
    color: var(--clr-danger);
  }

  .bm-input {
    flex: 1;
    min-width: 0;
    padding: 1px var(--sp-2);
    font-size: var(--text-s);
    color: var(--clr-text-1);
    background: var(--clr-bg-2);
    border: 1px solid var(--clr-border-1);
    border-radius: 999px;
  }

  .bm-input:focus {
    outline: none;
    border-color: var(--clr-accent-strong);
  }

  .bm-input:disabled {
    opacity: 0.6;
  }

  .bm-question {
    flex: 1;
    min-width: 0;
    font-size: var(--text-s);
    color: var(--clr-text-2);
  }

  .bm-go,
  .bm-cancel {
    flex-shrink: 0;
    font-size: var(--text-xs);
    font-weight: 500;
    border-radius: 999px;
    padding: 1px 10px;
    transition: all var(--t-fast) var(--ease-out);
  }

  .bm-go {
    color: var(--clr-accent-strong);
    background: var(--clr-accent-dim);
  }

  .bm-go:hover:not(:disabled) {
    background: color-mix(in srgb, var(--clr-accent-strong) 24%, transparent);
  }

  .bm-go.danger {
    color: var(--clr-danger);
    background: color-mix(in srgb, var(--clr-danger) 14%, transparent);
  }

  .bm-go.danger:hover:not(:disabled) {
    background: color-mix(in srgb, var(--clr-danger) 24%, transparent);
  }

  .bm-cancel {
    color: var(--clr-text-3);
    border: 1px solid var(--clr-border-2);
  }

  .bm-cancel:hover:not(:disabled) {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .bm-go:disabled,
  .bm-cancel:disabled {
    cursor: default;
    opacity: 0.6;
  }

  .bm-error {
    margin-top: var(--sp-1);
    font-size: var(--text-xs);
    color: var(--clr-danger);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .sync {
    font-size: var(--text-xs);
    border-radius: 999px;
    padding: 1px 8px;
    border: 1px solid var(--clr-border-2);
    color: var(--clr-text-3);
  }

  .sync.ok {
    color: var(--clr-ok);
    border-color: color-mix(in srgb, var(--clr-ok) 35%, transparent);
  }

  .sync.warn {
    color: var(--clr-warn);
    border-color: color-mix(in srgb, var(--clr-warn) 35%, transparent);
  }

  .sync.danger {
    color: var(--clr-danger);
    border-color: color-mix(in srgb, var(--clr-danger) 35%, transparent);
  }

  .select-hint {
    margin-top: auto;
    padding-top: var(--sp-6);
    display: flex;
    align-items: center;
    justify-content: center;
    gap: var(--sp-2);
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .select-hint .kbd {
    padding: 0 4px;
  }

  .select-hint .kbd + .kbd {
    margin-left: -4px;
  }
</style>
