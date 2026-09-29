<script lang="ts">
  import Icon from "$lib/components/ui/Icon.svelte";
  import { drag } from "$lib/components/graph/dnd.svelte";
  import {
    autolandChip,
    autolandStory,
    isInterrupted,
    visibleAutolandJobs,
  } from "$lib/components/publish/autoland";
  import { app } from "$lib/state/app.svelte";
  import {
    dismissBreadcrumb,
    dismissError,
    goToSection,
    showOperations,
    undoLastMutation,
  } from "$lib/state/actions";
  import { autoland, dismissAutoLand } from "$lib/state/autoland.svelte";
  import { appUpdate, restartToUpdate } from "$lib/state/update.svelte";

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
    <!-- Errors hold the line until the next snapshot or a dismissal; the
         full text (jj's multi-line hints included) reads on hover. -->
    <span class="dot danger"></span>
    <span class="msg danger-text truncate" title={app.error}>{app.error}</span>
    <button
      class="dismiss"
      data-error-dismiss
      title="Dismiss"
      aria-label="Dismiss error"
      onclick={dismissError}
    >
      <Icon name="close" size={10} />
    </button>
  {:else if breadcrumb}
    <!-- A just-run mutation takes the line for a few seconds, with the
         way back one click away. -->
    <span class="bc-wrap" data-breadcrumb>
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
          <span class="op-id mono">{breadcrumb.outcome.operationId.slice(0, 8)}</span>
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
    <span class="dot"></span>
    <span class="msg truncate">{latestOp.description}</span>
    {#if snapshot.backend === "mock"}
      <span class="badge">mock data</span>
    {/if}
  {:else}
    <span class="dot"></span>
    <span class="msg">Idle — open a repository to begin</span>
  {/if}

  <div class="fill"></div>

  {#each visibleAutolandJobs(autoland.jobs, snapshot?.repoPath) as status (status.record.state.headBookmark)}
    <!-- The activity chips: every auto-land job story stays visible from
         every section (the live job first), and clicking one lands on the
         Publish job cards. A record restored from an earlier session (or
         whose repo was switched away) wears the interrupted state; each
         record only renders while its own repo is open. -->
    {@const job = status.record.state}
    {@const chip = autolandChip(status)}
    <span class="al-wrap">
      <button
        class="al-chip {chip.tone}"
        data-autoland-chip={isInterrupted(status)
          ? "interrupted"
          : job.phase.kind}
        title={autolandStory(status)}
        onclick={() => goToSection("publish")}
      >
        <span class="al-dot" class:pulse={chip.pulse}></span>
        <span class="truncate">{autoland.stopping && status.live ? `Auto-land ${job.headBookmark}: stopping…` : chip.label}</span>
      </button>
      {#if chip.dismissable}
        <button
          class="dismiss"
          data-autoland-dismiss
          title="Dismiss"
          onclick={() => void dismissAutoLand(status)}
        >
          <Icon name="close" size={10} />
        </button>
      {/if}
    </span>
  {/each}

  {#if appUpdate.version}
    <!-- A downloaded update waits for the user; nothing restarts on its own. -->
    <button
      class="update"
      data-app-update
      disabled={appUpdate.installing}
      title="Jiji {appUpdate.version} is downloaded. Restart to install it."
      onclick={() => void restartToUpdate()}
    >
      <Icon name="arrowUp" size={11} />
      {appUpdate.installing ? "Updating…" : `Restart to update to ${appUpdate.version}`}
    </button>
  {/if}

  {#if snapshot}
    <span class="meta mono" title="Repository backend">{snapshot.backend}</span>
  {/if}
</footer>

<style>
  .statusbar {
    height: var(--statusbar-h);
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: 0 var(--sp-3);
    border-top: 1px solid var(--clr-border-2);
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: color-mix(in srgb, var(--clr-text-3) 70%, transparent);
    flex-shrink: 0;
  }

  .dot.danger {
    background: var(--clr-danger);
  }

  .dot.accent {
    background: var(--clr-accent);
  }

  .msg {
    color: var(--clr-text-2);
    font-size: var(--text-xs);
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
  }

  .bc-undo {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
    height: 18px;
    padding: 0 6px;
    border-radius: var(--radius-s);
    font-size: var(--text-xs);
    font-weight: 500;
    color: var(--clr-accent-strong);
    transition: background var(--t-fast) var(--ease-out);
  }

  .bc-undo:hover:not(:disabled) {
    background: var(--clr-accent-dim);
  }

  .bc-undo:disabled {
    opacity: 0.6;
  }

  .update {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
    height: 18px;
    padding: 0 6px;
    border-radius: var(--radius-s);
    font-size: var(--text-xs);
    font-weight: 500;
    color: var(--clr-accent-strong);
    transition: background var(--t-fast) var(--ease-out);
  }

  .update:hover:not(:disabled) {
    background: var(--clr-accent-dim);
  }

  .update:disabled {
    opacity: 0.6;
  }

  .danger-text {
    color: var(--clr-danger);
  }

  .al-wrap {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    min-width: 0;
    flex-shrink: 1;
  }

  .al-chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    height: 18px;
    padding: 0 6px;
    border-radius: var(--radius-s);
    font-size: var(--text-xs);
    font-weight: 500;
    color: var(--clr-text-2);
    transition: background var(--t-fast) var(--ease-out);
  }

  .al-chip:hover {
    background: var(--clr-bg-hover);
  }

  .al-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: currentColor;
    flex-shrink: 0;
  }

  /* A job that is actually working keeps a slow heartbeat. */
  .al-dot.pulse {
    animation: pulse 2s ease-in-out infinite;
  }

  .al-chip.accent {
    color: var(--clr-accent-strong);
  }

  .al-chip.ok {
    color: var(--clr-ok);
  }

  .al-chip.warn {
    color: var(--clr-warn);
  }

  .al-chip.danger {
    color: var(--clr-danger);
  }

  .al-chip.muted {
    color: var(--clr-text-3);
  }

  .dismiss {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    width: 16px;
    height: 16px;
    color: var(--clr-text-3);
    border-radius: var(--radius-s);
    transition:
      color var(--t-fast) var(--ease-out),
      background var(--t-fast) var(--ease-out);
  }

  .dismiss:hover {
    color: var(--clr-text-1);
    background: var(--clr-bg-hover);
  }

  .badge {
    flex-shrink: 0;
    font-size: var(--text-xs);
    color: var(--clr-warn);
  }

  .fill {
    flex: 1;
  }

  .meta {
    flex-shrink: 0;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }
</style>
