<script lang="ts">
  import { tick } from "svelte";
  import Icon from "$lib/components/ui/Icon.svelte";
  import type { OperationItem } from "$lib/bindings/OperationItem";
  import { errorMessage } from "$lib/api";
  import { restoreOperation, revertOperation } from "$lib/state/actions";
  import { clockTime, isRootOp } from "./ops";

  type OpAction = "restore" | "revert";

  let {
    op,
    lineUp = true,
    lineDown = true,
    restoreCount = null,
    confirm = null,
    onToggle = () => {},
  }: {
    op: OperationItem;
    /** Whether the timeline rail continues above/below this row. */
    lineUp?: boolean;
    lineDown?: boolean;
    /** Operations a restore to this row would unwind (newer ones). */
    restoreCount?: number | null;
    /** Which confirm panel is open under this row; owned by the view so
     *  only one panel is open across the timeline. */
    confirm?: OpAction | null;
    onToggle?: (action: OpAction | null) => void;
  } = $props();

  const quiet = $derived(op.isSnapshot && !op.isCurrent);
  // The root operation is the state before the repo existed: nothing to
  // revert, and restoring to it would empty the repo.
  const actionable = $derived(!isRootOp(op));
  // Restoring to the current operation is a no-op; reverting it is undo.
  const canRestore = $derived(actionable && !op.isCurrent);

  let acting = $state(false);
  let error = $state<string | null>(null);
  let panelEl = $state<HTMLDivElement | undefined>();

  function toggle(action: OpAction) {
    error = null;
    onToggle(confirm === action ? null : action);
    tick().then(() => panelEl?.focus());
  }

  async function run(action: () => Promise<unknown>) {
    if (acting) return;
    acting = true;
    error = null;
    try {
      await action();
      onToggle(null);
    } catch (err) {
      error = errorMessage(err);
    } finally {
      acting = false;
    }
  }

  function runConfirmed() {
    if (confirm === "restore") run(() => restoreOperation(op.id));
    else if (confirm === "revert") run(() => revertOperation(op.id));
  }

  function onPanelKeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      onToggle(null);
    } else if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      runConfirmed();
    }
  }
</script>

<div
  class="row"
  class:current={op.isCurrent}
  class:quiet
  class:open={confirm !== null}
  data-op-id={op.id}
>
  <span class="rail" class:line-up={lineUp} class:line-down={lineDown}>
    {#if op.isCurrent}
      <span class="glyph at mono">@</span>
    {:else}
      <span class="glyph dot"></span>
    {/if}
  </span>

  <div class="body">
    <div class="head">
      <span class="desc" class:root={!op.description}>
        {op.description || "root()"}
      </span>
      {#if op.isCurrent}
        <span class="chip now">current</span>
      {/if}
      {#if actionable}
        <span class="op-actions">
          {#if canRestore}
            <button
              class="op-action"
              class:armed={confirm === "restore"}
              data-op-action="restore"
              title="Restore the repo to this point"
              onclick={() => toggle("restore")}
            >
              <Icon name="restore" size={11} />
              Restore
            </button>
          {/if}
          <button
            class="op-action"
            class:armed={confirm === "revert"}
            data-op-action="revert"
            title={op.isCurrent
              ? "Undo this operation"
              : "Back out just this operation"}
            onclick={() => toggle("revert")}
          >
            <Icon name="undo" size={11} />
            Revert
          </button>
        </span>
      {/if}
    </div>
    <div class="meta">
      <span class="mono id" title={op.id}>{op.id.slice(0, 8)}</span>
      <span class="time" title={op.timestamp}>{clockTime(op.timestamp)}</span>
      <span class="user truncate">{op.user}</span>
      {#if op.effects.length > 0}
        <span class="chips">
          {#each op.effects as effect (effect.label)}
            <span class="chip effect {effect.kind}">
              {#if effect.kind === "workingCopy"}
                <i class="wc-glyph mono">@</i>
              {:else}
                <Icon name="bookmark" size={9} />
              {/if}
              {effect.label}
            </span>
          {/each}
          {#if op.moreEffects > 0}
            <span class="chip effect overflow">+{op.moreEffects} more</span>
          {/if}
        </span>
      {/if}
    </div>

    {#if confirm === "restore"}
      <div
        class="confirm-panel"
        role="alertdialog"
        aria-label="Confirm restore"
        tabindex="-1"
        bind:this={panelEl}
        onkeydown={onPanelKeydown}
      >
        <p class="confirm-title">
          Restore the repo to this point
          <span class="confirm-context truncate">“{op.description || "root()"}”</span>
        </p>
        <ul class="consequences">
          {#if restoreCount !== null && restoreCount > 0}
            <li>
              {restoreCount} later operation{restoreCount === 1 ? "" : "s"}
              unwind{restoreCount === 1 ? "s" : ""} — the repo returns to
              exactly the state this operation left it in.
            </li>
          {:else}
            <li>
              Everything recorded after this operation is unwound — the repo
              returns to exactly the state it left.
            </li>
          {/if}
          <li>Files on disk follow the restored working copy.</li>
          <li>
            Nothing is lost: this records a new operation, and every undone
            operation stays in this log.
          </li>
        </ul>
        <div class="confirm-row">
          <span class="confirm-hint">⌘↵ to confirm</span>
          {#if error}
            <span class="confirm-error truncate" title={error}>{error}</span>
          {/if}
          <button class="confirm-cancel" onclick={() => onToggle(null)} disabled={acting}>
            Cancel
          </button>
          <button class="confirm-go" onclick={runConfirmed} disabled={acting}>
            {acting ? "Restoring…" : "Restore"}
          </button>
        </div>
      </div>
    {:else if confirm === "revert"}
      <div
        class="confirm-panel"
        role="alertdialog"
        aria-label="Confirm revert"
        tabindex="-1"
        bind:this={panelEl}
        onkeydown={onPanelKeydown}
      >
        <p class="confirm-title">
          Revert this operation
          <span class="confirm-context truncate">“{op.description || "root()"}”</span>
        </p>
        <ul class="consequences">
          {#if op.isCurrent}
            <li>This is the latest operation, so reverting it is a plain undo.</li>
          {:else}
            <li>
              Only this operation is backed out — everything recorded after it
              stays.
            </li>
          {/if}
          <li>
            Nothing is lost: reverting records a new operation, and reverting
            that brings this one back.
          </li>
        </ul>
        <div class="confirm-row">
          <span class="confirm-hint">⌘↵ to confirm</span>
          {#if error}
            <span class="confirm-error truncate" title={error}>{error}</span>
          {/if}
          <button class="confirm-cancel" onclick={() => onToggle(null)} disabled={acting}>
            Cancel
          </button>
          <button class="confirm-go" onclick={runConfirmed} disabled={acting}>
            {acting ? "Reverting…" : "Revert"}
          </button>
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .row {
    display: flex;
    gap: var(--sp-2);
  }

  /* The rail: a continuous line the glyphs sit on, jj-op-log style. */
  .rail {
    position: relative;
    flex-shrink: 0;
    width: 22px;
    display: flex;
    justify-content: center;
  }

  .rail::before,
  .rail::after {
    content: "";
    position: absolute;
    left: 50%;
    width: 1.5px;
    transform: translateX(-50%);
    background: color-mix(in srgb, var(--clr-text-3) 38%, transparent);
    opacity: 0;
  }

  .rail::before {
    top: 0;
    height: 11px;
  }

  .rail::after {
    top: 21px;
    bottom: 0;
  }

  .rail.line-up::before {
    opacity: 1;
  }

  .rail.line-down::after {
    opacity: 1;
  }

  .glyph {
    position: relative;
    z-index: 1;
    margin-top: 12px;
  }

  .glyph.dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    border: 1.5px solid color-mix(in srgb, var(--clr-text-3) 80%, var(--clr-text-2));
    background: var(--clr-bg-1);
  }

  .quiet .glyph.dot {
    width: 6px;
    height: 6px;
    margin-top: 13px;
    border-color: color-mix(in srgb, var(--clr-text-3) 55%, transparent);
  }

  .glyph.at {
    margin-top: 3px;
    font-size: 13px;
    font-weight: 700;
    line-height: 1.6;
    color: var(--clr-accent-strong);
    background: var(--clr-bg-1);
    text-shadow: 0 0 8px color-mix(in srgb, var(--clr-accent) 60%, transparent);
  }

  .body {
    flex: 1;
    min-width: 0;
    padding: 6px 0 7px;
  }

  .head {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }

  .desc {
    font-size: var(--text-m);
    color: var(--clr-text-1);
    overflow-wrap: anywhere;
  }

  .current .desc {
    font-weight: 600;
  }

  .quiet .desc {
    color: var(--clr-text-3);
  }

  .desc.root {
    font-family: var(--font-mono);
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  /* Time-travel actions: hover-revealed so the journal stays calm. */
  .op-actions {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-1);
    margin-left: auto;
    flex-shrink: 0;
    opacity: 0;
    transition: opacity var(--t-fast) var(--ease-out);
  }

  .row:hover .op-actions,
  .row:focus-within .op-actions,
  .row.open .op-actions {
    opacity: 1;
  }

  .op-action {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
    font-size: var(--text-xs);
    font-weight: 500;
    border-radius: 999px;
    padding: 1px 9px;
    border: 1px solid var(--clr-border-2);
    color: var(--clr-text-2);
    background: var(--clr-bg-1);
    transition: all var(--t-fast) var(--ease-out);
  }

  .op-action :global(svg) {
    color: var(--clr-text-3);
  }

  .op-action:hover:not(:disabled),
  .op-action.armed {
    border-color: color-mix(in srgb, var(--clr-accent) 45%, transparent);
    color: var(--clr-accent-strong);
    background: var(--clr-accent-dim);
  }

  .op-action:hover:not(:disabled) :global(svg),
  .op-action.armed :global(svg) {
    color: var(--clr-accent-strong);
  }

  .meta {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    column-gap: var(--sp-2);
    row-gap: 3px;
    margin-top: 2px;
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .quiet .meta {
    margin-top: 0;
  }

  .id {
    color: color-mix(in srgb, var(--clr-text-3) 80%, var(--clr-text-2));
  }

  .time {
    font-variant-numeric: tabular-nums;
  }

  .user {
    max-width: 14em;
  }

  .chips {
    display: inline-flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 4px;
  }

  .chip {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    height: 16px;
    font-size: var(--text-xs);
    border-radius: 999px;
    padding: 0 7px;
    white-space: nowrap;
  }

  .chip.now {
    flex-shrink: 0;
    background: var(--clr-accent-dim);
    color: var(--clr-accent-strong);
  }

  .chip.effect {
    background: var(--clr-bg-3);
    color: var(--clr-text-2);
    border: 1px solid var(--clr-border-2);
  }

  .chip.effect :global(svg) {
    color: var(--clr-text-3);
  }

  .chip.effect.remoteBookmark :global(svg) {
    color: color-mix(in srgb, var(--clr-accent) 70%, var(--clr-text-3));
  }

  .wc-glyph {
    font-style: normal;
    font-size: 10px;
    font-weight: 700;
    color: var(--clr-working-copy);
  }

  .chip.effect.overflow {
    color: var(--clr-text-3);
  }

  /* The plan step before time travel, in the workbench panels' vocabulary. */
  .confirm-panel {
    margin: var(--sp-2) 0 var(--sp-1);
    padding: var(--sp-3);
    background: var(--clr-bg-2);
    border: 1px solid var(--clr-border-1);
    border-radius: var(--radius-m);
    outline: none;
  }

  .confirm-title {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    min-width: 0;
    font-size: var(--text-s);
    font-weight: 600;
    color: var(--clr-text-1);
  }

  .confirm-context {
    min-width: 0;
    font-weight: 400;
    color: var(--clr-text-3);
  }

  .consequences {
    margin: var(--sp-2) 0 0;
    padding-left: 1.3em;
    display: grid;
    gap: 3px;
    font-size: var(--text-s);
    line-height: 1.45;
    color: var(--clr-text-2);
  }

  .confirm-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin-top: var(--sp-2);
  }

  .confirm-hint {
    flex: 1;
    min-width: 0;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .confirm-error {
    min-width: 0;
    max-width: 28em;
    font-size: var(--text-xs);
    color: var(--clr-danger);
  }

  .confirm-cancel,
  .confirm-go {
    flex-shrink: 0;
    font-size: var(--text-xs);
    font-weight: 500;
    border-radius: 999px;
    padding: 2px 11px;
    transition: all var(--t-fast) var(--ease-out);
  }

  .confirm-cancel {
    color: var(--clr-text-3);
    border: 1px solid var(--clr-border-2);
  }

  .confirm-cancel:hover:not(:disabled) {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .confirm-go {
    color: var(--clr-accent-strong);
    background: var(--clr-accent-dim);
    border: 1px solid transparent;
  }

  .confirm-go:hover:not(:disabled) {
    border-color: color-mix(in srgb, var(--clr-accent) 45%, transparent);
  }

  .confirm-go:disabled,
  .confirm-cancel:disabled {
    opacity: 0.6;
  }
</style>
