<script lang="ts">
  import { tick } from "svelte";
  import { SvelteSet } from "svelte/reactivity";
  import Icon from "$lib/components/ui/Icon.svelte";
  import type { FileDiff } from "$lib/bindings/FileDiff";
  import type { FileStatus } from "$lib/bindings/FileStatus";
  import {
    activeRowKey,
    buildTreeRows,
    visibleTreeRows,
    type TreeRow,
  } from "./tree";

  let {
    files,
    activeIndex,
    onpick,
  }: {
    files: FileDiff[];
    /// Index into `files` of the file the diff is scrolled to; the tree
    /// tracks it like a reading ribbon.
    activeIndex: number;
    /// Scroll the diff to one file section.
    onpick: (index: number) => void;
  } = $props();

  const STATUS_GLYPH: Record<FileStatus, string> = {
    added: "A",
    modified: "M",
    removed: "D",
    renamed: "R",
    copied: "C",
  };

  const rows = $derived(buildTreeRows(files));
  const collapsed = new SvelteSet<string>();
  const visible = $derived(visibleTreeRows(rows, collapsed));
  const activePath = $derived(files[activeIndex]?.path ?? "");
  const active = $derived(activeRowKey(rows, collapsed, activePath));

  let treeEl = $state<HTMLDivElement | undefined>();

  // Roving tabindex: one row is in the tab order — the last row the
  // keyboard visited, else the active one, so tabbing in lands where the
  // diff is. Arrows move focus from there.
  let cursor = $state<string | null>(null);
  const tabbable = $derived(
    cursor !== null && visible.some((row) => row.path === cursor)
      ? cursor
      : active,
  );

  function toggleDir(path: string) {
    if (!collapsed.delete(path)) collapsed.add(path);
  }

  function focusRow(row: TreeRow) {
    cursor = row.path;
    void tick().then(() =>
      treeEl
        ?.querySelector<HTMLElement>(
          `[data-tree-row="${CSS.escape(row.path)}"]`,
        )
        ?.focus(),
    );
  }

  /// The row a Left-arrow retreats to: the nearest earlier row one level up.
  function parentIndex(index: number): number | null {
    const depth = visible[index].depth;
    for (let i = index - 1; i >= 0; i--) {
      if (visible[i].depth < depth) return i;
    }
    return null;
  }

  // The WAI-ARIA tree contract, minus type-ahead. stopPropagation keeps
  // ArrowUp/Down from also walking the graph selection (a window-level
  // binding).
  function onRowKeydown(event: KeyboardEvent, index: number) {
    const row = visible[index];
    let target: number | null = null;
    switch (event.key) {
      case "ArrowDown":
        target = Math.min(visible.length - 1, index + 1);
        break;
      case "ArrowUp":
        target = Math.max(0, index - 1);
        break;
      case "Home":
        target = 0;
        break;
      case "End":
        target = visible.length - 1;
        break;
      case "ArrowLeft":
        if (row.kind === "dir" && !collapsed.has(row.path)) {
          collapsed.add(row.path);
        } else {
          target = parentIndex(index);
        }
        break;
      case "ArrowRight":
        if (row.kind === "dir" && collapsed.has(row.path)) {
          collapsed.delete(row.path);
        } else if (row.kind === "dir") {
          target = Math.min(visible.length - 1, index + 1);
        }
        break;
      default:
        return;
    }
    event.preventDefault();
    event.stopPropagation();
    if (target !== null && target !== index) focusRow(visible[target]);
  }

  // Follow the diff's reading position: as scrolling moves the active row,
  // keep it in the tree's viewport. block: nearest, so it never yanks the
  // tree around while the row is already visible.
  $effect(() => {
    const key = active;
    if (!treeEl) return;
    void tick().then(() =>
      treeEl
        ?.querySelector(`[data-tree-row="${CSS.escape(key)}"]`)
        ?.scrollIntoView({ block: "nearest" }),
    );
  });

  function fileTitle(row: TreeRow & { kind: "file" }): string {
    const path = row.renamedFrom
      ? `${row.renamedFrom} → ${row.path}`
      : row.path;
    const stats =
      row.stats.added > 0 || row.stats.removed > 0
        ? ` · +${row.stats.added} −${row.stats.removed}`
        : "";
    return path + stats;
  }
</script>

<!-- The changed files as a directory tree: the diff's map and its
     navigation. Rows mirror the scroller — the file under the reading
     line carries the accent — and directories fold their subtree into an
     aggregate. -->
<div class="tree" bind:this={treeEl} role="tree" aria-label="Changed files">
  {#each visible as row, index (row.path)}
    {#if row.kind === "dir"}
      {@const open = !collapsed.has(row.path)}
      <button
        type="button"
        class="row dir"
        class:active={active === row.path}
        role="treeitem"
        aria-expanded={open}
        aria-level={row.depth + 1}
        aria-selected={active === row.path}
        tabindex={tabbable === row.path ? 0 : -1}
        data-tree-row={row.path}
        title={row.path}
        onclick={() => toggleDir(row.path)}
        onfocus={() => (cursor = row.path)}
        onkeydown={(event) => onRowKeydown(event, index)}
      >
        <span class="guides" aria-hidden="true">
          {#each { length: row.depth } as _, i (i)}<i></i>{/each}
        </span>
        <span class="chev" class:open>
          <Icon name="chevronRight" size={11} />
        </span>
        <span class="label mono truncate">{row.label}</span>
        {#if !open}
          {#if row.hasConflict}
            <i class="conflict-dot" title="contains conflicts"></i>
          {/if}
          <span class="summary mono">
            <span class="count">{row.fileCount}</span>
            {#if row.stats.added > 0}<span class="add">+{row.stats.added}</span
              >{/if}
            {#if row.stats.removed > 0}<span class="del"
                >−{row.stats.removed}</span
              >{/if}
          </span>
        {/if}
      </button>
    {:else}
      <!-- data-path + data-file-index make the row a context-menu diff
           target (the file's verbs: open, copy, annotate, discard), same
           as right-clicking the file's card in the scroller. -->
      <button
        type="button"
        class="row file"
        class:active={active === row.path}
        role="treeitem"
        aria-level={row.depth + 1}
        aria-selected={active === row.path}
        tabindex={tabbable === row.path ? 0 : -1}
        data-tree-row={row.path}
        data-path={row.path}
        data-file-index={row.index}
        title={fileTitle(row)}
        onclick={() => onpick(row.index)}
        onfocus={() => (cursor = row.path)}
        onkeydown={(event) => onRowKeydown(event, index)}
      >
        <span class="guides" aria-hidden="true">
          {#each { length: row.depth } as _, i (i)}<i></i>{/each}
        </span>
        <span class="glyph {row.status} mono">{STATUS_GLYPH[row.status]}</span>
        <span class="label name mono truncate" class:gone={row.status === "removed"}
          >{row.name}</span>
        {#if row.hasConflict}
          <i class="conflict-dot" title="conflict"></i>
        {/if}
        {#if row.stats.added > 0 || row.stats.removed > 0}
          <span class="summary mono">
            {#if row.stats.added > 0}<span class="add">+{row.stats.added}</span
              >{/if}
            {#if row.stats.removed > 0}<span class="del"
                >−{row.stats.removed}</span
              >{/if}
          </span>
        {/if}
      </button>
    {/if}
  {/each}
</div>

<style>
  .tree {
    height: 100%;
    overflow-y: auto;
    overflow-x: hidden;
    padding: var(--sp-1) var(--sp-1) var(--sp-6);
  }

  /* Rows tile with no vertical gaps so the indent guides read as
     continuous threads. */
  .row {
    position: relative;
    display: flex;
    align-items: center;
    width: 100%;
    height: 22px;
    padding: 0 var(--sp-2) 0 var(--sp-1);
    border-radius: var(--radius-s);
    font-size: var(--text-s);
    text-align: left;
    transition:
      background var(--t-fast) var(--ease-out),
      color var(--t-fast) var(--ease-out);
  }

  .row:hover {
    background: var(--clr-bg-hover);
  }

  /* The reading ribbon: where the diff is scrolled to right now. */
  .row.active {
    background: var(--clr-accent-dim);
  }

  /* One 14px cell per ancestor level, each threading a hairline under its
     parent's chevron. */
  .guides {
    flex-shrink: 0;
    display: flex;
    align-self: stretch;
  }

  .guides i {
    position: relative;
    width: 14px;
  }

  .guides i::before {
    content: "";
    position: absolute;
    left: 7px;
    top: 0;
    bottom: 0;
    width: 1px;
    background: var(--clr-border-2);
  }

  .chev {
    flex-shrink: 0;
    width: 14px;
    display: grid;
    place-items: center;
    color: var(--clr-text-3);
    transition: transform var(--t-fast) var(--ease-out);
  }

  .chev.open {
    transform: rotate(90deg);
  }

  .row:hover .chev {
    color: var(--clr-text-2);
  }

  /* Same cell width as the chevron, so file names align with directory
     labels at the same depth. */
  .glyph {
    flex-shrink: 0;
    width: 14px;
    text-align: center;
    font-size: var(--text-xs);
    font-weight: 600;
  }

  .glyph.added {
    color: var(--diff-add-fg);
  }

  .glyph.modified {
    color: var(--clr-warn);
  }

  .glyph.removed {
    color: var(--diff-del-fg);
  }

  .glyph.renamed,
  .glyph.copied {
    color: var(--clr-accent-strong);
  }

  .label {
    margin-left: 4px;
    min-width: 0;
    color: var(--clr-text-2);
  }

  .label.name {
    color: var(--clr-text-1);
    font-weight: 400;
  }

  .label.gone {
    text-decoration: line-through;
    color: var(--clr-text-2);
  }

  .conflict-dot {
    flex-shrink: 0;
    width: 5px;
    height: 5px;
    margin-left: 6px;
    border-radius: 50%;
    background: var(--clr-danger);
  }

  .summary {
    flex-shrink: 0;
    margin-left: auto;
    padding-left: var(--sp-2);
    display: flex;
    gap: 6px;
    font-size: var(--text-xs);
  }

  .count {
    color: var(--clr-text-3);
  }

  .add {
    color: var(--diff-add-fg);
  }

  .del {
    color: var(--diff-del-fg);
  }
</style>
