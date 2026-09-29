<script lang="ts">
  import { untrack } from "svelte";
  import { SvelteSet } from "svelte/reactivity";
  import type { DiffLine } from "$lib/bindings/DiffLine";
  import type { DiffSegment } from "$lib/bindings/DiffSegment";
  import type { FileDiff } from "$lib/bindings/FileDiff";
  import type { FileStatus } from "$lib/bindings/FileStatus";
  import type { ImageContent } from "$lib/bindings/ImageContent";
  import Icon from "$lib/components/ui/Icon.svelte";
  import { splitPath } from "$lib/components/inspector/inspect";
  import {
    binaryNote,
    chunkRows,
    conflictHunks,
    fileStats,
    formatBytes,
    gutterDigits,
    maxLineCh,
    splitLayoutRows,
    unifiedRows,
    type DiffLayout,
    type SplitCell,
  } from "./diff";
  import {
    locateRow,
    overlayMatches,
    rowOffsetPx,
    type FindMatch,
    type FindSpan,
  } from "./find";
  import { fileHighlighter, plainSpans, type HighlightSpan } from "./highlight";
  import { observeViewport, rowOffsets, visibleRange } from "$lib/viewport";

  let {
    file,
    index,
    layout = "unified",
    collapsed = false,
    ontoggle,
    resolve = null,
    onresolve,
    find = null,
  }: {
    file: FileDiff;
    index: number;
    layout?: DiffLayout;
    collapsed?: boolean;
    ontoggle?: () => void;
    /// Renders the Resolve action on a conflicted file's header: which
    /// external tool will open, whether this file is the one the tool is
    /// currently holding, and whether launching another is blocked.
    resolve?: { tool: string; waiting: boolean; disabled: boolean } | null;
    onresolve?: () => void;
    /// The find bar's matches in this file, in reading order. `active` is
    /// the index (in `matches`) of the diff-wide current match when it
    /// lives here, else -1; `seq` bumps on every navigation so revealing
    /// re-fires even when the target match is unchanged.
    find?: { matches: FindMatch[]; active: number; seq: number } | null;
  } = $props();

  // Unique per card: names the pannable code body for the scrollbar's
  // aria-controls.
  const uid = $props.id();

  const STATUS_GLYPH: Record<FileStatus, string> = {
    added: "A",
    modified: "M",
    removed: "D",
    renamed: "R",
    copied: "C",
  };

  // The body renders in blocks of rows so a 20k-line diff never has more
  // than a couple of viewports of real DOM: blocks outside the overscan
  // margin are a single placeholder div. Lines never wrap (long ones ride
  // the file's horizontal scroll), so every row's height is exact and so
  // is every placeholder — scroll geometry never shifts as blocks mount.
  // ~64 rows is ~1.2k px — one block mounts in well under a frame. The
  // window is recomputed synchronously from the block divs' own rects, so
  // content is in place when the frame paints — no observer latency, no
  // blank flashes mid-drag.
  const BLOCK_ROWS = 64;
  const OVERSCAN_PX = 1200;

  const parts = $derived(splitPath(file.path));
  const stats = $derived(fileStats(file));
  const text = $derived(file.content.kind === "text" ? file.content : null);
  const image = $derived(file.content.kind === "image" ? file.content : null);
  const conflict = $derived(
    file.content.kind === "conflict" ? file.content : null,
  );
  // The conflict body's stand-in hunks: one per resolved run and per
  // region side, in render order, referencing the DTO's own line arrays —
  // the identity the highlight cache and the code-column sizing key on.
  const cHunks = $derived(conflict ? conflictHunks(conflict.sections) : []);
  const regionTotal = $derived(
    conflict
      ? conflict.sections.filter((s) => s.kind === "region").length
      : 0,
  );
  // "Conflict N of M" ordinals, index-aligned with the sections.
  const regionOrdinal = $derived.by(() => {
    let n = 0;
    return (conflict?.sections ?? []).map((s) =>
      s.kind === "region" ? ++n : 0,
    );
  });

  // Pixel dimensions per image side, read off the decoded images — the
  // backend ships bytes, not metadata. Cards remount per diff (keyed in
  // DiffView), so stale dimensions never survive a comparison switch.
  const imgDims = $state<{ old?: string; new?: string }>({});

  function onImgLoad(event: Event, side: "old" | "new"): void {
    const el = event.currentTarget as HTMLImageElement;
    imgDims[side] = `${el.naturalWidth}×${el.naturalHeight}`;
  }
  const digits = $derived(text ? gutterDigits(text.hunks) : 2);
  const codew = $derived(text ? maxLineCh(text.hunks) : maxLineCh(cHunks));

  // Syntax spans are computed lazily per hunk as blocks mount and cached
  // by segments-array identity, so both layouts share one computation and
  // repeated renders of a mounted block are lookups (see highlight.ts).
  const mounted = new SvelteSet<number>();
  let hl = $state<(segments: DiffSegment[]) => HighlightSpan[]>(plainSpans);
  const highlightVisible = $derived(!collapsed && (mounted.size > 0 || conflict !== null));
  $effect(() => {
    const path = file.path;
    const hunks = text?.hunks ?? cHunks;
    if (!highlightVisible) return;
    let active = true;
    hl = plainSpans;
    void fileHighlighter(path, hunks).then((highlighter) => {
      if (active) hl = highlighter;
    });
    return () => { active = false; };
  });

  // ---- Find decoration ----
  // The find bar's matches for this file, re-keyed by segments-array
  // identity — the same key the highlight cache uses, and one both
  // layouts' rows carry by reference.
  const findRanges = $derived.by(() => {
    const map = new Map<DiffSegment[], { start: number; end: number }[]>();
    if (!find || !text) return map;
    for (const m of find.matches) {
      if (m.hunk < 0) continue;
      const segments = text.hunks[m.hunk]?.lines[m.line]?.segments;
      if (!segments) continue;
      const list = map.get(segments);
      if (list) list.push({ start: m.start, end: m.end });
      else map.set(segments, [{ start: m.start, end: m.end }]);
    }
    return map;
  });
  const findActive = $derived(
    find && find.active >= 0 ? (find.matches[find.active] ?? null) : null,
  );
  const activeSegments = $derived(
    findActive && findActive.hunk >= 0 && text
      ? (text.hunks[findActive.hunk]?.lines[findActive.line]?.segments ?? null)
      : null,
  );
  const pathHit = $derived(find?.matches.some((m) => m.hunk < 0) ?? false);

  // Syntax spans with the find overlay: undecorated lines pass through
  // the highlight cache untouched.
  function deco(segments: DiffSegment[]): FindSpan[] {
    const spans = hl(segments);
    const ranges = findRanges.get(segments);
    if (!ranges) return spans;
    return overlayMatches(
      spans,
      ranges,
      activeSegments === segments ? findActive!.start : null,
    );
  }

  let sectionEl = $state<HTMLElement>();

  // Scroll the current match into view. Geometry is exact without
  // mounting: block placeholders have computed heights, and a row's
  // offset within its block follows from the row kinds (see find.ts) —
  // so the reveal lands even when the target block is windowed out, and
  // the scroll itself mounts it.
  $effect(() => {
    const f = find;
    if (!f || f.active < 0) return;
    void f.seq;
    const m = f.matches[f.active];
    if (!m) return;
    untrack(() => reveal(m));
  });

  function reveal(m: FindMatch): void {
    if (m.hunk < 0) {
      sectionEl?.scrollIntoView({ block: "start" });
      return;
    }
    const el = layout === "split" ? paneEls.l : linesEl;
    const scroller = linesEl?.closest(".scroller");
    if (!text || !el || !linesEl || !scroller) return;
    const segments = text.hunks[m.hunk]?.lines[m.line]?.segments;
    if (!segments) return;
    const blocks = layout === "split" ? splBlocks : uniBlocks;
    const at = locateRow(blocks, segments);
    if (!at || at.block >= el.children.length) return;
    const rowH = parseFloat(getComputedStyle(linesEl).lineHeight) || 17;
    const y =
      el.children[at.block].getBoundingClientRect().top -
      scroller.getBoundingClientRect().top +
      scroller.scrollTop +
      rowOffsetPx(blocks[at.block], at.row, rowH);
    scroller.scrollTo({ top: Math.max(0, y - scroller.clientHeight / 3) });
  }

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
  let linesEl = $state<HTMLDivElement>();
  // The split layout's two panes; the left one is the windowing probe.
  const SIDES: ("l" | "r")[] = ["l", "r"];
  const paneEls = $state<{ l?: HTMLDivElement; r?: HTMLDivElement }>({});

  // An unmounted block's placeholder height. Exact: a line row is one
  // row height, a gap divider one and a half, and nothing wraps.
  function placeholderHeight(rows: object[]): string {
    let gaps = 0;
    for (const row of rows) if ("gap" in row) gaps++;
    return `calc(${rows.length} * var(--row-h) + ${gaps} * (var(--row-h) / 2))`;
  }

  // Offsets are in line-height units, including the extra half-line gap.
  // Rebuilt only when content/layout changes; scrolling does two element
  // reads and a binary search, regardless of the number of diff blocks.
  const blockOffsets = $derived(rowOffsets<readonly object[]>(
    layout === "split" ? splBlocks : uniBlocks,
    (rows) => rows.length + rows.filter((row) => "gap" in row).length / 2,
  ));

  function updateWindow(view?: { top: number; height: number }): void {
    const el = layout === "split" ? paneEls.l : linesEl;
    if (!el || !linesEl) return;
    const scroller = el.closest<HTMLElement>(".scroller");
    const viewport = view ?? {
      top: scroller?.getBoundingClientRect().top ?? 0,
      height: scroller?.clientHeight ?? window.innerHeight,
    };
    const style = getComputedStyle(linesEl);
    const rowH = parseFloat(style.lineHeight) || 19.2;
    const padding = el === linesEl ? parseFloat(style.paddingTop) || 0 : 0;
    const top = viewport.top - el.getBoundingClientRect().top - padding;
    const { start, end } = visibleRange(
      blockOffsets, top / rowH, viewport.height / rowH, OVERSCAN_PX / rowH,
    );
    for (const i of mounted) if (i < start || i >= end) mounted.delete(i);
    for (let i = start; i < end; i++) mounted.add(i);
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

  // Share scroll/resize work across every file in this diff.
  $effect(() => {
    const scroller = linesEl?.closest<HTMLElement>(".scroller");
    if (!scroller) return;
    return observeViewport(scroller, (view) => untrack(() => updateWindow(view)));
  });

  // ---- Horizontal panning ----
  // The diff owns no inner scroll containers: code cells are
  // `overflow: clip` viewports and their text slides on one shared
  // per-file offset (--panx). WKWebView latches a whole wheel gesture to
  // the innermost scroll container under the pointer, so an inner
  // overflow-x pane would eat vertical gestures too (overflow-y cannot
  // compute to visible beside it); with clip there is nothing to latch —
  // vertical scrolling stays native to the document scroller, and only
  // horizontal deltas are routed here. Both split panes read the same
  // offset, so they can never drift out of alignment.
  let panX = $state(0);
  // The code column's viewport and content widths, read off any mounted
  // cell: every cell's slide is min-width --codew, so one speaks for all.
  let viewW = $state(0);
  let contW = $state(0);
  const panMax = $derived(Math.max(0, contW - viewW));
  let panning = $state(false);
  let hdrag = $state<{ x: number; pan: number } | null>(null);
  // Whether the current wheel gesture went horizontal-dominant and is
  // ours until it goes quiet. Momentum arrives as more wheel events, so
  // inertia rides the same path.
  let manual = false;
  let gestureTimer = 0;
  let panningTimer = 0;

  function measure(): void {
    const cell = linesEl?.querySelector<HTMLElement>(".code:not(.absent)");
    if (!cell) return;
    viewW = cell.clientWidth;
    contW = cell.scrollWidth;
    if (panX > panMax) panX = panMax;
  }

  function pan(dx: number): void {
    if (contW === 0) measure();
    const next = Math.max(0, Math.min(panMax, panX + dx));
    if (next === panX) return;
    panX = next;
    panning = true;
    clearTimeout(panningTimer);
    panningTimer = window.setTimeout(() => (panning = false), 700);
  }

  // Wheel routing: a vertical-dominant event is left entirely native —
  // the document scroller is the only scroll container, so the platform's
  // scroll physics apply untouched, and incidental sideways jitter never
  // shifts the code. Once a gesture turns horizontal-dominant it is ours
  // until it ends: preventDefault keeps WKWebView from rubber-banding a
  // document that cannot scroll sideways, dx pans the code, and dy keeps
  // flowing to the document so diagonal gestures stay alive. Pinch-zoom
  // (ctrlKey) is not our gesture.
  function onWheel(e: WheelEvent): void {
    if (e.ctrlKey) return;
    clearTimeout(gestureTimer);
    gestureTimer = window.setTimeout(() => (manual = false), 150);
    if (!manual && Math.abs(e.deltaX) <= Math.abs(e.deltaY)) return;
    manual = true;
    e.preventDefault();
    pan(e.deltaX);
    if (e.deltaY !== 0) linesEl?.closest(".scroller")?.scrollBy(0, e.deltaY);
  }

  // preventDefault requires an explicitly non-passive listener.
  $effect(() => {
    const el = linesEl;
    if (!el) return;
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => {
      el.removeEventListener("wheel", onWheel);
      clearTimeout(gestureTimer);
      clearTimeout(panningTimer);
    };
  });

  // Keep the measurements fresh: cells exist only once blocks mount, and
  // the geometry moves with the window, the file tree, and the layout
  // toggle. untrack: measure clamps panX, which must not feed back.
  $effect(() => {
    void mounted.size;
    void codew;
    void layout;
    if (linesEl) untrack(measure);
  });

  $effect(() => {
    const el = linesEl;
    if (!el) return;
    const ro = new ResizeObserver(() => measure());
    ro.observe(el);
    return () => ro.disconnect();
  });

  function thumbDown(e: PointerEvent): void {
    e.preventDefault();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    hdrag = { x: e.clientX, pan: panX };
  }

  function thumbMove(e: PointerEvent): void {
    if (!hdrag || viewW === 0) return;
    const next = hdrag.pan + (e.clientX - hdrag.x) * (contW / viewW);
    panX = Math.max(0, Math.min(panMax, next));
  }

  function thumbUp(): void {
    hdrag = null;
  }

  // A press on the bare track centers the thumb there; thumb presses
  // bubble here too, so let them through.
  function trackDown(e: PointerEvent): void {
    if (e.target !== e.currentTarget) return;
    const track = e.currentTarget as HTMLElement;
    const next = (e.offsetX / track.clientWidth) * contW - viewW / 2;
    panX = Math.max(0, Math.min(panMax, next));
  }
</script>

<!-- One side of an aligned split row: gutter cell, sign cell, code cell —
     or a recessed absent triple when the row only exists on the other side.
     The sign is outside .selectable, so copied code never grows ± noise. -->
{#snippet half(cell: SplitCell | null, side: "l" | "r")}
  {#if cell}
    <span
      class="no {side} {cell.kind}"
      data-old={side === "l" ? cell.no : undefined}
      data-new={side === "r" ? cell.no : undefined}>{cell.no}</span><span
      class="sign {side} {cell.kind}"
      aria-hidden="true"
      data-old={side === "l" ? cell.no : undefined}
      data-new={side === "r" ? cell.no : undefined}
    >{cell.kind === "added" ? "+" : cell.kind === "removed" ? "−" : ""}</span><span
      class="code selectable {side} {cell.kind}"
      class:intraline={cell.intraline}
      data-old={side === "l" ? cell.no : undefined}
      data-new={side === "r" ? cell.no : undefined}
    ><span class="slide">{#each deco(cell.segments) as span}{#if span.hit}<mark
        class="find {span.cls ?? ''}"
        class:cur={span.current}>{span.text}</mark>{:else if span.changed}<mark
        class={span.cls}>{span.text}</mark>{:else if span.cls}<span
        class={span.cls}>{span.text}</span>{:else}{span.text}{/if}{/each}</span></span>
  {:else}
    <span class="no {side} absent"></span><span class="sign {side} absent"
    ></span><span class="code {side} absent"></span>
  {/if}
{/snippet}

<!-- One row of the conflict body: the unified grid with empty number
     cells — a conflicted file has no single line numbering to show. -->
{#snippet cline(row: DiffLine)}
  <div
    class="line {row.kind}"
    class:intraline={row.segments.some((s) => s.changed) &&
      row.segments.some((s) => !s.changed)}
  >
    <span class="no"></span><span class="no"></span><span class="sign"
      aria-hidden="true"
    >{row.kind === "added"
        ? "+"
        : row.kind === "removed"
          ? "−"
          : ""}</span><span class="code selectable"
    ><span class="slide">{#each deco(row.segments) as span}{#if span.changed}<mark
        class={span.cls}>{span.text}</mark>{:else if span.cls}<span
        class={span.cls}>{span.text}</span>{:else}{span.text}{/if}{/each}</span></span>
  </div>
{/snippet}

<!-- The file's horizontal scrollbar: the only scroll UI the diff owns.
     Rendered when the code is wider than its column, pinned to the bottom
     of the scrollport while the file passes by, revealed on hover or
     while panning — the overlay-scrollbar manners of the rest of the app. -->
{#snippet hbar()}
  {#if contW > viewW + 1}
    <div class="hscroll" class:dragging={hdrag !== null}>
      <div
        class="htrack"
        role="scrollbar"
        aria-controls="{uid}-code"
        aria-orientation="horizontal"
        aria-valuemin={0}
        aria-valuemax={Math.round(panMax)}
        aria-valuenow={Math.round(panX)}
        tabindex="-1"
        onpointerdown={trackDown}
      >
        <div
          class="hthumb"
          role="presentation"
          style:width="{(viewW / contW) * 100}%"
          style:left="{(panX / contW) * 100}%"
          onpointerdown={thumbDown}
          onpointermove={thumbMove}
          onpointerup={thumbUp}
          onpointercancel={thumbUp}
        ></div>
      </div>
    </div>
  {/if}
{/snippet}

<!-- One side of an image comparison: the picture on a quiet checker (so
     transparency reads as transparency), captioned with which side it is,
     its pixel dimensions once decoded, and its byte size. A side over the
     inline cap keeps its caption and shows a placeholder instead. -->
{#snippet imgSide(side: ImageContent, which: "old" | "new")}
  <figure class="img-side">
    <div class="img-box">
      {#if side.data !== null}
        <img
          src="data:{side.mime};base64,{side.data}"
          alt="{which === 'old' ? 'Old' : 'New'} {parts.name}"
          onload={(e) => onImgLoad(e, which)}
        />
      {:else}
        <span class="img-nopreview">Too large to preview</span>
      {/if}
    </div>
    <figcaption class="img-caption">
      <span class="img-label {which}">{which === "old" ? "Old" : "New"}</span>
      {#if imgDims[which]}<span>{imgDims[which]}</span>{/if}
      <span>{formatBytes(side.size)}</span>
    </figcaption>
  </figure>
{/snippet}

<!-- One file of the continuous diff. The header stays stuck below the
     change header while the file's lines scroll by, and doubles as the
     collapse toggle for the file's body. -->
<section
  class="file"
  data-file-index={index}
  data-path={file.path}
  bind:this={sectionEl}
>
  <div class="file-head">
    <button
      type="button"
      class="head-toggle"
      aria-expanded={!collapsed}
      onclick={ontoggle}
    >
      <span class="chev" class:open={!collapsed}>
        <Icon name="chevronRight" size={12} />
      </span>
      <span class="status {file.status}">{STATUS_GLYPH[file.status]}</span>
      <span
        class="path mono"
        class:find-hit={pathHit}
        class:cur={pathHit && findActive !== null && findActive.hunk < 0}
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
        <span class="conflict-chip">
          {regionTotal > 1 ? `${regionTotal} conflicts` : "conflict"}
        </span>
      {/if}
      {#if stats.added > 0 || stats.removed > 0}
        <span class="stats mono">
          {#if stats.added > 0}<span class="add">+{stats.added}</span>{/if}
          {#if stats.removed > 0}<span class="del">−{stats.removed}</span>{/if}
        </span>
      {/if}
    </button>
    {#if file.hasConflict && resolve}
      <button
        type="button"
        class="resolve"
        class:waiting={resolve.waiting}
        disabled={resolve.disabled}
        data-resolve-path={file.path}
        title="Open the 3-way merge in {resolve.tool} and record the saved result"
        onclick={onresolve}
      >
        {resolve.waiting ? `Waiting for ${resolve.tool}…` : `Resolve in ${resolve.tool}`}
      </button>
    {/if}
  </div>

  {#if !collapsed}
    {#if text}
    {#if text.hunks.length === 0}
      <p class="note">No content changes</p>
    {:else if layout === "split"}
      <!-- Two half-width panes divided at the pane midpoint, panning
           sideways on one shared offset — long lines on one side never
           squeeze the other, and the halves can never disagree about
           where they are. Rows never wrap, so the two stacks stay
           aligned by construction; the windowing reads the left pane's
           blocks and both panes mount in lockstep. -->
      <div
        class="lines mono split"
        id="{uid}-code"
        class:panning
        bind:this={linesEl}
        style:--numw="{digits + 1}ch"
        style:--codew="{codew}ch"
        style:--panx="{panX}px"
      >
        <div class="panes">
          {#each SIDES as side (side)}
            <div class="pane" class:r={side === "r"} bind:this={paneEls[side]}>
              {#each splBlocks as rows, blockIndex}
                <div
                  class="split-grid"
                  style:height={mounted.has(blockIndex)
                    ? null
                    : placeholderHeight(rows)}
                >
                  {#if mounted.has(blockIndex)}
                    <!-- Keyed by row identity: rows are rebuilt whenever the
                         hunks change, and a row updated in place can
                         shape-shift under a rendered branch (a present cell
                         turning absent) faster than the branch
                         re-evaluates. -->
                    {#each rows as row (row)}
                      {#if "gap" in row}
                        <div
                          class="gap split-gap"
                          aria-label="{row.gap} unchanged lines"
                        >
                          <span class="gap-label">
                            {#if row.context}<span class="sym">{row.context}</span>{/if}
                            <span class="count"
                            >{row.gap} unchanged {row.gap === 1 ? "line" : "lines"}</span>
                          </span>
                        </div>
                      {:else}
                        {@render half(side === "l" ? row.left : row.right, side)}
                      {/if}
                    {/each}
                  {/if}
                </div>
              {/each}
              {@render hbar()}
            </div>
          {/each}
        </div>
        {#if text.truncated}
          <p class="note">Diff truncated — this file changes more lines than Jiji renders</p>
        {/if}
      </div>
    {:else}
      <div
        class="lines mono"
        id="{uid}-code"
        class:panning
        bind:this={linesEl}
        style:--numw="{digits + 1}ch"
        style:--codew="{codew}ch"
        style:--panx="{panX}px"
      >
        {#each uniBlocks as rows, blockIndex}
          <div
            class="block"
            style:height={mounted.has(blockIndex)
              ? null
              : placeholderHeight(rows)}
          >
            {#if mounted.has(blockIndex)}
              <!-- Keyed by row identity — see the split layout note above. -->
              {#each rows as row (row)}
                {#if "gap" in row}
                  <div class="gap" aria-label="{row.gap} unchanged lines">
                    <span class="gap-label">
                      {#if row.context}<span class="sym">{row.context}</span>{/if}
                      <span class="count"
                      >{row.gap} unchanged {row.gap === 1 ? "line" : "lines"}</span>
                    </span>
                  </div>
                {:else}
                  <div
                    class="line {row.kind}"
                    class:intraline={row.intraline}
                    data-old={row.oldNo ?? undefined}
                    data-new={row.newNo ?? undefined}
                  >
                    <span class="no">{row.oldNo ?? ""}</span><span class="no"
                    >{row.newNo ?? ""}</span><span class="sign" aria-hidden="true"
                    >{row.kind === "added"
                        ? "+"
                        : row.kind === "removed"
                          ? "−"
                          : ""}</span><span class="code selectable"
                    ><span class="slide">{#each deco(row.segments) as span}{#if span.hit}<mark
                        class="find {span.cls ?? ''}"
                        class:cur={span.current}>{span.text}</mark>{:else if span.changed}<mark
                        class={span.cls}>{span.text}</mark>{:else if span.cls}<span
                        class={span.cls}>{span.text}</span>{:else}{span.text}{/if}{/each}</span></span>
                  </div>
                {/if}
              {/each}
            {/if}
          </div>
        {/each}
        {#if text.truncated}
          <p class="note">Diff truncated — this file changes more lines than Jiji renders</p>
        {/if}
        {@render hbar()}
      </div>
    {/if}
    {:else if conflict}
      <!-- A conflicted file's merge structure, read-only: resolved runs as
           context, each region showing every side as its diff from the
           base. Rows reuse the unified grid and pan on the shared offset;
           sections arrive context-trimmed and budget-capped, so the body
           renders whole without windowing. -->
      <div
        class="lines mono"
        id="{uid}-code"
        class:panning
        bind:this={linesEl}
        style:--numw="{digits + 1}ch"
        style:--codew="{codew}ch"
        style:--panx="{panX}px"
      >
        {#each conflict.sections as section, si}
          {#if section.kind === "resolved"}
            {#each section.lines as row (row)}
              {@render cline(row)}
            {/each}
          {:else if section.kind === "elided"}
            <div class="gap" aria-label="{section.lines} resolved lines">
              <span class="gap-label">
                <span class="count"
                >{section.lines} resolved {section.lines === 1 ? "line" : "lines"}</span>
              </span>
            </div>
          {:else}
            <section class="region" aria-label="Conflict {regionOrdinal[si]} of {regionTotal}">
              <div class="region-head">
                Conflict {regionOrdinal[si]} of {regionTotal}
              </div>
              {#each section.sides as side, i}
                <div class="side-head">
                  <span class="side-name"
                  >Side #{i + 1}{section.sides.length === 2
                      ? ` · ${i === 0 ? "ours" : "theirs"}`
                      : ""}</span>
                  <span class="side-note">vs base</span>
                  {#if side.label}
                    <span class="side-label" title={side.label}>{side.label}</span>
                  {/if}
                </div>
                {#each side.lines as row (row)}
                  {@render cline(row)}
                {/each}
              {/each}
            </section>
          {/if}
        {/each}
        {#if conflict.truncated}
          <p class="note">Conflict truncated — this file holds more conflict content than Jiji renders</p>
        {/if}
        {@render hbar()}
      </div>
    {:else if image}
      <div class="img-diff">
        {#if image.old}{@render imgSide(image.old, "old")}{/if}
        {#if image.new}{@render imgSide(image.new, "new")}{/if}
      </div>
    {:else if file.content.kind === "binary"}
      <p class="note">{binaryNote(file.content)}</p>
    {:else if file.content.kind === "tooLarge"}
      <p class="note">File too large to diff</p>
    {:else}
      <p class="note">Not rendered — this change is too big to show every file</p>
    {/if}
  {/if}
</section>

<style>
  /* Files run edge to edge in one continuous scroll, each opened by a
     header that pins to the top of the scrollport while its lines pass —
     no cards, no margins: the width belongs to the code. */
  .file + .file {
    border-top: 1px solid var(--clr-border-1);
  }

  .file:last-of-type {
    border-bottom: 1px solid var(--clr-border-2);
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
    padding-right: var(--sp-3);
    background: var(--diff-file-head);
    border-bottom: 1px solid var(--clr-border-2);
  }

  /* The whole left run of the header stays the collapse toggle; actions
     like Resolve sit beside it rather than nesting buttons. */
  .head-toggle {
    flex: 1;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
    height: 30px;
    padding: 0 0 0 var(--sp-2);
    text-align: left;
  }

  .file-head:hover {
    background: color-mix(in srgb, var(--clr-text-1) 3%, var(--diff-file-head));
  }

  .resolve {
    flex-shrink: 0;
    height: 22px;
    font-size: var(--text-xs);
    font-weight: 500;
    font-family: var(--font-ui);
    padding: 0 8px;
    border-radius: var(--radius-s);
    border: 1px solid var(--clr-border-1);
    background: var(--clr-bg-3);
    color: var(--clr-text-1);
    transition:
      background var(--t-fast) var(--ease-out),
      border-color var(--t-fast) var(--ease-out),
      color var(--t-fast) var(--ease-out);
  }

  .resolve:hover:not(:disabled) {
    background: var(--clr-accent-dim);
    border-color: color-mix(in srgb, var(--clr-accent) 40%, transparent);
    color: var(--clr-accent-strong);
  }

  .resolve:disabled {
    opacity: 0.55;
  }

  .resolve.waiting {
    opacity: 1;
    color: var(--clr-accent-strong);
    border-color: color-mix(in srgb, var(--clr-accent) 40%, transparent);
    background: var(--clr-accent-dim);
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

  /* A/M/D/R in the status's hue — a letter, not a tile. */
  .status {
    flex-shrink: 0;
    width: 12px;
    text-align: center;
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    font-weight: 600;
  }

  .status.added {
    color: var(--diff-add-fg);
  }

  .status.modified {
    color: var(--clr-warn);
  }

  .status.removed {
    color: var(--diff-del-fg);
  }

  .status.renamed,
  .status.copied {
    color: var(--clr-accent-strong);
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
    font-weight: 500;
    color: var(--clr-danger);
  }

  .stats {
    flex-shrink: 0;
    margin-left: auto;
    display: flex;
    gap: 6px;
    font-size: var(--text-xs);
  }

  .stats .add {
    color: var(--diff-add-fg);
  }

  .stats .del {
    color: var(--diff-del-fg);
  }

  .note {
    padding: var(--sp-2) var(--sp-4);
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  /* Image comparison: old and new side by side as plain hairlined boxes.
     No tinted fills — the pictures carry the content; the caption labels
     carry the add/remove hues. */
  .img-diff {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-4);
    padding: var(--sp-4);
  }

  .img-side {
    min-width: 0;
    max-width: min(48%, 480px);
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    margin: 0;
  }

  /* A whisper-quiet checker derived from the recess surface, so alpha
     regions read as transparent in both themes without shouting. */
  .img-box {
    display: grid;
    place-items: center;
    padding: var(--sp-3);
    border: 1px solid var(--clr-border-2);
    background:
      repeating-conic-gradient(
          color-mix(in srgb, var(--clr-text-1) 4%, var(--diff-recess)) 0% 25%,
          var(--diff-recess) 0% 50%
        )
        0 0 / 14px 14px;
  }

  .img-box img {
    display: block;
    max-width: 100%;
    max-height: 360px;
  }

  .img-nopreview {
    padding: var(--sp-4) var(--sp-2);
    font-size: var(--text-s);
    color: var(--clr-text-3);
    font-style: italic;
  }

  .img-caption {
    display: flex;
    align-items: baseline;
    gap: var(--sp-3);
    font-family: var(--font-ui);
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .img-label {
    font-weight: 500;
  }

  .img-label.old {
    color: var(--diff-del-fg);
  }

  .img-label.new {
    color: var(--diff-add-fg);
  }

  /* Lines never wrap — code keeps its indentation silhouette, the shape
     the eye actually reads — so the surplus pans sideways per file. The
     body holds NO scroll containers: each code cell clips (overflow:
     clip is not a scroll container, so WKWebView cannot latch wheel
     gestures to it — vertical scrolling always belongs to the document)
     and the text inside slides on the file's shared --panx offset. Rows
     are plain viewport-width grids; the gutter rail (numbers + sign) is
     a static column on its own recessed surface, no longer pinned by
     position: sticky — which also frees it to run at a smaller size than
     the code. Grid tracks in ch resolve against the row's font, so the
     rail columns shrink with it. */
  .lines {
    --row-h: calc(var(--text-code) * var(--lh-code));
    font-size: var(--text-code);
    line-height: var(--lh-code);
    padding: 2px 0;
  }

  .line {
    display: grid;
    grid-template-columns: var(--numw) var(--numw) 2ch minmax(0, 1fr);
    width: 100%;
    min-height: var(--row-h);
    font-size: var(--text-xs);
  }

  .no {
    background: var(--diff-gutter-surface);
    text-align: right;
    padding-right: 1ch;
    color: var(--clr-text-3);
    user-select: none;
    /* The rail's smaller digits center on the code's row height. */
    line-height: var(--row-h);
  }

  /* The ± marker: the left-edge anchor the eye scans before it reads
     color. Outside .selectable, so copied code stays clean. */
  .sign {
    background: var(--diff-gutter-surface);
    text-align: center;
    user-select: none;
    line-height: var(--row-h);
  }

  .code {
    font-size: var(--text-code);
    overflow: clip;
  }

  /* The sliding text. Sized to the file's widest line (--codew) so every
     cell shares one content width — any cell's scrollWidth is the file's,
     and the pan range is uniform. Trailing padding rides inside, so the
     last column keeps its breathing room at full pan. */
  .slide {
    display: block;
    width: max-content;
    min-width: var(--codew, 0px);
    padding: 0 var(--sp-4) 0 8px;
    white-space: pre;
    tab-size: 4;
    transform: translateX(calc(-1 * var(--panx, 0px)));
  }

  .code mark {
    background: transparent;
    color: inherit;
    border-radius: 2px;
    padding: 1px 0;
  }

  /* Syntax classes from highlight.ts — color only, so the add/remove
     tints and intraline mark backgrounds stay the layer underneath.
     :global() because the classes arrive from data, not static markup. */
  .code :global(.syn-kw) {
    color: var(--syn-kw);
  }

  .code :global(.syn-str) {
    color: var(--syn-str);
  }

  .code :global(.syn-const) {
    color: var(--syn-const);
  }

  .code :global(.syn-type) {
    color: var(--syn-type);
  }

  .code :global(.syn-fn) {
    color: var(--syn-fn);
  }

  .code :global(.syn-prop) {
    color: var(--syn-prop);
  }

  .code :global(.syn-comment) {
    color: var(--syn-comment);
    font-style: italic;
  }

  .code :global(.syn-meta) {
    color: var(--syn-meta);
  }

  /* Markdown structure: weight and slant, not new hues. */
  .code :global(.syn-heading) {
    font-weight: 600;
  }

  .code :global(.syn-link) {
    color: var(--syn-fn);
  }

  .code :global(.syn-em) {
    font-style: italic;
  }

  .code :global(.syn-strong) {
    font-weight: 600;
  }

  /* Changed lines speak through three redundant channels — fill, tinted
     gutter rail, ± sign — so add/remove segregates before reading, the
     way dedicated diff tools do, not only once the hue is inspected. */
  .line.added {
    background: var(--diff-add-bg);
  }

  .line.added .no,
  .line.added .sign {
    background: var(--diff-add-gutter);
    color: var(--diff-add-fg);
  }

  .line.removed {
    background: var(--diff-del-bg);
  }

  .line.removed .no,
  .line.removed .sign {
    background: var(--diff-del-gutter);
    color: var(--diff-del-fg);
  }

  /* Word-level emphasis only where a line has a changed/unchanged mix;
     whole added or removed lines stay a flat tint. */
  .line.added.intraline mark {
    background: var(--diff-add-word);
  }

  .line.removed.intraline mark {
    background: var(--diff-del-word);
  }

  /* An elided run is a seam between hunks: a recessed band that chunks
     the diff into blocks, labeled with the declaration enclosing the code
     below (the backend's git-style "funcname") and the skipped count.
     Rows are viewport-width, so the label sits at the pane edge without
     any pinning. */
  .gap {
    display: grid;
    grid-template-columns: var(--numw) var(--numw) 2ch minmax(0, 1fr);
    align-items: center;
    height: calc(var(--row-h) * 1.5);
    background: var(--diff-recess);
    user-select: none;
    font-size: var(--text-xs);
  }

  .gap .gap-label {
    grid-column: 4;
    justify-self: start;
    min-width: 0;
    display: flex;
    align-items: baseline;
    gap: var(--sp-3);
    padding: 0 var(--sp-3) 0 8px;
  }

  /* The symbol reads as code; the count as commentary. */
  .gap .sym {
    max-width: 64ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: pre;
    font-size: var(--text-xs);
    color: var(--clr-text-2);
  }

  .gap .count {
    flex-shrink: 0;
    font-family: var(--font-ui);
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  /* Conflict regions: a danger-tinted band opens each region and hairlines
     frame it; every side gets a quiet recessed header. Flat surfaces only,
     like the rest of the app — the add/remove line tints underneath stay
     the loudest layer. */
  .region {
    border-top: 1px solid
      color-mix(in srgb, var(--clr-danger) 35%, var(--clr-border-2));
    border-bottom: 1px solid
      color-mix(in srgb, var(--clr-danger) 35%, var(--clr-border-2));
  }

  .region-head {
    padding: 2px var(--sp-3) 2px 8px;
    font-family: var(--font-ui);
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--clr-danger);
    background: color-mix(in srgb, var(--clr-danger) 8%, transparent);
    user-select: none;
  }

  .side-head {
    display: flex;
    align-items: baseline;
    gap: var(--sp-3);
    padding: 1px var(--sp-3) 1px 8px;
    font-family: var(--font-ui);
    font-size: var(--text-xs);
    background: var(--diff-recess);
    border-top: 1px solid var(--clr-border-2);
    border-bottom: 1px solid var(--clr-border-2);
    color: var(--clr-text-2);
    user-select: none;
  }

  .side-head .side-name {
    font-weight: 600;
  }

  .side-head .side-note {
    color: var(--clr-text-3);
  }

  .side-head .side-label {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--clr-text-3);
  }

  /* Side-by-side: two equal panes split by a divider fixed at the pane
     midpoint, both panning on the file's one --panx offset. Every block
     renders its own grid, but the tracks are fixed (gutter from --numw,
     sign column, the code remainder), so nothing shifts across blocks —
     and rows never wrap, so the two stacks align by construction. */
  .panes {
    display: flex;
  }

  .pane {
    flex: 1 1 0;
    min-width: 0;
  }

  .pane.r {
    border-left: 1px solid var(--clr-border-2);
  }

  .split-grid {
    display: grid;
    grid-template-columns: var(--numw) 2ch minmax(0, 1fr);
    font-size: var(--text-xs);
  }

  .split-grid .no,
  .split-grid .sign,
  .split-grid .code {
    min-height: var(--row-h);
  }

  .split-grid .code.added {
    background: var(--diff-add-bg);
  }

  .split-grid .code.removed {
    background: var(--diff-del-bg);
  }

  .split-grid .no.added,
  .split-grid .sign.added {
    background: var(--diff-add-gutter);
    color: var(--diff-add-fg);
  }

  .split-grid .no.removed,
  .split-grid .sign.removed {
    background: var(--diff-del-gutter);
    color: var(--diff-del-fg);
  }

  .split-grid .code.added.intraline mark {
    background: var(--diff-add-word);
  }

  .split-grid .code.removed.intraline mark {
    background: var(--diff-del-word);
  }

  /* A row with no counterpart on this side reads as "nothing here", not
     as an empty line: the same recessed surface as the elided bands, no
     texture — flat, so it never competes with the content. */
  .split-grid .absent {
    background: var(--diff-recess);
  }

  /* The base .gap template is the unified rail; a pane's rail is one
     number column and a sign. */
  .split-gap {
    grid-column: 1 / -1;
    grid-template-columns: var(--numw) 2ch minmax(0, 1fr);
  }

  .split-gap .gap-label {
    grid-column: 3;
  }

  /* The horizontal scrollbar: a zero-height sticky seat at the end of
     the pane keeps the overlay riding the bottom of the scrollport while
     the file passes by, and resting on the file's own end when it is
     shorter. Sized like the app's native overlay thumbs (global.css).
     Track offsets in ch resolve at the rail's font size, mirroring the
     row grids. */
  .hscroll {
    position: sticky;
    bottom: 0;
    height: 0;
    z-index: 2;
    font-size: var(--text-xs);
  }

  .htrack {
    position: absolute;
    left: calc(2 * var(--numw) + 2ch);
    right: 0;
    bottom: 0;
    height: 11px;
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--t-fast) var(--ease-out);
  }

  .pane .htrack {
    left: calc(var(--numw) + 2ch);
  }

  .lines:hover .htrack,
  .lines.panning .htrack,
  .hscroll.dragging .htrack {
    opacity: 1;
    pointer-events: auto;
  }

  .hthumb {
    position: absolute;
    top: 3px;
    height: 5px;
    border-radius: 3px;
    background: var(--clr-scrollbar);
  }

  .hthumb:hover,
  .hscroll.dragging .hthumb {
    background: var(--clr-scrollbar-hover);
  }

  /* Find-bar hits ride above every other layer — the intraline word
     emphasis gives way under one (these selectors out-rank those rules) —
     and the current match steps up in strength. A warm tint against the
     cool accent palette, so hits read as "found", not "selected". */
  .code mark.find,
  .line.added.intraline mark.find,
  .line.removed.intraline mark.find,
  .split-grid .code.added.intraline mark.find,
  .split-grid .code.removed.intraline mark.find {
    background: color-mix(in srgb, var(--clr-warn) 28%, transparent);
  }

  .code mark.find.cur,
  .line.added.intraline mark.find.cur,
  .line.removed.intraline mark.find.cur,
  .split-grid .code.added.intraline mark.find.cur,
  .split-grid .code.removed.intraline mark.find.cur {
    background: color-mix(in srgb, var(--clr-warn) 55%, transparent);
  }

  /* The header when the file's path matched the query; stronger while it
     is the current match. */
  .path.find-hit .fname {
    background: color-mix(in srgb, var(--clr-warn) 28%, transparent);
    border-radius: 3px;
  }

  .path.find-hit.cur .fname {
    background: color-mix(in srgb, var(--clr-warn) 55%, transparent);
  }
</style>
