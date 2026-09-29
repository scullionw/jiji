<script lang="ts">
  import { onMount, tick } from "svelte";
  import { tinykeys } from "tinykeys";
  import SplitPane from "$lib/components/shell/SplitPane.svelte";
  import EmptyState from "$lib/components/ui/EmptyState.svelte";
  import Icon from "$lib/components/ui/Icon.svelte";
  import GraphView from "$lib/components/graph/GraphView.svelte";
  import FocusView from "$lib/components/graph/FocusView.svelte";
  import ViewMenu from "$lib/components/graph/ViewMenu.svelte";
  import DragOverlay from "$lib/components/graph/DragOverlay.svelte";
  import { density, toggleDensity } from "$lib/components/graph/density.svelte";
  import { attachRowDnd, drag } from "$lib/components/graph/dnd.svelte";
  import { laneBudget } from "$lib/components/graph/lanes.svelte";
  import {
    buildFocusModel,
    buildGraphModel,
    emphasizedStreamId,
    fullLanesFor,
    laneMap,
    nodeRowHeight,
    resolveFocusedWorkstream,
    selectableIds,
    streamOfNode,
  } from "$lib/components/graph/graph";
  import {
    extendRange,
    rangeCompareFrom,
    validRange,
    type RangeSelection,
  } from "$lib/components/graph/selection";
  import DiffView from "$lib/components/diff/DiffView.svelte";
  import { currentDiff } from "$lib/components/diff/current.svelte";
  import {
    findNode,
    isAncestor,
    stackPosition,
  } from "$lib/components/inspector/inspect";
  import Inspector from "$lib/components/views/Inspector.svelte";
  import { app } from "$lib/state/app.svelte";
  import { consumeIntent, jumpToChange, sendIntent } from "$lib/state/actions";

  // Parent only renders this view when a snapshot exists.
  const snapshot = $derived(app.snapshot!);
  const model = $derived(buildGraphModel(snapshot));
  // One stream, one hue: every stream-colored surface shares this map.
  const lanes = $derived(laneMap(snapshot.workstreams));
  const emphasized = $derived(
    emphasizedStreamId(model, snapshot, app.selectedNodeId, app.focusedWorkstreamId),
  );
  const selectedNode = $derived(
    app.selectedNodeId ? findNode(snapshot, app.selectedNodeId) : undefined,
  );

  // The ⇧-click range, gated on still being coherent: anchored on the
  // selection, contiguous in its lane, with a diff base. The stored state
  // can briefly outlive a snapshot refresh or selection move; rendering
  // reads only this derived, and the cleanup effect below drops the stale
  // storage in the same flush.
  const range = $derived.by((): RangeSelection | null => {
    const ids = app.selectedRange;
    if (!ids || !app.selectedNodeId || !ids.includes(app.selectedNodeId)) {
      return null;
    }
    if (!validRange(snapshot, ids)) return null;
    const from = rangeCompareFrom(snapshot, ids);
    return from ? { ids, from } : null;
  });
  // The diff pane's subject: the range's head (its diff runs base → head),
  // else the single selection. validRange guarantees the head is drawn.
  const diffNode = $derived(
    range ? findNode(snapshot, range.ids[0]) : selectedNode,
  );
  // Row highlight covers the whole run, not just the anchor.
  const selectedIds = $derived(
    new Set(range?.ids ?? (app.selectedNodeId ? [app.selectedNodeId] : [])),
  );

  // Two ways to read the same workstreams: the whole tree at once, or one
  // lane at a time. A reading preference, not per-repo state — it persists
  // across sessions, like the diff layout.
  type WorkbenchViewMode = "graph" | "focus";
  const VIEW_KEY = "jiji.workbench.view";
  let view = $state<WorkbenchViewMode>(
    localStorage.getItem(VIEW_KEY) === "focus" ? "focus" : "graph",
  );

  function setView(next: WorkbenchViewMode) {
    view = next;
    localStorage.setItem(VIEW_KEY, next);
  }

  // The command palette's view-mode commands land here.
  $effect(() => {
    const intent = app.intent;
    if (intent?.kind === "view") {
      setView(intent.view);
      consumeIntent();
    }
  });

  const focused = $derived(
    resolveFocusedWorkstream(snapshot, app.focusedWorkstreamId),
  );
  const focusModel = $derived(
    view === "focus" && focused ? buildFocusModel(snapshot, focused) : null,
  );
  const siblings = $derived(
    snapshot.workstreams.filter((w) => w.id !== focused?.id),
  );
  const ids = $derived(
    view === "focus"
      ? focusModel
        ? selectableIds(focusModel.graph)
        : []
      : selectableIds(model),
  );

  // The selections one arrow-key step away; the diff surface prefetches
  // them so walking the graph swaps diffs without a fetch wait.
  const neighborIds = $derived.by(() => {
    if (!app.selectedNodeId) return [];
    const index = ids.indexOf(app.selectedNodeId);
    if (index === -1) return [];
    return [ids[index + 1], ids[index - 1]].filter((id) => id !== undefined);
  });

  // Jumping from the diff header can land on a change in another
  // workstream; refocus that lane so the selection stays visible.
  function jumpTo(id: string) {
    const owner = stackPosition(snapshot, id)?.workstream;
    if (owner && owner.id !== app.focusedWorkstreamId) {
      app.focusedWorkstreamId = owner.id;
    }
    app.selectedNodeId = id;
    app.selectedRange = null;
  }

  let container = $state<HTMLDivElement | undefined>();

  // The gutter's lane budget follows the pane: however many columns deep
  // history needs, the leftmost lanes get full spacing only as far as the
  // row's text keeps its room; the rest compress. Measured off the
  // scroller so a pane drag re-budgets live.
  let paneWidth = $state(0);
  $effect(() => {
    if (!container || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver((entries) => {
      paneWidth = entries[0]?.contentRect.width ?? 0;
    });
    observer.observe(container);
    return () => observer.disconnect();
  });
  $effect(() => {
    const columns =
      view === "focus"
        ? (focusModel?.graph.columnCount ?? 1)
        : model.columnCount;
    laneBudget.full = fullLanesFor(paneWidth || 400, columns);
  });

  // Row drag-and-drop: one delegated controller on the pane's scroller
  // covers both view modes, since every row carries data-node-id. The
  // snapshot is read lazily at gesture time, so the controller survives
  // refreshes without rewiring.
  $effect(() => {
    if (!container) return;
    return attachRowDnd(container, () => app.snapshot);
  });

  // Snapshots refresh underneath the UI; drop selection/focus that no
  // longer resolves instead of rendering ghosts.
  $effect(() => {
    if (
      app.selectedNodeId &&
      !snapshot.nodes.some((n) => n.id === app.selectedNodeId)
    ) {
      app.selectedNodeId = null;
    }
    // A stored range that stopped being coherent (rendering already
    // ignores it — see the `range` derived) gets cleared for good.
    if (app.selectedRange && range === null) {
      app.selectedRange = null;
    }
    if (
      app.focusedWorkstreamId &&
      !snapshot.workstreams.some((w) => w.id === app.focusedWorkstreamId)
    ) {
      app.focusedWorkstreamId = null;
    }
  });

  // The annotate and evolution panes follow their subject: any selection
  // move — including annotate's own line-click jump — closes them, and a
  // ⇧-click range takes the surface, so neither lingers invisibly behind.
  $effect(() => {
    if (
      app.annotate &&
      (app.annotate.changeId !== app.selectedNodeId || app.selectedRange)
    ) {
      app.annotate = null;
    }
    if (
      app.evolution &&
      (app.evolution.changeId !== app.selectedNodeId || app.selectedRange)
    ) {
      app.evolution = null;
    }
  });

  // Keep the selected row visible no matter where the selection came from
  // (keyboard, row click, or an inspector jump). While the pane is
  // collapsed there is nothing to keep visible — but reading the flag here
  // means expanding it re-runs the scroll for the current selection.
  $effect(() => {
    const id = app.selectedNodeId;
    if (!id || app.graphCollapsed) return;
    tick().then(() => {
      container
        ?.querySelector(`[data-node-id="${CSS.escape(id)}"]`)
        ?.scrollIntoView({ block: "nearest" });
    });
  });

  // Selecting a row also makes its workstream the current one, so the
  // emphasis, inspector context, and graph all move together.
  //
  // ⇧-click while a change in the same lane is selected extends the
  // selection to the contiguous run between them; the diff pane renders
  // the run as one combined diff. A ⇧-click outside the anchor's lane
  // falls through to a plain select — the ranges this slice supports are
  // exactly the ones a lane spells out.
  //
  // ⌘-click while another change is selected compares instead of selects:
  // the diff pane measures the two changes against each other (the
  // header's "vs" comparison). The diff always reads forward — when the
  // clicked row is a descendant of the selection, the selection moves to
  // it and the old selection becomes the base, so picking the two ends in
  // either order shows old → new, never the reverse. ⌘-clicking the
  // active compare base again returns to the parent diff.
  function select(id: string, event?: MouseEvent) {
    const selected = app.selectedNodeId;
    if (event?.shiftKey && !event.metaKey && !event.ctrlKey && selected) {
      const run = extendRange(snapshot, selected, id);
      if (run) {
        app.selectedRange = run.length > 1 ? run : null;
        return;
      }
    }
    app.selectedRange = null;
    if ((event?.metaKey || event?.ctrlKey) && selected && selected !== id) {
      if ((currentDiff.diff?.from ?? null) === id) {
        sendIntent({ kind: "compare", mode: { kind: "parent" } });
      } else if (isAncestor(snapshot, selected, id)) {
        applySelection(id);
        sendIntent({ kind: "compare", mode: { kind: "change", id: selected } });
      } else {
        sendIntent({ kind: "compare", mode: { kind: "change", id } });
      }
      return;
    }
    applySelection(id);
  }

  function applySelection(id: string) {
    app.selectedNodeId = id;
    const stream = streamOfNode(model, id);
    if (stream) app.focusedWorkstreamId = stream;
  }

  function focusWorkstream(id: string) {
    app.focusedWorkstreamId = id;
    app.selectedNodeId = null;
    app.selectedRange = null;
  }

  function isEditable(target: EventTarget | null): boolean {
    const el = target as HTMLElement | null;
    return (
      !!el &&
      (el.tagName === "INPUT" ||
        el.tagName === "TEXTAREA" ||
        el.isContentEditable)
    );
  }

  // Whether a bare key is the workbench's to handle: nothing else owns
  // it (an overlay, a text field) and no command modifier is held.
  function bareKey(event: KeyboardEvent): boolean {
    if (event.defaultPrevented || app.paletteOpen || app.searchOpen || isEditable(event.target)) return false;
    return !(event.metaKey || event.ctrlKey || event.altKey);
  }

  // Move the selection to the row `to` picks from the current index (-1
  // with nothing selected), clamped to the rows on screen.
  function travel(event: KeyboardEvent, to: (index: number) => number) {
    if (!bareKey(event) || ids.length === 0) return;
    event.preventDefault();
    const index = app.selectedNodeId ? ids.indexOf(app.selectedNodeId) : -1;
    select(ids[Math.min(ids.length - 1, Math.max(0, to(index)))]);
  }

  // `delta` rows on from the selection; with nothing selected, stepping
  // down starts at the top and stepping up at the bottom.
  function stepBy(delta: number): (index: number) => number {
    return (index) =>
      index === -1 ? (delta > 0 ? 0 : ids.length - 1) : index + delta;
  }

  function moveSelection(event: KeyboardEvent, delta: number) {
    travel(event, stepBy(delta));
  }

  // Page Up/Down and Home/End belong to the pane the user is in: with
  // focus in the graph (a row click puts it on the scroller) they travel
  // the selection a screenful or to either end; anywhere else they keep
  // scrolling the diff natively.
  function travelInGraph(event: KeyboardEvent, to: (index: number) => number) {
    if (!container?.contains(document.activeElement)) return;
    travel(event, to);
  }

  function pageRows(): number {
    const rows = Math.floor((container?.clientHeight ?? 0) / nodeRowHeight(density.mode));
    return Math.max(1, rows - 1);
  }

  function pageSelection(event: KeyboardEvent, direction: number) {
    travelInGraph(event, stepBy(direction * pageRows()));
  }

  function switchView(event: KeyboardEvent, next: WorkbenchViewMode) {
    if (!bareKey(event)) return;
    event.preventDefault();
    setView(next);
  }

  onMount(() =>
    tinykeys(window, {
      ArrowDown: (event) => moveSelection(event, 1),
      ArrowUp: (event) => moveSelection(event, -1),
      KeyJ: (event) => moveSelection(event, 1),
      KeyK: (event) => moveSelection(event, -1),
      PageDown: (event) => pageSelection(event, 1),
      PageUp: (event) => pageSelection(event, -1),
      Home: (event) => travelInGraph(event, () => 0),
      End: (event) => travelInGraph(event, () => ids.length - 1),
      // jj's own name for the working copy: jump back to it from anywhere
      // in history, refocusing its workstream.
      "Shift+@": (event) => {
        if (!bareKey(event)) return;
        event.preventDefault();
        jumpToChange(snapshot.workingCopy);
      },
      KeyG: (event) => switchView(event, "graph"),
      KeyW: (event) => switchView(event, "focus"),
      Escape: (event) => {
        // An active drag owns Esc: it cancels the gesture, not the selection.
        if (!event.defaultPrevented && !app.paletteOpen && !app.searchOpen && !isEditable(event.target) && !drag.active) {
          // Staged exit: a pane over the diff (annotate or evolution)
          // closes back to it first, then a ⇧-click range collapses to
          // its anchor, then a comparison on screen leaves; the next Esc
          // clears the selection.
          if (app.annotate) {
            app.annotate = null;
          } else if (app.evolution) {
            app.evolution = null;
          } else if (app.selectedRange) {
            app.selectedRange = null;
          } else if (currentDiff.diff?.from != null) {
            sendIntent({ kind: "compare", mode: { kind: "parent" } });
          } else {
            app.selectedNodeId = null;
          }
        }
      },
    }),
  );

</script>

<!-- The graph owns the left pane; the continuous diff owns the right. The
     graph is the pane that gives way, so the diff keeps the width — up to
     giving way entirely (⌘⇧B) so the diff reads at full width. -->
<SplitPane
  id="graph"
  side="start"
  min={320}
  max={760}
  initial={400}
  collapsed={app.graphCollapsed}
>
  {#snippet panel()}
    <div class="graph-pane">
      <div class="pane-head">
        <ViewMenu {snapshot} />
        <div class="segmented view-toggle" role="group" aria-label="Workbench view">
          <button
            class:active={view === "graph"}
            aria-pressed={view === "graph"}
            title="Graph — every workstream in one tree (G)"
            onclick={() => setView("graph")}
          >
            Graph
          </button>
          <button
            class:active={view === "focus"}
            aria-pressed={view === "focus"}
            title="Focus — one workstream at a time (W)"
            onclick={() => setView("focus")}
          >
            Focus
          </button>
        </div>
        <button
          class="icon-btn"
          title={density.mode === "classic"
            ? "Classic rows — switch to compact, jj-dense rows"
            : "Compact rows — switch to larger, classic rows"}
          aria-label="Classic rows"
          aria-pressed={density.mode === "classic"}
          onclick={toggleDensity}
        >
          <Icon
            name={density.mode === "classic" ? "rowsClassic" : "rowsCompact"}
            size={14}
          />
        </button>
      </div>
      <!-- Focusable so a row click lands focus in the graph (WebKit does
           not focus clicked buttons; it focuses the nearest focusable
           ancestor), which is how Page Up/Down and Home/End know to
           travel the graph instead of scrolling the diff. -->
      <div class="scroller" bind:this={container} tabindex="-1">
        {#key view}
          <div class="view-body {view}">
            {#if view === "graph"}
              {#if model.rows.length > 0}
                <GraphView
                  scroller={container}
                  activeId={app.selectedNodeId}
                  {model}
                  {emphasized}
                  {lanes}
                  {selectedIds}
                  onselect={select}
                />
                {#if snapshot.truncated}
                  <p class="all-clear">
                    History goes deeper — this view stops at its newest 1,000
                    commits.
                  </p>
                {/if}
                {#if snapshot.workstreams.length === 0 && snapshot.view.kind === "workstreams"}
                  <p class="all-clear">
                    Everything here is already on trunk.
                    <span class="mono">jj new</span> starts a change.
                  </p>
                {/if}
              {:else}
                <EmptyState
                  icon="workbench"
                  title="Nothing to show"
                  body={snapshot.view.kind === "workstreams"
                    ? "This repository has no visible changes yet."
                    : "Nothing matches this view."}
                  hint="jj new"
                />
              {/if}
            {:else if focused && focusModel}
              <FocusView
                scroller={container}
                activeId={app.selectedNodeId}
                model={focusModel}
                workstream={focused}
                {siblings}
                {lanes}
                {selectedIds}
                onselect={select}
                onfocus={focusWorkstream}
              />
            {:else}
              <EmptyState
                icon="workbench"
                title="No mutable work"
                body="Everything here is already on trunk. New changes will show up as a workstream."
                hint="jj new"
              />
            {/if}
          </div>
        {/key}
      </div>
    </div>
  {/snippet}
  {#snippet children()}
    {#if diffNode}
      <div class="diff-pane">
        <DiffView
          {snapshot}
          node={diffNode}
          {range}
          neighbors={neighborIds}
          onjump={jumpTo}
          oncollapse={() => (app.selectedRange = null)}
          onclose={() => {
            app.selectedNodeId = null;
            app.selectedRange = null;
          }}
        />
      </div>
    {:else}
      <div class="detail-pane">
        <Inspector />
      </div>
    {/if}
  {/snippet}
</SplitPane>

<!-- The pointer-following plan card while a row is in hand. -->
<DragOverlay {snapshot} />

<style>
  .diff-pane {
    height: 100%;
    overflow: hidden;
    container: diff-pane / inline-size;
  }

  .detail-pane {
    height: 100%;
    overflow-y: auto;
    container: detail-pane / inline-size;
  }

  .graph-pane {
    height: 100%;
    display: flex;
    flex-direction: column;
    container: graph-pane / inline-size;
  }

  /* The pane's toolbar shares its height and hairline with the diff's
     header bar, so one line runs across the whole window. */
  .pane-head {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: var(--toolbar-h);
    padding: 0 var(--sp-2) 0 var(--sp-3);
    border-bottom: 1px solid var(--clr-border-2);
  }

  .scroller {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    outline: none;
  }

  .view-body.graph {
    padding: var(--sp-1) 0 var(--sp-6);
  }

  .view-body.focus {
    padding: var(--sp-3) var(--sp-3) var(--sp-6);
  }

  .all-clear {
    padding: var(--sp-3) var(--sp-4);
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .all-clear .mono {
    color: var(--clr-text-2);
  }
</style>
