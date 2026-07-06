<script lang="ts">
  import Icon from "$lib/components/ui/Icon.svelte";
  import type { IconName } from "$lib/components/ui/icons";
  import { app, type Section } from "$lib/state/app.svelte";

  const items: { id: Section; icon: IconName; label: string }[] = [
    { id: "workbench", icon: "workbench", label: "Workbench" },
    { id: "conflicts", icon: "conflicts", label: "Conflicts" },
    { id: "publish", icon: "publish", label: "Publish" },
    { id: "operations", icon: "operations", label: "Operations" },
    { id: "workspaces", icon: "workspaces", label: "Workspaces" },
  ];

  const hasRepo = $derived(app.snapshot !== null);
  // The inbox count rides the nav item so conflict state is never hidden
  // behind a section switch.
  const conflictCount = $derived(app.snapshot?.conflicts.length ?? 0);
</script>

<nav class="sidebar" data-tauri-drag-region>
  <!-- Clearance for the macOS traffic lights (overlay title bar). -->
  <div class="traffic-spacer" data-tauri-drag-region></div>
  <div class="brand mono" title="Jiji">jj</div>
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
        <Icon name={item.icon} size={18} />
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
    width: 58px;
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    padding-bottom: var(--sp-3);
  }

  .traffic-spacer {
    height: 38px;
    width: 100%;
    flex-shrink: 0;
  }

  .brand {
    width: 32px;
    height: 32px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-m);
    background: linear-gradient(
      145deg,
      color-mix(in srgb, var(--clr-accent) 26%, var(--clr-bg-2)),
      color-mix(in srgb, var(--clr-accent) 10%, var(--clr-bg-1))
    );
    border: 1px solid color-mix(in srgb, var(--clr-accent) 28%, transparent);
    box-shadow: var(--shadow-1);
    color: var(--clr-accent-strong);
    font-size: var(--text-m);
    font-weight: 700;
    letter-spacing: -0.5px;
    margin-bottom: var(--sp-4);
  }

  .items {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .nav-item {
    position: relative;
    width: 38px;
    height: 38px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-m);
    color: var(--clr-text-3);
    transition:
      background var(--t-fast) var(--ease-out),
      color var(--t-fast) var(--ease-out);
  }

  .badge {
    position: absolute;
    top: 2px;
    right: 2px;
    min-width: 14px;
    height: 14px;
    display: grid;
    place-items: center;
    padding: 0 3px;
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

  .nav-item.active {
    background: var(--clr-accent-dim);
    color: var(--clr-accent-strong);
  }

  /* The "you are here" tick at the window edge, so the active section
     reads even in the periphery. */
  .nav-item.active::before {
    content: "";
    position: absolute;
    left: -10px;
    top: 50%;
    transform: translateY(-50%);
    width: 3px;
    height: 18px;
    border-radius: 0 3px 3px 0;
    background: var(--clr-accent);
  }

  .nav-item:disabled {
    opacity: 0.35;
    cursor: default;
  }

  .spacer {
    flex: 1;
  }

  .foot {
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }
</style>
