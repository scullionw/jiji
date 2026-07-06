<script lang="ts">
  import Icon from "$lib/components/ui/Icon.svelte";
  import ThemeMenu from "./ThemeMenu.svelte";
  import LicenseBadge from "./LicenseBadge.svelte";
  import { app } from "$lib/state/app.svelte";
  import { chooseRepo, refreshSnapshot, togglePalette } from "$lib/state/actions";
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

<header class="topbar" data-tauri-drag-region>
  {#if snapshot}
    <div class="repo">
      <span class="repo-tile"><Icon name="folder" size={13} /></span>
      <span class="name">{snapshot.repoName}</span>
      <span class="path mono truncate" title={snapshot.repoPath}>
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
      <span class="upstream-dot"></span>
      {upstreamState.label}
    </button>
  {/if}
  {#if snapshot && trunk}
    <button
      class="icon-btn"
      title="Refresh snapshot (⌘R)"
      aria-label="Refresh snapshot"
      onclick={refreshSnapshot}
    >
      <Icon name="refresh" size={15} />
    </button>
  {/if}
  {#if snapshot}
    <span class="divider" aria-hidden="true"></span>
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
    align-items: center;
    gap: var(--sp-2);
    color: var(--clr-text-3);
    min-width: 0;
  }

  .repo-tile {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border-radius: var(--radius-s);
    background: var(--clr-bg-2);
    border: 1px solid var(--clr-border-2);
    color: var(--clr-text-2);
    flex-shrink: 0;
  }

  .name {
    font-weight: 650;
    font-size: var(--text-m);
    letter-spacing: -0.01em;
    color: var(--clr-text-1);
  }

  .path {
    font-size: var(--text-xs);
    color: var(--clr-text-3);
    max-width: 320px;
  }

  .divider {
    width: 1px;
    height: 16px;
    background: var(--clr-border-2);
    flex-shrink: 0;
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

  /* The upstream check: quiet by design (the GitButler lesson — always
     visible, never noisy). Age text idle, a soft pulse while checking,
     warn tone only when the remote could not be reached. */
  .upstream-chip {
    display: flex;
    align-items: center;
    gap: 6px;
    height: 28px;
    padding: 0 var(--sp-3);
    border: 1px solid var(--clr-border-2);
    border-radius: 999px;
    background: var(--clr-bg-1);
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

  .upstream-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--clr-text-3);
  }

  .upstream-chip.busy .upstream-dot {
    background: var(--clr-accent);
    animation: upstream-pulse 1.2s ease-in-out infinite;
  }

  .upstream-chip.failed {
    color: var(--clr-warn);
  }

  .upstream-chip.failed .upstream-dot {
    background: var(--clr-warn);
  }

  @keyframes upstream-pulse {
    50% {
      opacity: 0.3;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .upstream-chip.busy .upstream-dot {
      animation: none;
    }
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
    border: 1px solid transparent;
    color: var(--clr-text-2);
    transition:
      background var(--t-fast) var(--ease-out),
      border-color var(--t-fast) var(--ease-out),
      color var(--t-fast) var(--ease-out);
  }

  .icon-btn:hover {
    background: var(--clr-bg-hover);
    border-color: var(--clr-border-2);
    color: var(--clr-text-1);
  }
</style>
