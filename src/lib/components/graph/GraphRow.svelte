<script lang="ts">
  import { cubicOut } from "svelte/easing";
  import { Tween } from "svelte/motion";
  import { SvelteMap } from "svelte/reactivity";
  import Icon from "$lib/components/ui/Icon.svelte";
  import { prBadge, prForBookmark } from "$lib/components/publish/pr";
  import { GRAPH_MOTION_MS, motionMs } from "$lib/motion";
  import { forge } from "$lib/state/forge.svelte";
  import { shortAge } from "$lib/time";
  import { currentDiff } from "$lib/components/diff/current.svelte";
  import { density } from "./density.svelte";
  import { drag } from "./dnd.svelte";
  import { laneBudget } from "./lanes.svelte";
  import { rewritePreview } from "./preview.svelte";
  import {
    keyedRails,
    laneTier,
    laneVar,
    nodeRowHeight,
    railInPath,
    railOutPath,
    railX,
    SYNC_GLYPH,
    type KeyedRail,
    type NodeRow,
    type Rail,
  } from "./graph";

  let {
    row,
    gutter,
    emphasized,
    lanes,
    selected = false,
    onselect,
  }: {
    row: NodeRow;
    /** Gutter width in px; may be fractional mid-morph — the view tweens it. */
    gutter: number;
    /** Workstream id rendered hot; other streams render calm. */
    emphasized: string | null;
    /** Stream id → lane slot; rails and markers wear their stream's hue. */
    lanes: Map<string, number>;
    selected?: boolean;
    /** Receives the click, so ⌘-click can compare instead of select. */
    onselect: (event: MouseEvent) => void;
  } = $props();

  const node = $derived(row.node);
  const isBase = $derived(node.kind === "immutable");
  const tone = $derived(
    isBase ? "base" : row.stream !== null && row.stream === emphasized ? "hot" : "calm",
  );
  const title = $derived(node.description.split("\n")[0] ?? "");

  // Classic mode trades jj's dense one-liners for the taller two-line rows
  // of traditional Git clients; the gutter geometry and markers scale with
  // the row so the tree keeps its proportions.
  const classic = $derived(density.mode === "classic");
  const H = $derived(nodeRowHeight(density.mode));
  const CY = $derived(H / 2);
  const gw = $derived(gutter);

  // Lanes past the pane's full-spacing budget draw in the narrow band:
  // hairline rails, small markers. A node there still gets its whole text
  // row — only its marker is compact — and past the band's end it shares
  // the last slot with whatever else overflowed, knocked out from the
  // rails so it never reads as sitting on a neighbor's line.
  const full = $derived(laneBudget.full);
  const tier = $derived(laneTier(row.column, full));
  const narrowNode = $derived(tier !== "full");
  /** Marker radius: mutable dot, and half-size of the base diamond. */
  const mr = $derived(narrowNode ? (classic ? 3 : 2.5) : classic ? 5 : 4);
  // Clearance around the marker so rails do not pierce hollow glyphs.
  const clear = $derived(
    narrowNode
      ? mr + 2
      : (row.isWorkingCopy ? 7 : 6) + (classic ? 1 : 0),
  );

  // The gutter morphs instead of swapping: rows survive rewrites (keyed by
  // change id), so the node marker and each rail (keyed by role + stream,
  // not column) tween sideways to their new columns and the layout change
  // reads as movement. Rails are drawn from the tweened positions every
  // frame; a brand-new rail starts at its target (nothing to slide from).
  // svelte-ignore state_referenced_locally — the initial value is meant to
  // be captured once; the effect below tweens every later change.
  const nxT = new Tween(railX(row.column, laneBudget.full), {
    easing: cubicOut,
  });
  $effect(() => {
    nxT.set(railX(row.column, full), { duration: motionMs(GRAPH_MOTION_MS) });
  });
  const nx = $derived(nxT.current);

  const rails = $derived(keyedRails(row));
  const railXs = new SvelteMap<string, Tween<number>>();
  $effect(() => {
    for (const { key, rail } of rails) {
      const target = railX(rail.col, full);
      const existing = railXs.get(key);
      if (!existing) {
        railXs.set(key, new Tween(target, { easing: cubicOut }));
      } else if (existing.target !== target) {
        existing.set(target, { duration: motionMs(GRAPH_MOTION_MS) });
      }
    }
    for (const key of railXs.keys()) {
      if (!rails.some((kr) => kr.key === key)) railXs.delete(key);
    }
  });

  function railD(kr: KeyedRail): string {
    const x = railXs.get(kr.key)?.current ?? railX(kr.rail.col, full);
    if (kr.role === "in") return railInPath(x, nx, H, clear);
    if (kr.role === "out") return railOutPath(x, nx, H, clear);
    return `M ${x} 0 V ${H}`;
  }

  function railTone(rail: Rail): string {
    if (rail.stream === null) return "base";
    return rail.stream === emphasized ? "hot" : "calm";
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

  // The hover-scrub: rows the pending rewrite would touch light up while a
  // drag or plan panel is scrubbing destinations. The row in hand already
  // dims, so it stays out; a picked panel destination wears the same ring
  // a drag target does.
  const planDest = $derived(
    !drag.active && rewritePreview.destination === node.id,
  );
  const affected = $derived(
    rewritePreview.ids.has(node.id) && !isDragSource && !planDest,
  );

  // The two ends of an active comparison, marked from the diff actually
  // on screen (set by ⌘-click or the header's compare panel): the base is
  // the row the diff is measured from, and the subject — normally the
  // selection — is marked too, so the pair reads at a glance.
  const comparison = $derived(
    currentDiff.diff?.from != null ? currentDiff.diff : null,
  );
  const isCompareBase = $derived(comparison?.from === node.id);
  const isCompareTo = $derived(comparison?.id === node.id);
</script>

<button
  class="row {tone}"
  class:classic
  class:selected
  class:base={isBase}
  class:wc={row.isWorkingCopy}
  class:lane-narrow={narrowNode}
  class:drag-source={isDragSource}
  class:drop-ok={dropOk || planDest}
  class:drop-no={isDropTarget && !dropOk}
  class:affected
  class:compare-base={isCompareBase}
  class:compare-to={isCompareTo}
  style:--lane={laneVar(lanes, row.stream)}
  style:height="{H}px"
  data-node-id={node.id}
  data-kind={node.kind}
  data-stream={row.stream}
  aria-pressed={selected}
  onclick={onselect}
>
  <span class="gutter" style:width="{gw}px">
    <svg width={gw} height={H} viewBox="0 0 {gw} {H}" aria-hidden="true">
      {#each rails as kr (kr.key)}
        {@const railTier = laneTier(kr.rail.col, full)}
        <path
          class="rail {railTone(kr.rail)}"
          class:elided={kr.rail.elided}
          class:narrow={railTier !== "full"}
          class:overflow={railTier === "overflow"}
          style:--lane={laneVar(lanes, kr.rail.stream)}
          d={railD(kr)}
        />
      {/each}
      {#if stubBelow}
        <path class="rail stub" class:narrow={narrowNode} d="M {nx} {CY + clear} V {H}" />
      {/if}

      {#if tier === "overflow"}
        <circle class="mk-knockout" cx={nx} cy={CY} r={mr + 2} />
      {/if}
      {#if row.isWorkingCopy}
        <text class="mk-wc" x={nx} y={CY + 1}>@</text>
      {:else if node.hasConflict}
        {@const xr = mr - 0.8}
        <circle class="mk-knockout" cx={nx} cy={CY} r={mr + 1.5} />
        <path
          class="mk-conflict"
          d="M {nx - xr} {CY - xr} L {nx + xr} {CY + xr} M {nx + xr} {CY - xr} L {nx - xr} {CY + xr}"
        />
      {:else if isBase}
        <rect
          class="mk-base"
          x={nx - mr}
          y={CY - mr}
          width={mr * 2}
          height={mr * 2}
          rx="1.5"
          transform="rotate(45 {nx} {CY})"
        />
      {:else}
        <circle class="mk-change {tone}" cx={nx} cy={CY} r={mr} />
      {/if}
    </svg>
  </span>

  {#if classic}
    <span class="lines">
      <span class="line">
        {@render desc()}
        {@render tags()}
      </span>
      <span class="line">
        {@render ids()}
        {@render meta()}
      </span>
    </span>
  {:else}
    {@render ids()}
    {@render desc()}
    {@render tags()}
    {@render meta()}
  {/if}
</button>

{#snippet ids()}
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
{/snippet}

{#snippet desc()}
  <span class="desc truncate" class:undescribed={!title}>
    {title || "No description yet"}
  </span>
{/snippet}

{#snippet tags()}
  <span class="tags">
    {#if isCompareBase}
      <span class="ref diffbase" title="The diff pane measures the selection against this change">
        diff base
      </span>
    {/if}
    {#if node.hasConflict}
      <span class="ref conflict">conflict</span>
    {/if}
    {#each row.bookmarks as bookmark (`${bookmark.name}@${bookmark.remote ?? ""}:${bookmark.isLocal}`)}
      {@const sync = SYNC_GLYPH[bookmark.sync]}
      {@const pr = prForBookmark(bookmark, forge.prs)}
      <!-- Remote-only refs label with their remote and render muted: they
           are someone else's position, not a local bookmark. -->
      <span
        class="ref bm"
        class:trunk={bookmark.isTrunk}
        class:remote={!bookmark.isLocal}
        data-bookmark={bookmark.name}
        data-bookmark-remote={bookmark.remote ?? ""}
        title="{bookmark.name}{bookmark.remote ? `@${bookmark.remote}` : ''} — {sync.label}"
      >
        <span class="bm-name truncate">
          {bookmark.isLocal
            ? bookmark.name
            : `${bookmark.name}@${bookmark.remote}`}
        </span>
        {#if sync.glyph}<i class="sync {sync.tone}">{sync.glyph}</i>{/if}
      </span>
      {#if pr}
        {@const badge = prBadge(pr)}
        <!-- The bookmark's pull request, right beside its label. Passive
             here (rows are buttons); the change header links out. -->
        <span class="ref pr {badge.tone}" title={badge.title} data-pr={pr.number}>
          {badge.label}
          {#if badge.review}<i class="pg {badge.review.tone}">{badge.review.glyph}</i>{/if}
          {#if badge.checks}<i class="pg {badge.checks.tone}">{badge.checks.glyph}</i>{/if}
        </span>
      {/if}
    {/each}
    <!-- Colocated git tags, read-only: markers only, no tag management. -->
    {#each node.tags as tag (tag)}
      <span class="ref tag" data-tag={tag} title="git tag {tag}">
        <Icon name="tag" size={9} />
        <span class="bm-name truncate">{tag}</span>
      </span>
    {/each}
    {#if row.isStackHead && row.behindTrunk > 0}
      <span class="behind" title="{row.behindTrunk} change{row.behindTrunk === 1 ? '' : 's'} behind trunk">
        ↓{row.behindTrunk}
      </span>
    {/if}
    {#if node.isEmpty}
      <span class="empty-note">empty</span>
    {/if}
  </span>
{/snippet}

{#snippet meta()}
  <span class="meta" title="{node.author} · {node.timestamp}">
    <span class="author truncate">{node.author}</span>
    <span class="age">{shortAge(node.timestamp)}</span>
  </span>
{/snippet}

<style>
  .row {
    /* Height comes inline from the density mode, matching the gutter SVG. */
    position: relative;
    isolation: isolate;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    text-align: left;
    padding-right: var(--sp-3);
    /* What the marker knockouts paint, so a hollow node never shows a
       darker disc through a tinted row. */
    --row-fill: var(--clr-bg-1);
  }

  /* Row states paint edge to edge behind the content; the tree's rails
     run straight through them, like a table row, not a picked-up card. */
  .row::before {
    content: "";
    position: absolute;
    inset: 0;
    z-index: -1;
    transition: background var(--t-fast) var(--ease-out);
  }

  /* Classic mode stacks the row into two lines: subject and refs on top,
     identity and authorship beneath. */
  .lines {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .line {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }

  .row:hover {
    --row-fill: color-mix(in srgb, var(--clr-text-1) 4.5%, var(--clr-bg-1));
  }

  .row:hover::before {
    background: var(--clr-bg-hover);
  }

  /* The trunk/base zone reads recessed inside the same tree. */
  .row.base {
    --row-fill: var(--clr-bg-1);
  }

  .row.selected {
    --row-fill: color-mix(in srgb, var(--clr-accent) 17%, var(--clr-bg-1));
  }

  .row.selected::before {
    background: var(--clr-accent-dim);
  }

  /* An active comparison marks both endpoints on the row's leading edge,
     so the pair reads at a glance; the base's label names its role. */
  .row.compare-base::before {
    background: color-mix(in srgb, var(--clr-accent) 5%, transparent);
    box-shadow: inset 2px 0 0 var(--clr-accent);
  }

  .row.compare-to.selected::before {
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
  .row.drop-ok::before {
    background: var(--clr-accent-dim);
    box-shadow: inset 0 0 0 1px var(--clr-accent);
  }

  .row.drop-no::before {
    background: color-mix(in srgb, var(--clr-danger) 7%, transparent);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--clr-danger) 45%, transparent);
  }

  /* The hover-scrub wash: this row would be rewritten by the plan being
     scrubbed. Warm and quiet — informative, not alarming. */
  .row.affected::before {
    background: color-mix(in srgb, var(--clr-warn) 9%, transparent);
  }

  .row.affected:not(.selected)::before {
    box-shadow: inset 2px 0 0 color-mix(in srgb, var(--clr-warn) 70%, transparent);
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
    stroke-linecap: round;
  }

  .rail.hot {
    stroke: color-mix(in srgb, var(--lane, var(--clr-accent)) 80%, var(--clr-bg-1));
  }

  .rail.calm {
    stroke: color-mix(in srgb, var(--lane, var(--clr-accent)) 38%, var(--clr-bg-1));
  }

  .rail.base {
    stroke: color-mix(in srgb, var(--clr-text-3) 50%, var(--clr-bg-1));
  }

  .rail.stub {
    stroke: color-mix(in srgb, var(--clr-text-3) 50%, var(--clr-bg-1));
    stroke-dasharray: 2 3;
  }

  /* Edges crossing elided history read as interrupted. */
  .rail.elided {
    stroke-dasharray: 2 3;
  }

  /* The narrow band: hairlines, and past its end a shared, quieter slot. */
  .rail.narrow {
    stroke-width: 1;
  }

  .rail.overflow {
    opacity: 0.55;
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
    stroke: var(--row-fill);
    stroke-width: 3;
  }

  .classic .mk-wc {
    font-size: 15px;
  }

  .lane-narrow .mk-wc {
    font-size: 10px;
    stroke-width: 2;
  }

  .mk-change {
    fill: var(--row-fill);
    stroke-width: 1.5;
  }

  /* The selected node fills in: a solid dot reads as "this one" at any
     lane color, without a halo. */
  .selected .mk-change {
    stroke-width: 1.5;
    fill: var(--lane, var(--clr-accent));
  }

  .lane-narrow .mk-change {
    stroke-width: 1.2;
  }

  .lane-narrow .mk-conflict {
    stroke-width: 1.4;
  }

  .mk-change.hot {
    stroke: var(--lane, var(--clr-accent));
  }

  .mk-change.calm {
    stroke: color-mix(in srgb, var(--lane, var(--clr-accent)) 55%, var(--clr-text-3));
  }

  .mk-base {
    fill: color-mix(in srgb, var(--clr-text-3) 80%, var(--clr-bg-1));
  }

  .mk-knockout {
    fill: var(--row-fill);
  }

  .mk-conflict {
    fill: none;
    stroke: var(--clr-danger);
    stroke-width: 1.8;
    stroke-linecap: round;
  }

  /* jj's change id: the shortest unique prefix in full ink, the rest
     receding — the part you would actually type stands out. */
  .id {
    flex-shrink: 0;
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .id b {
    color: var(--clr-text-1);
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

  .hot .desc,
  .selected .desc {
    color: var(--clr-text-1);
  }

  .base .desc {
    color: var(--clr-text-3);
  }

  .desc.undescribed {
    color: var(--clr-text-3);
    font-style: italic;
  }

  /* The subject line carries the classic row; bases stay quiet. */
  .classic .desc {
    font-size: var(--text-l);
    font-weight: 500;
  }

  .classic.base .desc {
    font-weight: 400;
  }

  .tags {
    display: flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
    min-width: 0;
    overflow: hidden;
  }

  .ref.diffbase {
    font-family: var(--font-ui);
    color: var(--clr-accent-strong);
    background: var(--clr-accent-dim);
  }

  .ref.conflict {
    font-family: var(--font-ui);
    color: var(--clr-danger);
    background: var(--clr-danger-dim);
  }

  /* Trunk is the one ref the whole graph measures against. */
  .ref.bm.trunk {
    color: var(--clr-accent-strong);
    background: var(--clr-accent-dim);
  }

  /* Someone else's position: present but quiet. */
  .ref.bm.remote {
    background: transparent;
    padding: 0 2px;
    color: var(--clr-text-3);
  }

  .bm-name {
    max-width: 104px;
  }

  .ref.tag :global(svg) {
    color: var(--clr-text-3);
  }

  /* PR badge tones read like GitHub's states: open green, draft muted,
     merged accent, closed danger. */
  .ref.pr.open {
    color: var(--clr-ok);
    background: var(--clr-ok-dim);
  }

  .ref.pr.draft {
    color: var(--clr-text-3);
  }

  .ref.pr.merged {
    color: var(--clr-accent-strong);
    background: var(--clr-accent-dim);
  }

  .ref.pr.closed {
    color: var(--clr-danger);
    background: var(--clr-danger-dim);
  }

  .pg,
  .sync {
    font-style: normal;
    font-size: 10px;
  }

  .pg.ok,
  .sync.ok {
    color: var(--clr-ok);
  }

  .pg.warn,
  .sync.warn {
    color: var(--clr-warn);
  }

  .pg.danger,
  .sync.danger {
    color: var(--clr-danger);
  }

  .sync.muted {
    color: var(--clr-text-3);
  }

  .behind {
    flex-shrink: 0;
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    color: var(--clr-warn);
  }

  .empty-note {
    flex-shrink: 0;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .meta {
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
    flex-shrink: 0;
    margin-left: auto;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .classic .meta {
    font-size: var(--text-s);
  }

  /* The description owns the row; long author names give way first, and
     disappear entirely when the graph pane is squeezed. */
  .author {
    max-width: 6.5em;
  }

  @container graph-pane (max-width: 560px) {
    .author {
      display: none;
    }
  }

  .age {
    min-width: 2.2em;
    text-align: right;
  }
</style>
