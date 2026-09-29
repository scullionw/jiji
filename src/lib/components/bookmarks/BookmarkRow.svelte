<script lang="ts">
  // One bookmark in the manager: identity and target on the left, state
  // chips in the middle, hover actions on the right. Destructive actions
  // confirm inline — the row itself states the consequence, no dialog.

  import type { BookmarkState } from "$lib/bindings/BookmarkState";
  import { SYNC_GLYPH } from "$lib/components/graph/graph";
  import { shortAge } from "$lib/time";

  let {
    bookmark,
    canJump,
    busy,
    onjump,
    onpush,
    ontrack,
    onuntrack,
    ondelete,
    ondeleteremote,
  }: {
    bookmark: BookmarkState;
    /** The target is drawn in the current view, so the row can jump. */
    canJump: boolean;
    busy: boolean;
    onjump: () => void;
    onpush?: () => void;
    ontrack?: () => void;
    onuntrack?: () => void;
    ondelete?: () => void;
    ondeleteremote?: () => void;
  } = $props();

  const sync = $derived(SYNC_GLYPH[bookmark.sync]);
  // Which destructive action is awaiting its confirm click.
  let confirming = $state<"delete" | "deleteRemote" | null>(null);

  function confirmLabel(): string {
    if (confirming === "delete") return `Delete ${bookmark.name}?`;
    const where = bookmark.remote ?? "the remote";
    return bookmark.isLocal
      ? `Delete ${bookmark.name} here and on ${where}?`
      : `Push a deletion of ${bookmark.name} to ${where}?`;
  }
</script>

<div
  class="row"
  class:busy
  data-bookmark={bookmark.name}
  data-bookmark-remote={bookmark.remote ?? ""}
>
  <button
    class="ident"
    disabled={!canJump}
    title={canJump
      ? "Show in the workbench"
      : "Not drawn in the current view — switch the workbench view to see it"}
    onclick={onjump}
  >
    <span class="name-wrap">
      <span class="name truncate">
        <span class="mono">{bookmark.name}</span>
        {#if bookmark.isTrunk}<span class="chip trunk">trunk</span>{/if}
        {#if sync.glyph}<i class="sync {sync.tone}" title={sync.label}>{sync.glyph}</i>{/if}
        {#if !bookmark.isLocal && bookmark.tracked}
          <span class="chip note" title="jj tracks this remote branch">tracked</span>
        {/if}
        {#if bookmark.isMerged}
          <span class="chip merged" title="Already ancestry of trunk — deleting is cleanup">
            merged
          </span>
        {/if}
      </span>
      <span class="target truncate">
        <span class="mono id">{bookmark.target.slice(0, 8)}</span>
        {bookmark.targetDescription || "(no description)"}
      </span>
    </span>
  </button>

  {#if confirming}
    <span class="confirm">
      <span class="confirm-text truncate">{confirmLabel()}</span>
      <button
        class="act danger solid"
        disabled={busy}
        onclick={() => {
          const action = confirming === "delete" ? ondelete : ondeleteremote;
          confirming = null;
          action?.();
        }}
      >
        Delete
      </button>
      <button class="act" onclick={() => (confirming = null)}>Cancel</button>
    </span>
  {:else}
    <span class="actions">
      {#if onpush}
        <button class="act" disabled={busy} onclick={onpush}>Push</button>
      {/if}
      {#if ontrack}
        <button class="act" disabled={busy} onclick={ontrack}>Track</button>
      {/if}
      {#if onuntrack}
        <button class="act" disabled={busy} onclick={onuntrack}>Untrack</button>
      {/if}
      {#if ondelete}
        <button
          class="act danger"
          disabled={busy}
          onclick={() => (confirming = "delete")}
        >
          Delete
        </button>
      {/if}
      {#if ondeleteremote}
        <button
          class="act danger"
          disabled={busy}
          title={bookmark.isLocal
            ? "Delete here and push the deletion to the remote"
            : "Push a deletion of this branch to the remote"}
          onclick={() => (confirming = "deleteRemote")}
        >
          {bookmark.isLocal ? "Delete + remote" : "Delete on remote"}
        </button>
      {/if}
    </span>
    <span class="meta" title="{bookmark.targetAuthor} · {bookmark.targetTimestamp}">
      <span class="author truncate">{bookmark.targetAuthor}</span>
      <span class="age">
        {bookmark.targetTimestamp ? shortAge(bookmark.targetTimestamp) : ""}
      </span>
    </span>
  {/if}
</div>

<style>
  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-height: 40px;
    padding: 3px var(--sp-2);
    border-bottom: 1px solid var(--clr-border-2);
    transition: background var(--t-fast) var(--ease-out);
  }

  .row:hover {
    background: var(--clr-bg-hover);
  }

  .row.busy {
    opacity: 0.7;
  }

  .ident {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    text-align: left;
  }

  .ident:disabled {
    cursor: default;
  }

  .name-wrap {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  .name {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--text-s);
    color: var(--clr-text-1);
  }

  .name .mono {
    font-weight: 500;
  }

  .ident:not(:disabled):hover .name .mono {
    color: var(--clr-accent-strong);
  }

  .target {
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .target .id {
    color: var(--clr-text-2);
    margin-right: 4px;
  }

  .chip {
    flex-shrink: 0;
    height: 16px;
    padding: 0 5px;
    font-size: var(--text-xs);
    font-weight: 500;
    line-height: 16px;
    border-radius: var(--radius-s);
  }

  .chip.trunk {
    background: var(--clr-accent-dim);
    color: var(--clr-accent-strong);
  }

  .chip.note {
    background: color-mix(in srgb, var(--clr-text-1) 6%, transparent);
    color: var(--clr-text-3);
  }

  .chip.merged {
    background: var(--clr-ok-dim);
    color: var(--clr-ok);
  }

  .sync {
    font-style: normal;
    font-size: var(--text-xs);
  }

  .sync.ok {
    color: var(--clr-ok);
  }

  .sync.warn {
    color: var(--clr-warn);
  }

  .sync.danger {
    color: var(--clr-danger);
  }

  /* Actions surface on hover, like graph-row affordances. */
  .actions {
    flex-shrink: 0;
    display: none;
    gap: var(--sp-1);
  }

  .row:hover .actions {
    display: inline-flex;
  }

  .act {
    height: 22px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-s);
    font-size: var(--text-xs);
    font-weight: 500;
    color: var(--clr-text-1);
    background: var(--clr-bg-3);
    border: 1px solid var(--clr-border-1);
    transition:
      background var(--t-fast) var(--ease-out),
      color var(--t-fast) var(--ease-out);
  }

  .act:hover:not(:disabled) {
    background: color-mix(in srgb, var(--clr-bg-3) 92%, var(--clr-text-1));
  }

  .act.danger {
    color: var(--clr-danger);
  }

  .act.danger:hover:not(:disabled) {
    background: var(--clr-danger-dim);
  }

  .act.danger.solid {
    background: var(--clr-danger);
    border-color: transparent;
    color: #ffffff;
  }

  .act:disabled {
    opacity: 0.5;
  }

  .confirm {
    flex-shrink: 0;
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    max-width: 60%;
  }

  .confirm-text {
    font-size: var(--text-xs);
    color: var(--clr-text-2);
  }

  .meta {
    flex-shrink: 0;
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    width: 110px;
    justify-content: flex-end;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .author {
    max-width: 80px;
  }

  /* Meta yields to the action strip on hover so the row never jumps. */
  .row:hover .meta {
    display: none;
  }
</style>
