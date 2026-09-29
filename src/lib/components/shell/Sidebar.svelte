<script lang="ts">
  import Icon from "$lib/components/ui/Icon.svelte";
  import type { IconName } from "$lib/components/ui/icons";
  import { app, type Section } from "$lib/state/app.svelte";

  // The section rail: one icon per section, labels in the tooltip, the
  // ⌘1–⌘6 shortcuts beside them. Everything else the old sidebar carried
  // (repo identity, search, open) lives in the title bar and palette, so
  // the rail costs the diff 44px and nothing more.
  const items: { id: Section; icon: IconName; label: string; key: string }[] = [
    { id: "workbench", icon: "workbench", label: "Workbench", key: "1" },
    { id: "conflicts", icon: "conflicts", label: "Conflicts", key: "2" },
    { id: "publish", icon: "publish", label: "Publish", key: "3" },
    { id: "bookmarks", icon: "bookmark", label: "Bookmarks", key: "4" },
    { id: "operations", icon: "operations", label: "Operations", key: "5" },
    { id: "workspaces", icon: "workspaces", label: "Workspaces", key: "6" },
  ];

  const snapshot = $derived(app.snapshot);
  // The inbox count rides the rail so conflict state is never hidden
  // behind a section switch.
  const conflictCount = $derived(snapshot?.conflicts.length ?? 0);
</script>

<!-- No repo, no sections: the welcome screen gets the whole window. -->
{#if snapshot && !app.sidebarCollapsed}
  <nav class="rail" aria-label="Repository navigation" data-tauri-drag-region>
    {#each items as item (item.id)}
      {@const active = app.section === item.id}
      <button
        class="item"
        class:active
        title="{item.label} (⌘{item.key})"
        aria-label={item.label}
        aria-current={active ? "page" : undefined}
        onclick={() => (app.section = item.id)}
      >
        <Icon name={item.icon} size={17} />
        {#if item.id === "conflicts" && conflictCount > 0}
          <span class="badge" aria-label="{conflictCount} conflicts">
            {conflictCount > 9 ? "9+" : conflictCount}
          </span>
        {/if}
      </button>
    {/each}
  </nav>
{/if}

<style>
  .rail {
    width: var(--rail-w);
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
    padding-top: 6px;
    border-top: 1px solid var(--clr-border-2);
    border-right: 1px solid var(--clr-border-2);
  }

  .item {
    position: relative;
    width: 32px;
    height: 32px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-m);
    color: var(--clr-text-3);
    transition:
      background var(--t-fast) var(--ease-out),
      color var(--t-fast) var(--ease-out);
  }

  .item:hover {
    color: var(--clr-text-1);
    background: var(--clr-bg-hover);
  }

  /* The current section: the icon comes up to full ink on a quiet fill,
     with a short accent tick on the rail's edge. */
  .item.active {
    color: var(--clr-text-1);
    background: color-mix(in srgb, var(--clr-text-1) 8%, transparent);
  }

  .item.active::before {
    content: "";
    position: absolute;
    left: -6px;
    top: 9px;
    bottom: 9px;
    width: 2px;
    border-radius: 0 2px 2px 0;
    background: var(--clr-accent);
  }

  .badge {
    position: absolute;
    top: 3px;
    right: 2px;
    min-width: 14px;
    height: 14px;
    padding: 0 3px;
    display: grid;
    place-items: center;
    border-radius: 7px;
    background: var(--clr-danger);
    color: #ffffff;
    font-size: 9px;
    font-weight: 700;
    line-height: 1;
    font-variant-numeric: tabular-nums;
    /* Cut out of the icon beneath it. */
    box-shadow: 0 0 0 2px var(--clr-bg-0);
  }
</style>
