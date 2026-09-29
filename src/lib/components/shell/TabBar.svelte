<script lang="ts">
  import Icon from "$lib/components/ui/Icon.svelte";
  import { activateTab, closeTab, newTab } from "$lib/state/actions";
  import { tabTitles, tabs } from "$lib/state/tabs.svelte";

  const titles = $derived(tabTitles(tabs.all));
</script>

<!-- The open repos, one tab each — living in the title bar's flexible
     middle, so the strip's empty space keeps dragging the window. -->
<div class="tabbar" data-tauri-drag-region>
  <div
    class="strip"
    role="tablist"
    aria-label="Open repositories"
    data-tauri-drag-region
  >
    {#each tabs.all as tab (tab.id)}
      {@const title = titles.get(tab.id)!}
      <div
        class="tab"
        class:active={tab.id === tabs.activeId}
        class:blank={tab.path === null}
        role="tab"
        aria-selected={tab.id === tabs.activeId}
        onauxclick={(event) => {
          if (event.button === 1) closeTab(tab.id);
        }}
      >
        <button
          class="label"
          title={tab.path ?? "New tab"}
          onclick={() => void activateTab(tab.id)}
        >
          <span class="truncate">{title.label}{#if title.hint}<span class="hint">{title.hint}</span>{/if}</span>
        </button>
        <button
          class="close"
          title="Close tab (⌘W)"
          aria-label="Close {title.label}{title.hint ? ` (${title.hint})` : ''}"
          onclick={() => closeTab(tab.id)}
        >
          <Icon name="close" size={10} />
        </button>
      </div>
    {/each}
    <button class="new-tab" title="New tab (⌘T)" aria-label="New tab" onclick={newTab}>
      <Icon name="plus" size={13} />
    </button>
  </div>
</div>

<style>
  .tabbar {
    flex: 1;
    min-width: 0;
    height: 100%;
    display: flex;
    align-items: center;
  }

  .strip {
    display: flex;
    align-items: center;
    gap: 2px;
    max-width: 100%;
    overflow-x: auto;
    scrollbar-width: none;
  }

  .strip::-webkit-scrollbar {
    display: none;
  }

  .tab {
    flex-shrink: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    height: 24px;
    padding: 0 3px 0 10px;
    border-radius: var(--radius-m);
    color: var(--clr-text-3);
    font-size: var(--text-s);
    font-weight: 500;
    white-space: nowrap;
    transition:
      background var(--t-fast) var(--ease-out),
      color var(--t-fast) var(--ease-out);
  }

  .tab:hover {
    color: var(--clr-text-2);
    background: var(--clr-bg-hover);
  }

  .tab.active {
    color: var(--clr-text-1);
    background: color-mix(in srgb, var(--clr-text-1) 8%, transparent);
  }

  .label {
    min-width: 0;
    max-width: 180px;
    display: flex;
    align-items: center;
    height: 100%;
    font-size: inherit;
    font-weight: inherit;
    color: inherit;
  }

  /* Same-named repos: the parent directories that tell them apart. */
  .hint {
    margin-left: 0.45em;
    font-weight: 400;
    color: var(--clr-text-3);
  }

  /* The close affordance keeps its footprint (no layout shift) and only
     surfaces on hover or on the active tab. */
  .close {
    flex-shrink: 0;
    width: 18px;
    height: 18px;
    display: grid;
    place-items: center;
    margin-left: 5px;
    border-radius: var(--radius-s);
    color: var(--clr-text-3);
    opacity: 0;
    transition:
      opacity var(--t-fast) var(--ease-out),
      background var(--t-fast) var(--ease-out),
      color var(--t-fast) var(--ease-out);
  }

  .tab:hover .close,
  .tab.active .close {
    opacity: 1;
  }

  .close:hover {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .new-tab {
    flex-shrink: 0;
    width: 22px;
    height: 22px;
    display: grid;
    place-items: center;
    margin-left: 2px;
    border-radius: var(--radius-m);
    color: var(--clr-text-3);
    transition:
      background var(--t-fast) var(--ease-out),
      color var(--t-fast) var(--ease-out);
  }

  .new-tab:hover {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }
</style>
