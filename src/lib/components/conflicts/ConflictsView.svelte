<script lang="ts">
  import EmptyState from "$lib/components/ui/EmptyState.svelte";
  import SectionHeader from "$lib/components/ui/SectionHeader.svelte";
  import { app } from "$lib/state/app.svelte";
  import ConflictRow from "./ConflictRow.svelte";
  import { groupConflicts } from "./conflicts";

  const snapshot = $derived(app.snapshot!);
  const groups = $derived(groupConflicts(snapshot));
</script>

{#if groups.length === 0}
  <EmptyState
    icon="conflicts"
    title="No conflicts"
    body="Rewrites and syncs that collide land here as first-class items instead of blocking your work. Nothing needs attention right now."
    hint="everything applies cleanly"
  />
{:else}
  <div class="view">
    <div class="column">
      <SectionHeader
        icon="conflicts"
        title="Conflicts"
        description="Everything that needs attention, in plain language. jj never blocks on a conflict: rewrites and syncs always complete, what collided is recorded first-class, and other work can continue in the meantime."
      />

      {#each groups as group (group.key)}
        <section class="group" data-conflict-group={group.key}>
          <div class="group-head">
            <span class="group-label">{group.title}</span>
            <span class="group-count mono">{group.items.length}</span>
          </div>
          <p class="group-blurb">{group.blurb}</p>
          {#each group.items as item (item.id)}
            <ConflictRow {item} />
          {/each}
        </section>
      {/each}
    </div>
  </div>
{/if}

<style>
  .view {
    height: 100%;
    overflow-y: auto;
  }

  .column {
    max-width: 800px;
    margin-inline: auto;
    padding: var(--sp-6) var(--sp-6) var(--sp-8);
  }

  .group {
    margin-bottom: var(--sp-6);
  }

  .group-head {
    position: sticky;
    top: 0;
    z-index: 2;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-3) 0 var(--sp-1);
    background: var(--clr-bg-1);
  }

  .group-label {
    font-size: var(--text-s);
    font-weight: 600;
    color: var(--clr-text-2);
  }

  .group-count {
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .group-blurb {
    font-size: var(--text-s);
    color: var(--clr-text-3);
    padding-bottom: var(--sp-2);
    max-width: 58em;
  }
</style>
