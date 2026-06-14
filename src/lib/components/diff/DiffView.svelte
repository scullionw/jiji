<script lang="ts">
  import { tick, untrack } from "svelte";
  import { SvelteSet } from "svelte/reactivity";
  import EmptyState from "$lib/components/ui/EmptyState.svelte";
  import type { ChangeDiff } from "$lib/bindings/ChangeDiff";
  import type { GraphNode } from "$lib/bindings/GraphNode";
  import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
  import * as api from "$lib/api";
  import {
    resolveCompareFrom,
    type CompareMode,
  } from "$lib/components/inspector/inspect";
  import ChangeHeader from "./ChangeHeader.svelte";
  import FileDiffCard from "./FileDiffCard.svelte";
  import type { DiffLayout } from "./diff";

  let {
    snapshot,
    node,
    onjump,
    onclose,
  }: {
    snapshot: RepoSnapshot;
    node: GraphNode;
    onjump: (id: string) => void;
    onclose: () => void;
  } = $props();

  // What the diff is measured against. The mode is relative (trunk, stack
  // base) rather than a pinned id, so walking the stack keeps the same
  // comparison; it resolves to a concrete from-change per selection, or
  // null for the plain parent diff. Transient reading state — it lives
  // only as long as something is selected.
  let compare = $state<CompareMode>({ kind: "parent" });
  const compareFrom = $derived(resolveCompareFrom(snapshot, node.id, compare));

  // The diff arrives lazily; the header renders from the snapshot
  // immediately. A stale diff stays on screen while the same comparison
  // refetches (snapshot refresh) so the surface does not flash. The stale
  // dim only appears when a refetch is actually slow — auto-refresh
  // refetches constantly while the user edits files, and dimming each one
  // would pulse the surface.
  let diff = $state<ChangeDiff | null>(null);
  let error = $state<string | null>(null);
  let slow = $state(false);
  let requestSeq = 0;

  $effect(() => {
    const id = node.id;
    const from = compareFrom;
    void snapshot; // refetch when a new snapshot lands
    const token = ++requestSeq;
    error = null;
    const slowTimer = setTimeout(() => {
      if (token === requestSeq) slow = true;
    }, 200);
    (from ? api.compareDiff(from, id) : api.changeDiff(id)).then(
      (result) => {
        if (token !== requestSeq) return;
        clearTimeout(slowTimer);
        diff = result;
        slow = false;
      },
      (err) => {
        if (token !== requestSeq) return;
        clearTimeout(slowTimer);
        error = api.errorMessage(err);
        slow = false;
      },
    );
    return () => clearTimeout(slowTimer);
  });

  const current = $derived(
    diff && diff.id === node.id && (diff.from ?? null) === compareFrom
      ? diff
      : null,
  );

  // Unified vs side-by-side is a reading preference, not per-change state;
  // it persists across selections and sessions.
  const LAYOUT_KEY = "jiji.diff.layout";
  let layout = $state<DiffLayout>(
    localStorage.getItem(LAYOUT_KEY) === "split" ? "split" : "unified",
  );

  function setLayout(next: DiffLayout) {
    layout = next;
    localStorage.setItem(LAYOUT_KEY, next);
  }

  let scroller: HTMLDivElement | undefined;

  // Reading position and per-file collapse are per-comparison; selecting
  // another change or switching what it is measured against starts at the
  // top with every file expanded.
  const collapsedFiles = new SvelteSet<string>();

  $effect(() => {
    void node.id;
    void compareFrom;
    untrack(() => collapsedFiles.clear());
    scroller?.scrollTo({ top: 0 });
  });

  // Collapsing or expanding moves everything below it; the cards window
  // their rows from scroll geometry, so tell them it changed.
  function rewindowCards() {
    void tick().then(() => scroller?.dispatchEvent(new Event("scroll")));
  }

  function toggleFile(path: string) {
    if (!collapsedFiles.delete(path)) collapsedFiles.add(path);
    rewindowCards();
  }

  async function jumpToFile(index: number) {
    const path = current?.files[index]?.path;
    if (path && collapsedFiles.delete(path)) await tick();
    scroller
      ?.querySelector(`[data-file-index="${index}"]`)
      ?.scrollIntoView({ block: "start" });
    rewindowCards();
  }
</script>

<!-- The continuous multi-file diff for the current selection: every changed
     file in one scroll, with the file list as navigation, not a gate. -->
<div class="diff-view">
  <ChangeHeader
    {snapshot}
    {node}
    files={current?.files ?? null}
    {layout}
    onlayout={setLayout}
    {compare}
    {compareFrom}
    oncompare={(mode) => (compare = mode)}
    {onjump}
    onjumpfile={jumpToFile}
    {onclose}
  />
  <div class="scroller" bind:this={scroller} class:stale={slow && current}>
    {#if current}
      {#if current.files.length === 0}
        <div class="empty">
          <EmptyState
            icon="commit"
            title="No file changes"
            body={compareFrom
              ? "Nothing differs between these two changes."
              : node.isEmpty
                ? "This change is empty — nothing differs from its parent."
                : "Nothing differs from the parent tree."}
          />
        </div>
      {:else}
        {#each current.files as file, index (file.path)}
          <FileDiffCard
            {file}
            {index}
            {layout}
            collapsed={collapsedFiles.has(file.path)}
            ontoggle={() => toggleFile(file.path)}
          />
        {/each}
        {#if current.truncated}
          <p class="trailing-note">
            File list truncated — this change touches more files than Jiji renders
          </p>
        {/if}
      {/if}
    {:else if error}
      <p class="error">{error}</p>
    {:else}
      <div class="skeleton" aria-hidden="true">
        <i></i><i></i><i></i><i></i><i></i>
      </div>
    {/if}
  </div>
</div>

<style>
  .diff-view {
    height: 100%;
    display: flex;
    flex-direction: column;
    min-width: 0;
    overflow: hidden;
  }

  .scroller {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    transition: opacity var(--t-fast) var(--ease-out);
  }

  .scroller.stale {
    opacity: 0.7;
  }

  .empty {
    height: 100%;
    display: grid;
    place-items: center;
  }

  .trailing-note {
    padding: var(--sp-3) var(--sp-4) var(--sp-6);
    font-size: var(--text-s);
    color: var(--clr-text-3);
    font-style: italic;
  }

  .error {
    padding: var(--sp-4);
    font-size: var(--text-s);
    color: var(--clr-warn);
  }

  .skeleton {
    display: flex;
    flex-direction: column;
    gap: 9px;
    padding: var(--sp-4);
  }

  .skeleton i {
    height: 12px;
    border-radius: 4px;
    background: var(--clr-bg-3);
    animation: pulse 1.1s ease-in-out infinite alternate;
  }

  .skeleton i:nth-child(2) {
    width: 86%;
    animation-delay: 100ms;
  }

  .skeleton i:nth-child(3) {
    width: 64%;
    animation-delay: 200ms;
  }

  .skeleton i:nth-child(4) {
    width: 78%;
    animation-delay: 300ms;
  }

  .skeleton i:nth-child(5) {
    width: 42%;
    animation-delay: 400ms;
  }

  @keyframes pulse {
    from {
      opacity: 0.45;
    }
    to {
      opacity: 1;
    }
  }
</style>
