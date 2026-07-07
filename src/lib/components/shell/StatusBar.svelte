<script lang="ts">
  import { fly } from "svelte/transition";
  import Icon from "$lib/components/ui/Icon.svelte";
  import { drag } from "$lib/components/graph/dnd.svelte";
  import {
    autolandChip,
    autolandStory,
    autolandVisible,
    isInterrupted,
  } from "$lib/components/publish/autoland";
  import { motionMs } from "$lib/motion";
  import { app } from "$lib/state/app.svelte";
  import {
    dismissBreadcrumb,
    goToSection,
    showOperations,
    undoLastMutation,
  } from "$lib/state/actions";
  import { autoland, dismissAutoLand } from "$lib/state/autoland.svelte";

  const snapshot = $derived(app.snapshot);
  const latestOp = $derived(snapshot?.operations[0]);

  // The mutation breadcrumb takes over the status line briefly, then the
  // bar falls back to the snapshot's latest operation. The Undo affordance
  // is convenience, not the only path — the Operations timeline can revert
  // any operation at any time.
  const BREADCRUMB_MS = 8000;
  const breadcrumb = $derived(app.lastMutation);
  $effect(() => {
    if (!breadcrumb || undoing) return;
    const timer = setTimeout(dismissBreadcrumb, BREADCRUMB_MS);
    return () => clearTimeout(timer);
  });

  let undoing = $state(false);
  async function undo() {
    if (undoing) return;
    undoing = true;
    try {
      await undoLastMutation();
    } finally {
      undoing = false;
    }
  }
</script>

<footer class="statusbar">
  {#if drag.active}
    <!-- The live meaning of the drag gesture, mirroring the plan card. -->
    {#if drag.plan?.allowed}
      <span class="dot accent"></span>
      <span class="msg truncate">{drag.plan.summary} — release to apply</span>
    {:else if drag.plan}
      <span class="dot danger"></span>
      <span class="msg danger-text truncate">{drag.plan.reason}</span>
    {:else}
      <span class="dot"></span>
      <span class="msg truncate">
        Drag onto the change that becomes the new parent — ⌥ moves it alone, Esc cancels
      </span>
    {/if}
  {:else if app.error}
    <span class="dot danger"></span>
    <span class="msg danger-text truncate">{app.error}</span>
  {:else if breadcrumb}
    <!-- The takeover rises in so a just-run mutation registers in the
         periphery even when the eyes are on the graph. -->
    <span class="bc-wrap" in:fly={{ y: 6, duration: motionMs(150) }}>
      <span class="dot accent"></span>
      <button
        class="msg breadcrumb truncate"
        title="Show in Operations"
        onclick={() => {
          dismissBreadcrumb();
          showOperations();
        }}
      >
        {breadcrumb.outcome.summary}
        {#if breadcrumb.outcome.operationId}
          <span class="op-id mono">op {breadcrumb.outcome.operationId.slice(0, 8)}</span>
        {/if}
      </button>
      {#if breadcrumb.outcome.operationId}
        <button class="bc-undo" onclick={undo} disabled={undoing} title="Revert this operation">
          <Icon name="undo" size={11} />
          {undoing ? "Undoing…" : "Undo"}
        </button>
      {/if}
    </span>
  {:else if snapshot && latestOp}
    <span class="dot pulse"></span>
    <span class="msg truncate">{latestOp.description}</span>
    {#if snapshot.backend === "mock"}
      <span class="badge mono">mock data</span>
    {/if}
  {:else}
    <span class="dot"></span>
    <span class="msg">Idle — open a repository to begin</span>
  {/if}

  <div class="fill"></div>

  {#if autoland.job && autolandVisible(autoland.job, snapshot?.repoPath)}
    <!-- The activity chip: the auto-land job stays visible from every
         section, and clicking it lands on the Publish job card. A record
         restored from an earlier session wears the interrupted state; it
         only renders while its own repo is open. -->
    {@const status = autoland.job}
    {@const job = status.record.state}
    {@const chip = autolandChip(status)}
    <span class="al-wrap" in:fly={{ y: 6, duration: motionMs(150) }}>
      <button
        class="al-chip {chip.tone}"
        data-autoland-chip={isInterrupted(status)
          ? "interrupted"
          : job.phase.kind}
        title={autolandStory(status)}
        onclick={() => goToSection("publish")}
      >
        <span class="al-dot" class:pulse={chip.pulse}></span>
        <span class="truncate">{autoland.stopping && !chip.dismissable ? `Auto-land ${job.headBookmark}: stopping…` : chip.label}</span>
      </button>
      {#if chip.dismissable}
        <button
          class="al-x"
          data-autoland-dismiss
          title="Dismiss"
          onclick={() => void dismissAutoLand()}
        >
          ×
        </button>
      {/if}
    </span>
  {/if}

  <span class="meta mono">backend: {snapshot?.backend ?? "—"}</span>
</footer>

<style>
  .statusbar {
    height: 30px;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: 0 var(--sp-4);
    font-size: var(--text-s);
    color: var(--clr-text-3);
    border-top: 1px solid var(--clr-border-2);
  }

  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--clr-text-3);
    flex-shrink: 0;
  }

  .dot.pulse {
    background: var(--clr-ok);
    animation: pulse 2.4s var(--ease-out) infinite;
  }

  .dot.danger {
    background: var(--clr-danger);
  }

  .dot.accent {
    background: var(--clr-accent-strong);
  }

  .msg {
    color: var(--clr-text-2);
  }

  .bc-wrap {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }

  .breadcrumb {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
    color: var(--clr-text-1);
    transition: color var(--t-fast) var(--ease-out);
  }

  .breadcrumb:hover {
    color: var(--clr-accent-strong);
  }

  .op-id {
    flex-shrink: 0;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
    border: 1px solid var(--clr-border-2);
    border-radius: 999px;
    padding: 0 7px;
  }

  .bc-undo {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
    font-size: var(--text-xs);
    font-weight: 500;
    color: var(--clr-accent-strong);
    background: var(--clr-accent-dim);
    border: 1px solid transparent;
    border-radius: 999px;
    padding: 1px 9px;
    transition: all var(--t-fast) var(--ease-out);
  }

  .bc-undo:hover:not(:disabled) {
    border-color: color-mix(in srgb, var(--clr-accent) 45%, transparent);
  }

  .bc-undo:disabled {
    opacity: 0.6;
  }

  .danger-text {
    color: var(--clr-danger);
  }

  .al-wrap {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
    flex-shrink: 1;
  }

  .al-chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    font-size: var(--text-xs);
    font-weight: 500;
    border: 1px solid var(--clr-border-2);
    border-radius: 999px;
    padding: 1px 9px;
    color: var(--clr-text-2);
    transition: border-color var(--t-fast) var(--ease-out);
  }

  .al-chip:hover {
    border-color: color-mix(in srgb, var(--clr-accent) 45%, transparent);
  }

  .al-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: currentColor;
    flex-shrink: 0;
  }

  .al-dot.pulse {
    animation: pulse 2.4s var(--ease-out) infinite;
  }

  .al-chip.accent {
    color: var(--clr-accent-strong);
    border-color: color-mix(in srgb, var(--clr-accent) 35%, transparent);
  }

  .al-chip.ok {
    color: var(--clr-ok);
    border-color: color-mix(in srgb, var(--clr-ok) 35%, transparent);
  }

  .al-chip.warn {
    color: var(--clr-warn);
    border-color: color-mix(in srgb, var(--clr-warn) 35%, transparent);
  }

  .al-chip.danger {
    color: var(--clr-danger);
    border-color: color-mix(in srgb, var(--clr-danger) 35%, transparent);
  }

  .al-chip.muted {
    color: var(--clr-text-3);
  }

  .al-x {
    flex-shrink: 0;
    font-size: var(--text-s);
    line-height: 1;
    color: var(--clr-text-3);
    border-radius: 999px;
    padding: 2px 5px;
    transition: color var(--t-fast) var(--ease-out);
  }

  .al-x:hover {
    color: var(--clr-text-1);
  }

  .badge {
    font-size: var(--text-xs);
    color: var(--clr-warn);
    border: 1px solid color-mix(in srgb, var(--clr-warn) 35%, transparent);
    border-radius: 999px;
    padding: 1px 8px;
  }

  .fill {
    flex: 1;
  }

  .meta {
    font-size: var(--text-xs);
  }

  @keyframes pulse {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.35;
    }
  }
</style>
