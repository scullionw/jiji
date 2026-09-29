<script lang="ts">
  import Icon from "$lib/components/ui/Icon.svelte";
  import SettingsMenu from "./SettingsMenu.svelte";
  import TabBar from "./TabBar.svelte";
  import ThemeMenu from "./ThemeMenu.svelte";
  import LicenseBadge from "./LicenseBadge.svelte";
  import { app } from "$lib/state/app.svelte";
  import {
    openSearch,
    toggleGraphPanel,
    togglePalette,
  } from "$lib/state/actions";
  import { fetchUpstreamNow, upstream } from "$lib/state/upstream.svelte";
  import { upstreamChip } from "./upstream";

  const snapshot = $derived(app.snapshot);
  const trunk = $derived(snapshot?.bookmarks.find((b) => b.isTrunk));
  const upstreamState = $derived(
    snapshot
      ? upstreamChip(
          upstream,
          snapshot.gitRemotes.map((r) => r.name),
          upstream.now,
        )
      : null,
  );

  const syncLabel: Record<string, string> = {
    synced: "up to date",
    ahead: "ahead",
    behind: "behind",
    diverged: "diverged",
    localOnly: "local only",
  };
</script>

<!-- The title bar: the window's one horizontal strip of chrome. Repo tabs
     on the left (after the traffic lights on a Mac), where trunk stands
     and when the remote was last checked, then the window's controls.
     Every empty stretch keeps dragging the window. -->
<header class="titlebar" data-tauri-drag-region>
  <div class="lights" data-tauri-drag-region></div>
  <TabBar />

  {#if snapshot && (trunk || upstreamState)}
    <div class="sync" data-tauri-drag-region>
      {#if trunk}
        <span class="trunk" title="Trunk: {trunk.name}{trunk.remote ? `@${trunk.remote}` : ''} — {syncLabel[trunk.sync] ?? trunk.sync}">
          <Icon name="branch" size={12} />
          <span class="mono trunk-name">{trunk.name}{trunk.remote ? `@${trunk.remote}` : ""}</span>
          <span class="sync-state" class:off={trunk.sync !== "synced"}>
            {syncLabel[trunk.sync] ?? trunk.sync}
          </span>
        </span>
      {/if}
      {#if upstreamState}
        <button
          class="upstream-chip"
          class:busy={upstreamState.tone === "busy"}
          class:failed={upstreamState.tone === "error"}
          title={upstreamState.title}
          aria-label={upstreamState.title}
          data-upstream={upstreamState.tone}
          disabled={upstream.checking}
          onclick={() => void fetchUpstreamNow()}
        >
          <Icon name="refresh" size={11} />
          {upstreamState.label}
        </button>
      {/if}
    </div>
  {/if}

  <div class="actions">
    <!-- The graph pane's toggle lives up here so it stays reachable while
         the pane itself is off screen. Workbench-only: the other sections
         have no graph pane to bring back. -->
    {#if snapshot && app.section === "workbench"}
      <button
        class="icon-btn"
        title="{app.graphCollapsed ? 'Show' : 'Hide'} graph panel (⌘⇧B)"
        aria-label="{app.graphCollapsed ? 'Show' : 'Hide'} graph panel"
        onclick={toggleGraphPanel}
      >
        <Icon name={app.graphCollapsed ? "panelOpen" : "panelClose"} size={15} />
      </button>
    {/if}
    {#if snapshot}
      <button
        class="icon-btn"
        title="Search history (⌘⇧F)"
        aria-label="Search history"
        onclick={() => openSearch()}
      >
        <Icon name="search" size={15} />
      </button>
    {/if}
    <button
      class="icon-btn"
      title="Command palette (⌘K)"
      aria-label="Command palette"
      data-action="palette"
      onclick={togglePalette}
    >
      <Icon name="command" size={15} />
    </button>
    <span class="sep" aria-hidden="true"></span>
    <LicenseBadge />
    <SettingsMenu />
    <ThemeMenu />
  </div>
</header>

<style>
  .titlebar {
    height: var(--titlebar-h);
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding-right: var(--sp-2);
  }

  /* Clearance for the macOS traffic lights, which sit at their native
     position in the overlay title bar (the bar's height centers them).
     Native fullscreen hides them — AppShell flags the root — and other
     platforms draw their own frame, so both give the room back. */
  .lights {
    flex-shrink: 0;
    width: var(--sp-2);
  }

  :global([data-platform="mac"]) .lights {
    width: 68px;
  }

  :global([data-platform="mac"][data-fullscreen]) .lights {
    width: var(--sp-2);
  }

  .sync {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .trunk {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    min-width: 0;
  }

  .trunk-name {
    color: var(--clr-text-2);
    font-size: var(--text-xs);
  }

  .sync-state.off {
    color: var(--clr-warn);
  }

  /* The upstream check: always visible, never loud. The age idles in
     tertiary ink, the icon turns while checking, a failed fetch goes
     warn. */
  .upstream-chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 22px;
    padding: 0 6px;
    margin: 0 -6px;
    border-radius: var(--radius-m);
    color: var(--clr-text-3);
    font-size: var(--text-s);
    transition:
      background var(--t-fast) var(--ease-out),
      color var(--t-fast) var(--ease-out);
  }

  .upstream-chip:hover:not(:disabled) {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .upstream-chip.busy :global(svg) {
    animation: spin 0.9s linear infinite;
  }

  .upstream-chip.failed {
    color: var(--clr-warn);
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  .actions {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: 2px;
  }

  .actions :global(.icon-btn) {
    width: 24px;
    height: 24px;
  }

  .sep {
    width: 1px;
    height: 14px;
    margin: 0 6px;
    background: var(--clr-border-1);
  }

  @media (max-width: 1180px) {
    .trunk {
      display: none;
    }
  }

  @media (max-width: 1020px) {
    .sync {
      display: none;
    }
  }
</style>
