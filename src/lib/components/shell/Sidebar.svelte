<script lang="ts">
  import Icon from "$lib/components/ui/Icon.svelte";
  import type { IconName } from "$lib/components/ui/icons";
  import { app, type Section } from "$lib/state/app.svelte";
  import { chooseRepo } from "$lib/state/actions";

  const items: { id: Section; icon: IconName; label: string }[] = [
    { id: "workbench", icon: "workbench", label: "Workbench" },
    { id: "conflicts", icon: "conflicts", label: "Conflicts" },
    { id: "publish", icon: "publish", label: "Publish" },
    { id: "operations", icon: "operations", label: "Operations" },
    { id: "workspaces", icon: "workspaces", label: "Workspaces" },
  ];

  const snapshot = $derived(app.snapshot);
  const hasRepo = $derived(snapshot !== null);
  // The inbox count rides the nav item so conflict state is never hidden
  // behind a section switch.
  const conflictCount = $derived(snapshot?.conflicts.length ?? 0);
</script>

<nav class="sidebar" data-tauri-drag-region>
  <!-- Clearance for the macOS traffic lights (overlay title bar). -->
  <div class="traffic-spacer" data-tauri-drag-region></div>

  <!-- The repo the whole window is about; also the door to another one. -->
  <button
    class="repo-block"
    title={snapshot
      ? `${snapshot.repoPath} — switch repository (⌘O)`
      : "Open repository (⌘O)"}
    disabled={app.opening}
    onclick={chooseRepo}
  >
    <span class="brand mono">jj</span>
    <span class="repo-meta">
      <span class="repo-name truncate">
        {snapshot ? snapshot.repoName : "Jiji"}
      </span>
      <span class="repo-sub truncate">
        {app.opening
          ? "Opening…"
          : snapshot
            ? snapshot.repoPath
            : "Open a repository"}
      </span>
    </span>
    <span class="repo-switch"><Icon name="folder" size={13} /></span>
  </button>

  <div class="items">
    {#each items as item (item.id)}
      <button
        class="nav-item"
        class:active={hasRepo && app.section === item.id}
        disabled={!hasRepo}
        title={item.label}
        aria-label={item.label}
        onclick={() => (app.section = item.id)}
      >
        <Icon name={item.icon} size={15} />
        <span class="label">{item.label}</span>
        {#if item.id === "conflicts" && hasRepo && conflictCount > 0}
          <span class="badge mono">
            {conflictCount > 9 ? "9+" : conflictCount}
          </span>
        {/if}
      </button>
    {/each}
  </div>

  <div class="spacer" data-tauri-drag-region></div>
  <div class="foot mono">v0.1</div>
</nav>

<style>
  .sidebar {
    width: 212px;
    height: 100%;
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    padding: 0 var(--sp-2) var(--sp-3);
    border-right: 1px solid var(--clr-border-2);
  }

  .traffic-spacer {
    height: 38px;
    width: 100%;
    flex-shrink: 0;
  }

  .repo-block {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    padding: var(--sp-2);
    margin-bottom: var(--sp-4);
    border-radius: var(--radius-m);
    text-align: left;
    transition: background var(--t-fast) var(--ease-out);
  }

  .repo-block:hover:not(:disabled) {
    background: var(--clr-bg-hover);
  }

  .repo-block:disabled {
    opacity: 0.6;
  }

  .brand {
    flex-shrink: 0;
    width: 28px;
    height: 28px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-m);
    background: var(--clr-accent-dim);
    color: var(--clr-accent-strong);
    font-size: var(--text-m);
    font-weight: 700;
    letter-spacing: -0.5px;
  }

  .repo-meta {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }

  .repo-name {
    font-size: var(--text-m);
    font-weight: 650;
    letter-spacing: -0.01em;
    color: var(--clr-text-1);
  }

  .repo-sub {
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .repo-switch {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    color: var(--clr-text-3);
    opacity: 0;
    transition: opacity var(--t-fast) var(--ease-out);
  }

  .repo-block:hover .repo-switch {
    opacity: 1;
  }

  .items {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .nav-item {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    height: 30px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-m);
    font-size: var(--text-m);
    font-weight: 500;
    color: var(--clr-text-2);
    text-align: left;
    transition:
      background var(--t-fast) var(--ease-out),
      color var(--t-fast) var(--ease-out);
  }

  .nav-item :global(svg) {
    flex-shrink: 0;
    color: var(--clr-text-3);
    transition: color var(--t-fast) var(--ease-out);
  }

  .label {
    flex: 1;
    min-width: 0;
  }

  .badge {
    flex-shrink: 0;
    min-width: 16px;
    height: 16px;
    display: grid;
    place-items: center;
    padding: 0 4px;
    font-size: 9px;
    font-weight: 600;
    line-height: 1;
    border-radius: 999px;
    background: var(--clr-danger);
    color: var(--clr-bg-0);
  }

  .nav-item:hover:not(:disabled) {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .nav-item:hover:not(:disabled) :global(svg) {
    color: var(--clr-text-2);
  }

  .nav-item.active {
    background: var(--clr-accent-dim);
    color: var(--clr-accent-strong);
  }

  .nav-item.active :global(svg) {
    color: var(--clr-accent-strong);
  }

  .nav-item:disabled {
    opacity: 0.35;
    cursor: default;
  }

  .spacer {
    flex: 1;
  }

  .foot {
    padding-left: var(--sp-2);
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }
</style>
