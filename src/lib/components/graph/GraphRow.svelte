<script lang="ts">
  import Icon from "$lib/components/ui/Icon.svelte";
  import { shortAge } from "$lib/time";
  import { drag } from "./dnd.svelte";
  import {
    gutterWidth,
    railX,
    NODE_ROW_HEIGHT,
    SYNC_GLYPH,
    type NodeRow,
    type Rail,
  } from "./graph";

  let {
    row,
    columnCount,
    emphasized,
    selected = false,
    onselect,
  }: {
    row: NodeRow;
    columnCount: number;
    /** Workstream id rendered hot; other streams render calm. */
    emphasized: string | null;
    selected?: boolean;
    onselect: () => void;
  } = $props();

  const node = $derived(row.node);
  const isBase = $derived(node.kind === "immutable");
  const tone = $derived(
    isBase ? "base" : row.stream !== null && row.stream === emphasized ? "hot" : "calm",
  );
  const title = $derived(node.description.split("\n")[0] ?? "");

  const H = NODE_ROW_HEIGHT;
  const CY = H / 2;
  const CR = 6; // rail corner radius
  const gw = $derived(gutterWidth(columnCount));
  const nx = $derived(railX(row.column));
  // Clearance around the marker so rails do not pierce hollow glyphs.
  const clear = $derived(row.isWorkingCopy ? 7 : 6);

  function railTone(rail: Rail): string {
    if (rail.stream === null) return "base";
    return rail.stream === emphasized ? "hot" : "calm";
  }

  function inPath(rail: Rail): string {
    const x = railX(rail.col);
    if (rail.col === row.column) return `M ${x} 0 V ${CY - clear}`;
    const s = x > nx ? -1 : 1; // direction of travel toward the node
    return `M ${x} 0 V ${CY - CR} Q ${x} ${CY} ${x + s * CR} ${CY} H ${nx - s * clear}`;
  }

  function outPath(rail: Rail): string {
    const x = railX(rail.col);
    if (rail.col === row.column) return `M ${x} ${CY + clear} V ${H}`;
    const s = x > nx ? 1 : -1; // direction of travel away from the node
    return `M ${nx + s * clear} ${CY} H ${x - s * CR} Q ${x} ${CY} ${x} ${CY + CR} V ${H}`;
  }

  // Immutable bases keep history below them; a dashed stub hands the rail
  // off to the elision row underneath.
  const stubBelow = $derived(isBase && row.edgesOut.length === 0);

  // Drag-and-drop reads the shared session directly, so the same states
  // light up wherever this row renders (graph or focus view): the row in
  // hand dims, the row under the pointer answers as the prospective new
  // parent — or as a refused target.
  const isDragSource = $derived(drag.active && drag.sourceId === node.id);
  const isDropTarget = $derived(drag.active && drag.targetId === node.id);
  const dropOk = $derived(isDropTarget && drag.plan?.allowed === true);
</script>

<button
  class="row {tone}"
  class:selected
  class:base={isBase}
  class:drag-source={isDragSource}
  class:drop-ok={dropOk}
  class:drop-no={isDropTarget && !dropOk}
  data-node-id={node.id}
  data-kind={node.kind}
  data-stream={row.stream}
  aria-pressed={selected}
  onclick={onselect}
>
  <span class="gutter" style:width="{gw}px">
    <svg width={gw} height={H} viewBox="0 0 {gw} {H}" aria-hidden="true">
      {#each row.passThrough as rail (rail.col)}
        <path class="rail {railTone(rail)}" d="M {railX(rail.col)} 0 V {H}" />
      {/each}
      {#each row.edgesIn as rail (rail.col)}
        <path class="rail {railTone(rail)}" d={inPath(rail)} />
      {/each}
      {#each row.edgesOut as rail (rail.col)}
        <path
          class="rail {railTone(rail)}"
          class:elided={rail.elided}
          d={outPath(rail)}
        />
      {/each}
      {#if stubBelow}
        <path class="rail stub" d="M {nx} {CY + clear} V {H}" />
      {/if}

      {#if row.isWorkingCopy}
        <text class="mk-wc" x={nx} y={CY + 1}>@</text>
      {:else if node.hasConflict}
        <circle class="mk-knockout" cx={nx} cy={CY} r="5.5" />
        <path
          class="mk-conflict"
          d="M {nx - 3.2} {CY - 3.2} L {nx + 3.2} {CY + 3.2} M {nx + 3.2} {CY - 3.2} L {nx - 3.2} {CY + 3.2}"
        />
      {:else if isBase}
        <rect
          class="mk-base"
          x={nx - 4}
          y={CY - 4}
          width="8"
          height="8"
          rx="1.5"
          transform="rotate(45 {nx} {CY})"
        />
      {:else}
        <circle class="mk-change {tone}" cx={nx} cy={CY} r="4" />
      {/if}
    </svg>
  </span>

  {#if node.isDivergent}
    <!-- jj's ?? state: the change id names several commits, so it renders
         alarmed and the commit id becomes the row's real identity. -->
    <span
      class="id mono divergent"
      title="Divergent change: several visible commits share {node.changeId} — this copy is commit {node.commitId}"
      >{node.changeId.slice(0, 8)}<b class="qq">??</b></span
    >
    <span class="divergent-commit mono">{node.commitId.slice(0, 8)}</span>
  {:else}
    <span class="id mono"><b>{node.id.slice(0, 2)}</b>{node.id.slice(2, 8)}</span>
  {/if}

  <span class="desc truncate" class:undescribed={!title}>
    {title || "No description yet"}
  </span>

  <span class="tags">
    {#if row.isWorkingCopy}
      <span class="chip wc">working copy</span>
    {/if}
    {#if node.hasConflict}
      <span class="chip conflict">conflict</span>
    {/if}
    {#each row.bookmarks as bookmark (bookmark.name)}
      {@const sync = SYNC_GLYPH[bookmark.sync]}
      <span
        class="chip bm"
        class:trunk={bookmark.isTrunk}
        title="{bookmark.name}{bookmark.remote ? `@${bookmark.remote}` : ''} — {sync.label}"
      >
        <Icon name="bookmark" size={9} />
        <span class="bm-name truncate">{bookmark.name}</span>
        {#if sync.glyph}<i class="sync {sync.tone}">{sync.glyph}</i>{/if}
      </span>
    {/each}
    {#if row.isStackHead && row.behindTrunk > 0}
      <span class="chip behind" title="{row.behindTrunk} change{row.behindTrunk === 1 ? '' : 's'} behind trunk">
        ↓{row.behindTrunk}
      </span>
    {/if}
    {#if node.isEmpty}
      <span class="empty-note">(empty)</span>
    {/if}
  </span>

  <span class="meta" title="{node.author} · {node.timestamp}">
    <span class="author truncate">{node.author}</span>
    <span class="age">{shortAge(node.timestamp)}</span>
  </span>
</button>

<style>
  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    height: 26px;
    text-align: left;
    padding-right: var(--sp-3);
    transition: background var(--t-fast) var(--ease-out);
  }

  .row:hover {
    background: var(--clr-bg-hover);
  }

  /* The trunk/base zone reads recessed inside the same tree. */
  .row.base {
    background: color-mix(in srgb, var(--clr-bg-0) 40%, transparent);
  }

  .row.selected {
    background: color-mix(in srgb, var(--clr-accent) 9%, transparent);
    box-shadow: inset 2px 0 0 var(--clr-accent);
  }

  /* Mutable rows are objects you can pick up. */
  .row:not(.base) {
    cursor: grab;
  }

  .row.drag-source {
    opacity: 0.4;
  }

  /* The prospective new parent under the pointer. */
  .row.drop-ok {
    background: color-mix(in srgb, var(--clr-accent) 13%, transparent);
    box-shadow: inset 0 0 0 1.5px color-mix(in srgb, var(--clr-accent) 60%, transparent);
  }

  .row.drop-no {
    background: color-mix(in srgb, var(--clr-danger) 7%, transparent);
    box-shadow: inset 0 0 0 1.5px color-mix(in srgb, var(--clr-danger) 32%, transparent);
  }

  .gutter {
    flex-shrink: 0;
    height: 100%;
  }

  svg {
    display: block;
  }

  .rail {
    fill: none;
    stroke-width: 1.5;
  }

  .rail.hot {
    stroke: color-mix(in srgb, var(--clr-accent) 72%, var(--clr-bg-3));
  }

  .rail.calm {
    stroke: color-mix(in srgb, var(--clr-accent) 30%, var(--clr-bg-3));
  }

  .rail.base {
    stroke: color-mix(in srgb, var(--clr-text-3) 55%, var(--clr-bg-3));
  }

  .rail.stub {
    stroke: color-mix(in srgb, var(--clr-text-3) 55%, var(--clr-bg-3));
    stroke-dasharray: 2 3;
  }

  /* Edges crossing elided history read as interrupted. */
  .rail.elided {
    stroke-dasharray: 2 3;
  }

  /* Node markers, jj-native: @ working copy, ○ mutable, ◆ immutable, × conflict. */
  .mk-wc {
    font-family: var(--font-mono);
    font-size: 13px;
    font-weight: 700;
    fill: var(--clr-working-copy);
    text-anchor: middle;
    dominant-baseline: central;
    paint-order: stroke;
    stroke: var(--clr-bg-1);
    stroke-width: 3;
    filter: drop-shadow(0 0 5px color-mix(in srgb, var(--clr-working-copy) 55%, transparent));
  }

  .mk-change {
    fill: var(--clr-bg-1);
    stroke-width: 1.5;
  }

  .mk-change.hot {
    stroke: var(--clr-accent);
  }

  .mk-change.calm {
    stroke: color-mix(in srgb, var(--clr-accent) 45%, var(--clr-text-3));
  }

  .mk-base {
    fill: color-mix(in srgb, var(--clr-text-3) 75%, var(--clr-bg-3));
  }

  .mk-knockout {
    fill: var(--clr-bg-1);
  }

  .mk-conflict {
    fill: none;
    stroke: var(--clr-danger);
    stroke-width: 1.8;
    stroke-linecap: round;
  }

  .id {
    flex-shrink: 0;
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .id b {
    color: var(--clr-accent-strong);
    font-weight: 600;
  }

  .base .id b {
    color: var(--clr-text-2);
  }

  /* jj colors divergent change ids like an error: the id no longer names
     one commit. */
  .id.divergent,
  .id.divergent .qq {
    color: var(--clr-danger);
  }

  .id.divergent .qq {
    font-weight: 600;
  }

  .divergent-commit {
    flex-shrink: 0;
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .desc {
    min-width: 40px;
    flex-shrink: 1;
    font-size: var(--text-m);
    color: var(--clr-text-2);
  }

  .hot .desc {
    color: var(--clr-text-1);
  }

  .base .desc {
    color: var(--clr-text-3);
  }

  .desc.undescribed {
    color: var(--clr-text-3);
    font-style: italic;
  }

  .tags {
    display: flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
    min-width: 0;
    overflow: hidden;
  }

  .chip {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    flex-shrink: 0;
    height: 16px;
    font-size: var(--text-xs);
    border-radius: 999px;
    padding: 0 7px;
    white-space: nowrap;
  }

  .chip.wc {
    background: var(--clr-working-copy-dim);
    color: var(--clr-working-copy);
  }

  .chip.conflict {
    background: color-mix(in srgb, var(--clr-danger) 14%, transparent);
    color: var(--clr-danger);
  }

  .chip.bm {
    background: var(--clr-bg-3);
    color: var(--clr-text-2);
    border: 1px solid var(--clr-border-2);
  }

  .chip.bm.trunk {
    background: var(--clr-accent-dim);
    border-color: color-mix(in srgb, var(--clr-accent) 30%, transparent);
    color: var(--clr-accent-strong);
  }

  .bm-name {
    max-width: 96px;
  }

  .sync {
    font-style: normal;
    font-size: 9px;
  }

  .sync.ok {
    color: var(--clr-ok);
  }

  .sync.warn {
    color: var(--clr-warn);
  }

  .sync.danger {
    color: var(--clr-danger);
  }

  .sync.muted {
    color: var(--clr-text-3);
  }

  .chip.behind {
    background: color-mix(in srgb, var(--clr-warn) 12%, transparent);
    color: var(--clr-warn);
  }

  .empty-note {
    font-size: var(--text-xs);
    color: var(--clr-text-3);
    font-style: italic;
  }

  .meta {
    display: inline-flex;
    align-items: baseline;
    gap: 5px;
    flex-shrink: 0;
    margin-left: auto;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  /* The description owns the row; long author names give way first, and
     disappear entirely when the graph pane is squeezed. */
  .author {
    max-width: 6.5em;
  }

  @container graph-pane (max-width: 600px) {
    .author {
      display: none;
    }
  }

  .age {
    min-width: 2.2em;
    text-align: right;
  }
</style>
