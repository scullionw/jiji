<script lang="ts">
  import Icon from "$lib/components/ui/Icon.svelte";
  import ThemeMenu from "./ThemeMenu.svelte";
  import LicenseBadge from "./LicenseBadge.svelte";
  import { app } from "$lib/state/app.svelte";
  import { chooseRepo, refreshSnapshot, togglePalette } from "$lib/state/actions";

  const snapshot = $derived(app.snapshot);
  const trunk = $derived(snapshot?.bookmarks.find((b) => b.isTrunk));

  const syncLabel: Record<string, string> = {
    synced: "up to date",
    ahead: "ahead",
    behind: "behind",
    diverged: "diverged",
    localOnly: "local only",
  };
</script>

<header class="topbar" data-tauri-drag-region>
  {#if snapshot}
    <div class="repo">
      <Icon name="folder" size={14} />
      <span class="name">{snapshot.repoName}</span>
      <span class="path truncate" title={snapshot.repoPath}>
        {snapshot.repoPath}
      </span>
    </div>
  {:else}
    <div class="repo">
      <span class="name">Jiji</span>
      <span class="path">No repository open</span>
    </div>
  {/if}

  <div class="fill" data-tauri-drag-region></div>

  {#if snapshot && trunk}
    <div class="trunk-chip" title="Trunk target">
      <Icon name="branch" size={13} />
      <span class="mono">
        {trunk.name}{trunk.remote ? `@${trunk.remote}` : ""}
      </span>
      <span class="sync-dot" class:ok={trunk.sync === "synced"}></span>
      <span class="sync-label">{syncLabel[trunk.sync] ?? trunk.sync}</span>
    </div>
    <button
      class="icon-btn"
      title="Refresh snapshot (⌘R)"
      aria-label="Refresh snapshot"
      onclick={refreshSnapshot}
    >
      <Icon name="refresh" size={15} />
    </button>
  {/if}
  <LicenseBadge />
  <button
    class="icon-btn"
    title="Command palette (⌘K)"
    aria-label="Command palette"
    data-action="palette"
    onclick={togglePalette}
  >
    <Icon name="command" size={14} />
  </button>
  <ThemeMenu />
  <button
    class="icon-btn"
    title="Open repository (⌘O)"
    aria-label="Open repository"
    onclick={chooseRepo}
  >
    <Icon name="folder" size={15} />
  </button>
</header>

<style>
  .topbar {
    height: 46px;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: 0 var(--sp-2) 0 var(--sp-3);
  }

  .repo {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    color: var(--clr-text-3);
    min-width: 0;
  }

  .repo :global(svg) {
    align-self: center;
  }

  .name {
    font-weight: 600;
    font-size: var(--text-m);
    color: var(--clr-text-1);
  }

  .path {
    font-size: var(--text-s);
    color: var(--clr-text-3);
    max-width: 320px;
  }

  .fill {
    flex: 1;
    height: 100%;
  }

  .trunk-chip {
    display: flex;
    align-items: center;
    gap: 7px;
    height: 28px;
    padding: 0 var(--sp-3);
    border: 1px solid var(--clr-border-2);
    border-radius: 999px;
    background: var(--clr-bg-1);
    color: var(--clr-text-2);
    font-size: var(--text-s);
  }

  .sync-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--clr-text-3);
  }

  .sync-dot.ok {
    background: var(--clr-ok);
  }

  .sync-label {
    color: var(--clr-text-3);
  }

  .icon-btn {
    width: 30px;
    height: 30px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-m);
    color: var(--clr-text-2);
    transition:
      background var(--t-fast) var(--ease-out),
      color var(--t-fast) var(--ease-out);
  }

  .icon-btn:hover {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }
</style>
