<script lang="ts">
  import { untrack } from "svelte";
  import { SvelteSet } from "svelte/reactivity";
  import type { FileDiff } from "$lib/bindings/FileDiff";
  import type { FileStatus } from "$lib/bindings/FileStatus";
  import Icon from "$lib/components/ui/Icon.svelte";
  import { splitPath } from "$lib/components/inspector/inspect";
  import {
    chunkRows,
    fileStats,
    gutterDigits,
    maxLineCols,
    splitLayoutRows,
    unifiedRows,
    type DiffLayout,
    type SplitCell,
  } from "./diff";

  let {
    file,
    index,
    layout = "unified",
    collapsed = false,
    ontoggle,
  }: {
    file: FileDiff;
    index: number;
    layout?: DiffLayout;
    collapsed?: boolean;
    ontoggle?: () => void;
  } = $props();

  const STATUS_GLYPH: Record<FileStatus, string> = {
    added: "A",
    modified: "M",
    removed: "D",
    renamed: "R",
    copied: "C",
  };

  // The body renders in blocks of rows so a 20k-line diff never has more
  // than a couple of viewports of real DOM: blocks outside the overscan
  // margin are a single placeholder div of the exact same height (every
  // row is exactly --row-h tall), so scroll geometry never shifts as
  // blocks mount and unmount. ~64 rows is ~1.2k px — one block mounts in
  // well under a frame. The window is recomputed synchronously from the
  // scroll position (fixed row heights make it arithmetic), so content is
  // always in place when the frame paints — no observer latency, no blank
  // flashes mid-drag.
  const BLOCK_ROWS = 64;
  const OVERSCAN_PX = 1200;

  const parts = $derived(splitPath(file.path));
  const stats = $derived(fileStats(file));
  const text = $derived(file.content.kind === "text" ? file.content : null);
  const digits = $derived(text ? gutterDigits(text.hunks) : 2);
  const cols = $derived(text ? maxLineCols(text.hunks) : 0);

  // Lazy per-layout: only the active layout's rows are ever computed.
  const uniBlocks = $derived(
    text && layout === "unified"
      ? chunkRows(unifiedRows(text.hunks), BLOCK_ROWS)
      : [],
  );
  const splBlocks = $derived(
    text && layout === "split"
      ? chunkRows(splitLayoutRows(text.hunks), BLOCK_ROWS)
      : [],
  );

  // Block indexes currently carrying real rows rather than a placeholder.
  const mounted = new SvelteSet<number>();
  let linesEl = $state<HTMLDivElement>();

  function updateWindow(): void {
    const el = linesEl;
    if (!el) return;
    const blocks = layout === "split" ? splBlocks : uniBlocks;
    if (blocks.length === 0) {
      mounted.clear();
      return;
    }
    // Block offsets never move: placeholders are pixel-identical to their
    // mounted height, so one rect read maps the viewport onto block
    // indexes. line-height of .lines is --row-h; engines report numeric
    // line-heights as px, but a raw multiple of font-size also parses.
    const style = getComputedStyle(el);
    const lineHeight = parseFloat(style.lineHeight);
    const rowPx = style.lineHeight.endsWith("px")
      ? lineHeight
      : lineHeight * parseFloat(style.fontSize);
    const blockPx = BLOCK_ROWS * rowPx;
    const scroller = el.closest(".scroller");
    const view = scroller
      ? scroller.getBoundingClientRect()
      : { top: 0, bottom: window.innerHeight };
    const bodyTop = el.getBoundingClientRect().top;
    const first = Math.max(
      0,
      Math.floor((view.top - OVERSCAN_PX - bodyTop) / blockPx),
    );
    const last = Math.min(
      blocks.length - 1,
      Math.floor((view.bottom + OVERSCAN_PX - bodyTop) / blockPx),
    );
    for (const i of mounted) if (i < first || i > last) mounted.delete(i);
    for (let i = first; i <= last; i++) mounted.add(i);
  }

  // Re-window when the body first exists and whenever its content reshapes
  // (new diff, layout toggle). untrack: updateWindow reads and writes
  // `mounted`, which must not feed back into this effect.
  $effect(() => {
    void uniBlocks;
    void splBlocks;
    if (!linesEl) return;
    untrack(updateWindow);
  });

  // Scrolling and resizing move the viewport over the (static) blocks.
  $effect(() => {
    const scroller = linesEl?.closest(".scroller");
    if (!scroller) return;
    const onMove = () => untrack(updateWindow);
    scroller.addEventListener("scroll", onMove, { passive: true });
    window.addEventListener("resize", onMove);
    return () => {
      scroller.removeEventListener("scroll", onMove);
      window.removeEventListener("resize", onMove);
    };
  });
</script>

<!-- One side of an aligned split row: gutter cell plus code cell, or a
     hatched absent pair when the row only exists on the other side. -->
{#snippet half(cell: SplitCell | null, side: "l" | "r")}
  {#if cell}
    <span class="no {side} {cell.kind}">{cell.no}</span><span
      class="code selectable {side} {cell.kind}"
      class:intraline={cell.intraline}
    >{#each cell.segments as segment}{#if segment.changed}<mark
        >{segment.text}</mark>{:else}{segment.text}{/if}{/each}</span>
  {:else}
    <span class="no {side} absent"></span><span class="code {side} absent"></span>
  {/if}
{/snippet}

<!-- One file of the continuous diff. The header stays stuck below the
     change header while the file's lines scroll by, and doubles as the
     collapse toggle for the file's body. -->
<section class="file" data-file-index={index} data-path={file.path}>
  <button
    type="button"
    class="file-head"
    aria-expanded={!collapsed}
    onclick={ontoggle}
  >
    <span class="chev" class:open={!collapsed}>
      <Icon name="chevronRight" size={12} />
    </span>
    <span class="status {file.status}">{STATUS_GLYPH[file.status]}</span>
    <span
      class="path mono"
      title={file.renamedFrom ? `${file.renamedFrom} → ${file.path}` : file.path}
    >
      {#if parts.dir}<span class="dir">{parts.dir}</span>{/if}<span
        class="fname"
        class:gone={file.status === "removed"}>{parts.name}</span>
    </span>
    {#if file.renamedFrom}
      <span class="from mono truncate">← {file.renamedFrom}</span>
    {/if}
    {#if file.hasConflict}
      <span class="conflict-chip">conflict</span>
    {/if}
    {#if stats.added > 0 || stats.removed > 0}
      <span class="stats mono">
        {#if stats.added > 0}<span class="add">+{stats.added}</span>{/if}
        {#if stats.removed > 0}<span class="del">−{stats.removed}</span>{/if}
      </span>
    {/if}
  </button>

  {#if !collapsed}
    {#if text}
    {#if text.hunks.length === 0}
      <p class="note">No content changes</p>
    {:else if layout === "split"}
      <div
        class="lines mono split"
        bind:this={linesEl}
        style:--numw="{digits + 1}ch"
        style:--codew="{cols + 1}ch"
      >
        {#each splBlocks as rows, blockIndex}
          <div
            class="split-grid"
            style:height={mounted.has(blockIndex)
              ? null
              : `calc(${rows.length} * var(--row-h))`}
          >
            {#if mounted.has(blockIndex)}
              {#each rows as row}
                {#if "gap" in row}
                  <div class="gap split-gap" aria-label="{row.gap} unchanged lines">
                    <span>{row.gap} unchanged {row.gap === 1 ? "line" : "lines"}</span>
                    <i></i>
                  </div>
                {:else}
                  {@render half(row.left, "l")}{@render half(row.right, "r")}
                {/if}
              {/each}
            {/if}
          </div>
        {/each}
        {#if text.truncated}
          <p class="note">Diff truncated — this file changes more lines than Jiji renders</p>
        {/if}
      </div>
    {:else}
      <div
        class="lines mono"
        bind:this={linesEl}
        style:--numw="{digits + 1}ch"
        style:--codew="{cols + 1}ch"
      >
        {#each uniBlocks as rows, blockIndex}
          <div
            class="block"
            style:height={mounted.has(blockIndex)
              ? null
              : `calc(${rows.length} * var(--row-h))`}
          >
            {#if mounted.has(blockIndex)}
              {#each rows as row}
                {#if "gap" in row}
                  <div class="gap" aria-label="{row.gap} unchanged lines">
                    <i></i>
                    <span>{row.gap} unchanged {row.gap === 1 ? "line" : "lines"}</span>
                    <i></i>
                  </div>
                {:else}
                  <div class="line {row.kind}" class:intraline={row.intraline}>
                    <span class="no">{row.oldNo ?? ""}</span><span class="no"
                    >{row.newNo ?? ""}</span><span class="code selectable"
                    >{#each row.segments as segment}{#if segment.changed}<mark
                        >{segment.text}</mark>{:else}{segment.text}{/if}{/each}</span>
                  </div>
                {/if}
              {/each}
            {/if}
          </div>
        {/each}
        {#if text.truncated}
          <p class="note">Diff truncated — this file changes more lines than Jiji renders</p>
        {/if}
      </div>
    {/if}
    {:else if file.content.kind === "binary"}
      <p class="note">Binary file — no text diff</p>
    {:else if file.content.kind === "tooLarge"}
      <p class="note">File too large to diff</p>
    {:else}
      <p class="note">Not rendered — this change is too big to show every file</p>
    {/if}
  {/if}
</section>

<style>
  .file {
    border-bottom: 1px solid var(--clr-border-2);
    /* Jumping here from the file menu lands the header below the sticky
       change header, not underneath it. */
    scroll-margin-top: 1px;
  }

  .file:last-of-type {
    border-bottom: none;
  }

  .file-head {
    position: sticky;
    top: 0;
    z-index: 2;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    min-width: 0;
    padding: 5px var(--sp-4) 5px var(--sp-3);
    background: var(--clr-bg-1);
    border-bottom: 1px solid var(--clr-border-2);
    box-shadow: var(--shadow-edge);
    text-align: left;
  }

  .file-head:hover {
    background: var(--clr-bg-2);
  }

  .chev {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    color: var(--clr-text-3);
    transition: transform var(--t-fast) var(--ease-out);
  }

  .chev.open {
    transform: rotate(90deg);
  }

  .file-head:hover .chev {
    color: var(--clr-text-2);
  }

  .status {
    flex-shrink: 0;
    width: 15px;
    height: 15px;
    display: grid;
    place-items: center;
    border-radius: 4px;
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    font-weight: 600;
  }

  .status.added {
    color: var(--clr-ok);
    background: color-mix(in srgb, var(--clr-ok) 12%, transparent);
  }

  .status.modified {
    color: var(--clr-warn);
    background: color-mix(in srgb, var(--clr-warn) 12%, transparent);
  }

  .status.removed {
    color: var(--clr-danger);
    background: color-mix(in srgb, var(--clr-danger) 12%, transparent);
  }

  .status.renamed,
  .status.copied {
    color: var(--clr-accent);
    background: color-mix(in srgb, var(--clr-accent) 12%, transparent);
  }

  /* Where a renamed file came from; the destination path keeps priority
     when space runs out. */
  .from {
    flex: 0 10000 auto;
    min-width: 0;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .path {
    display: flex;
    min-width: 0;
    font-size: var(--text-s);
    white-space: nowrap;
  }

  /* The directory gives way long before the file name does. */
  .dir {
    flex: 0 1000 auto;
    min-width: 3ch;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--clr-text-3);
  }

  .fname {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--clr-text-1);
    font-weight: 500;
  }

  .fname.gone {
    text-decoration: line-through;
    color: var(--clr-text-2);
  }

  .conflict-chip {
    flex-shrink: 0;
    font-size: var(--text-xs);
    border-radius: 999px;
    padding: 1px 7px;
    background: color-mix(in srgb, var(--clr-danger) 14%, transparent);
    color: var(--clr-danger);
  }

  .stats {
    flex-shrink: 0;
    margin-left: auto;
    display: flex;
    gap: var(--sp-2);
    font-size: var(--text-xs);
  }

  .stats .add {
    color: var(--clr-ok);
  }

  .stats .del {
    color: var(--clr-danger);
  }

  .note {
    padding: var(--sp-2) var(--sp-4);
    font-size: var(--text-s);
    color: var(--clr-text-3);
    font-style: italic;
  }

  /* Every row is exactly --row-h tall, so an unmounted block's placeholder
     height (rows × --row-h, set inline) matches its mounted height to the
     pixel. --codew is the measured widest line; sizing the code column
     from it (instead of max-content) keeps layout cost independent of row
     count and keeps every block's columns aligned. */
  .lines {
    --row-h: calc(var(--text-code) * var(--lh-code));
    overflow-x: auto;
    font-size: var(--text-code);
    line-height: var(--lh-code);
    padding: 3px 0;
  }

  .block {
    min-width: calc(2 * var(--numw) + var(--codew) + var(--sp-2) + var(--sp-4));
  }

  .line {
    display: grid;
    grid-template-columns: var(--numw) var(--numw) 1fr;
    width: 100%;
    height: var(--row-h);
  }

  .no {
    text-align: right;
    padding-right: 1ch;
    color: var(--clr-text-3);
    opacity: 0.75;
    user-select: none;
    font-size: var(--text-xs);
    line-height: inherit;
  }

  .code {
    white-space: pre;
    padding: 0 var(--sp-4) 0 var(--sp-2);
    tab-size: 4;
  }

  .code mark {
    background: transparent;
    color: inherit;
    border-radius: 2px;
  }

  .line.added {
    background: color-mix(in srgb, var(--clr-ok) 15%, transparent);
  }

  .line.added .no {
    color: color-mix(in srgb, var(--clr-ok) 85%, var(--clr-text-3));
    opacity: 1;
  }

  .line.removed {
    background: color-mix(in srgb, var(--clr-danger) 15%, transparent);
  }

  .line.removed .no {
    color: color-mix(in srgb, var(--clr-danger) 80%, var(--clr-text-3));
    opacity: 1;
  }

  /* Word-level emphasis only where a line has a changed/unchanged mix;
     whole added or removed lines stay a flat tint. */
  .line.added.intraline mark {
    background: color-mix(in srgb, var(--clr-ok) 35%, transparent);
  }

  .line.removed.intraline mark {
    background: color-mix(in srgb, var(--clr-danger) 35%, transparent);
  }

  .gap {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    height: var(--row-h);
    padding: 0 var(--sp-4);
    user-select: none;
  }

  .gap span {
    flex-shrink: 0;
    font-family: var(--font-ui);
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .gap i {
    height: 1px;
    flex: 1;
    background: var(--clr-border-2);
  }

  /* Side-by-side: every block renders its own grid, but the tracks are
     fixed (gutters from --numw, code columns at least half the pane and
     exactly --codew when lines run wider), so the center divider lines up
     across blocks. Each half scrolls as one unit with the gutters. */
  .lines.split {
    container-type: inline-size;
  }

  .split-grid {
    display: grid;
    grid-template-columns:
      var(--numw)
      max(calc(50cqw - var(--numw)), calc(var(--codew) + var(--sp-2) + var(--sp-4)))
      var(--numw)
      max(calc(50cqw - var(--numw)), calc(var(--codew) + var(--sp-2) + var(--sp-4)));
    width: max-content;
    min-width: 100%;
  }

  .split-grid .no,
  .split-grid .code {
    height: var(--row-h);
  }

  .split-grid .no.r {
    border-left: 1px solid var(--clr-border-2);
  }

  .split-grid .added {
    background: color-mix(in srgb, var(--clr-ok) 15%, transparent);
  }

  .split-grid .removed {
    background: color-mix(in srgb, var(--clr-danger) 15%, transparent);
  }

  .split-grid .no.added {
    color: color-mix(in srgb, var(--clr-ok) 85%, var(--clr-text-3));
    opacity: 1;
  }

  .split-grid .no.removed {
    color: color-mix(in srgb, var(--clr-danger) 80%, var(--clr-text-3));
    opacity: 1;
  }

  .split-grid .code.added.intraline mark {
    background: color-mix(in srgb, var(--clr-ok) 35%, transparent);
  }

  .split-grid .code.removed.intraline mark {
    background: color-mix(in srgb, var(--clr-danger) 35%, transparent);
  }

  /* A row with no counterpart on this side reads as "nothing here", not as
     an empty line. */
  .split-grid .absent {
    background: repeating-linear-gradient(
      -45deg,
      transparent 0 5px,
      var(--clr-bg-hover) 5px 8px
    );
  }

  .split-gap {
    grid-column: 1 / -1;
  }

  /* The grid can be wider than the pane; keep the gap label in view. */
  .split-gap span {
    position: sticky;
    left: var(--sp-4);
  }
</style>
