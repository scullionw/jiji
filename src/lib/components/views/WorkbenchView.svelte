<script lang="ts">
  import { onMount, tick } from "svelte";
  import { tinykeys } from "tinykeys";
  import SplitPane from "$lib/components/shell/SplitPane.svelte";
  import EmptyState from "$lib/components/ui/EmptyState.svelte";
  import Icon from "$lib/components/ui/Icon.svelte";
  import GraphView from "$lib/components/graph/GraphView.svelte";
  import FocusView from "$lib/components/graph/FocusView.svelte";
  import DragOverlay from "$lib/components/graph/DragOverlay.svelte";
  import { attachRowDnd, drag } from "$lib/components/graph/dnd.svelte";
  import {
    buildFocusModel,
    buildGraphModel,
    emphasizedStreamId,
    resolveFocusedWorkstream,
    selectableIds,
    streamOfNode,
  } from "$lib/components/graph/graph";
  import DiffView from "$lib/components/diff/DiffView.svelte";
  import { findNode, stackPosition } from "$lib/components/inspector/inspect";
  import Inspector from "$lib/components/views/Inspector.svelte";
  import { app } from "$lib/state/app.svelte";
  import { consumeIntent } from "$lib/state/actions";
  import { viewIn } from "$lib/motion";

  // Parent only renders this view when a snapshot exists.
  const snapshot = $derived(app.snapshot!);
  const model = $derived(buildGraphModel(snapshot));
  const emphasized = $derived(
    emphasizedStreamId(model, snapshot, app.selectedNodeId, app.focusedWorkstreamId),
  );
  const selectedNode = $derived(
    app.selectedNodeId ? findNode(snapshot, app.selectedNodeId) : undefined,
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

  // Jumping from the diff header can land on a change in another
  // workstream; refocus that lane so the selection stays visible.
  function jumpTo(id: string) {
    const owner = stackPosition(snapshot, id)?.workstream;
    if (owner && owner.id !== app.focusedWorkstreamId) {
      app.focusedWorkstreamId = owner.id;
    }
    app.selectedNodeId = id;
  }

  let container = $state<HTMLDivElement | undefined>();

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
    if (
      app.focusedWorkstreamId &&
      !snapshot.workstreams.some((w) => w.id === app.focusedWorkstreamId)
    ) {
      app.focusedWorkstreamId = null;
    }
  });

  // Keep the selected row visible no matter where the selection came from
  // (keyboard, row click, or an inspector jump).
  $effect(() => {
    const id = app.selectedNodeId;
    if (!id) return;
    tick().then(() => {
      container
        ?.querySelector(`[data-node-id="${CSS.escape(id)}"]`)
        ?.scrollIntoView({ block: "nearest" });
    });
  });

  // Selecting a row also makes its workstream the current one, so the
  // emphasis, inspector context, and graph all move together.
  function select(id: string) {
    app.selectedNodeId = id;
    const stream = streamOfNode(model, id);
    if (stream) app.focusedWorkstreamId = stream;
  }

  function focusWorkstream(id: string) {
    app.focusedWorkstreamId = id;
    app.selectedNodeId = null;
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

  function moveSelection(event: KeyboardEvent, delta: number) {
    if (app.paletteOpen || isEditable(event.target)) return;
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    if (ids.length === 0) return;
    event.preventDefault();
    const index = app.selectedNodeId ? ids.indexOf(app.selectedNodeId) : -1;
    const next =
      index === -1
        ? delta > 0
          ? 0
          : ids.length - 1
        : Math.min(ids.length - 1, Math.max(0, index + delta));
    select(ids[next]);
  }

  function switchView(event: KeyboardEvent, next: WorkbenchViewMode) {
    if (app.paletteOpen || isEditable(event.target)) return;
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    event.preventDefault();
    setView(next);
  }

  onMount(() =>
    tinykeys(window, {
      ArrowDown: (event) => moveSelection(event, 1),
      ArrowUp: (event) => moveSelection(event, -1),
      KeyJ: (event) => moveSelection(event, 1),
      KeyK: (event) => moveSelection(event, -1),
      KeyG: (event) => switchView(event, "graph"),
      KeyW: (event) => switchView(event, "focus"),
      Escape: (event) => {
        // An active drag owns Esc: it cancels the gesture, not the selection.
        if (!app.paletteOpen && !isEditable(event.target) && !drag.active) {
          app.selectedNodeId = null;
        }
      },
    }),
  );

  const streamCount = $derived(snapshot.workstreams.length);
</script>

<!-- The graph owns the left pane; the continuous diff owns the right. The
     graph is the pane that gives way, so the diff keeps the width. -->
<SplitPane id="graph" side="start" min={340} max={760} initial={460}>
  {#snippet panel()}
    <div class="graph-pane">
      <div class="pane-head">
        <span class="pane-label">
          {streamCount === 0
            ? "No workstreams"
            : `${streamCount} workstream${streamCount === 1 ? "" : "s"}`}
        </span>
        <div class="view-toggle" role="group" aria-label="Workbench view">
          <button
            class:active={view === "graph"}
            title="Graph — every workstream in one tree (G)"
            onclick={() => setView("graph")}
          >
            <Icon name="branch" size={12} />
            Graph
          </button>
          <button
            class:active={view === "focus"}
            title="Focus — one workstream at a time (W)"
            onclick={() => setView("focus")}
          >
            <Icon name="stack" size={12} />
            Focus
          </button>
        </div>
      </div>
      <div class="scroller" bind:this={container}>
        {#key view}
          <div class="view-body {view}" in:viewIn>
            {#if view === "graph"}
              {#if model.rows.length > 0}
                <GraphView
                  {model}
                  {emphasized}
                  selectedId={app.selectedNodeId}
                  onselect={select}
                />
                {#if snapshot.workstreams.length === 0}
                  <p class="all-clear">
                    Everything here is already on trunk.
                    <span class="mono">jj new</span> starts a change.
                  </p>
                {/if}
              {:else}
                <EmptyState
                  icon="workbench"
                  title="Nothing to show"
                  body="This repository has no visible changes yet."
                  hint="jj new"
                />
              {/if}
            {:else if focused && focusModel}
              <FocusView
                model={focusModel}
                workstream={focused}
                {siblings}
                selectedId={app.selectedNodeId}
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
    {#if selectedNode}
      <div class="diff-pane">
        <DiffView
          {snapshot}
          node={selectedNode}
          onjump={jumpTo}
          onclose={() => (app.selectedNodeId = null)}
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
  }

  .detail-pane {
    height: 100%;
    overflow-y: auto;
  }

  .graph-pane {
    height: 100%;
    display: flex;
    flex-direction: column;
    container: graph-pane / inline-size;
  }

  .pane-head {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    height: 36px;
    padding: 0 var(--sp-3);
    border-bottom: 1px solid var(--clr-border-2);
  }

  .pane-label {
    font-size: var(--text-xs);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: var(--clr-text-3);
  }

  .view-toggle {
    flex-shrink: 0;
    display: inline-flex;
    border: 1px solid var(--clr-border-2);
    border-radius: 999px;
    overflow: hidden;
  }

  .view-toggle button {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 2px 9px;
    font-size: var(--text-xs);
    font-weight: 500;
    color: var(--clr-text-3);
    transition: all var(--t-fast) var(--ease-out);
  }

  .view-toggle button + button {
    border-left: 1px solid var(--clr-border-2);
  }

  .view-toggle button:hover {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .view-toggle button.active {
    background: var(--clr-bg-3);
    color: var(--clr-text-1);
  }

  .scroller {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }

  .view-body.graph {
    padding: var(--sp-3) 0 var(--sp-6);
  }

  .view-body.focus {
    padding: var(--sp-4) var(--sp-5) var(--sp-6);
  }

  .all-clear {
    padding: var(--sp-4);
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .all-clear .mono {
    color: var(--clr-text-2);
    background: var(--clr-bg-3);
    border-radius: var(--radius-s);
    padding: 1px 5px;
  }
</style>
