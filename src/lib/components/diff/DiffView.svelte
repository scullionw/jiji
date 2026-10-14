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
  import type { RangeSelection } from "$lib/components/graph/selection";
  import { app } from "$lib/state/app.svelte";
  import {
    closeAnnotate,
    closeEvolution,
    consumeIntent,
    resolveConflict,
  } from "$lib/state/actions";
  import {
    canResolve,
    mergeToolLabel,
  } from "$lib/components/conflicts/conflicts";
  import Icon from "$lib/components/ui/Icon.svelte";
  import AnnotatePane from "./AnnotatePane.svelte";
  import ChangeHeader from "./ChangeHeader.svelte";
  import EvolutionPane from "./EvolutionPane.svelte";
  import FileDiffCard from "./FileDiffCard.svelte";
  import FileTree from "./FileTree.svelte";
  import type { DiffLayout } from "./diff";
  import { findInDiff, type FindMatch } from "./find";
  import { currentDiff } from "./current.svelte";
  import { cachedDiff, fetchDiff, prefetchDiff } from "./diffcache";
  import { observeViewport } from "$lib/viewport";

  let {
    snapshot,
    node,
    range = null,
    neighbors = [],
    onjump,
    oncollapse,
    onclose,
  }: {
    snapshot: RepoSnapshot;
    /// The diff's subject; with a range active, the range's head.
    node: GraphNode;
    /// A ⇧-click multi-selection: the pane shows one combined diff from
    /// the run's root parent to `node`, and the header states the range
    /// instead of a single change.
    range?: RangeSelection | null;
    /// Change ids one selection step away (arrow keys, adjacent clicks);
    /// their diffs are prefetched so walking the graph feels instant.
    neighbors?: string[];
    onjump: (id: string) => void;
    /// Collapse an active range back to its anchor (the header's ×).
    oncollapse?: () => void;
    onclose: () => void;
  } = $props();

  // What the diff is measured against. Preset modes are relative (trunk,
  // stack base), so walking the stack keeps the same comparison; a pinned
  // change comparison belongs to the pair it was picked for, and moving
  // the selection anywhere else drops back to the parent diff — otherwise
  // every later selection keeps comparing against a change that stopped
  // mattering, with no obvious way out. Transient reading state — it
  // lives only as long as something is selected.
  let compare = $state<CompareMode>({ kind: "parent" });
  // The selection a change-pinned comparison was picked for.
  let comparePinnedTo = $state("");
  // An active range owns the comparison: root parent → head, always.
  const compareFrom = $derived(
    range ? range.from : resolveCompareFrom(snapshot, node.id, compare),
  );

  function setCompare(mode: CompareMode) {
    compare = mode;
    comparePinnedTo = mode.kind === "change" ? node.id : "";
  }

  $effect(() => {
    const id = node.id;
    untrack(() => {
      if (compare.kind === "change" && comparePinnedTo !== id) {
        compare = { kind: "parent" };
        comparePinnedTo = "";
      }
    });
  });

  // Diffs are cached per repo operation (see diffcache.ts): re-selecting
  // a change renders synchronously, and snapshot republishes that record
  // no new operation (view switches, converged refreshes) skip the
  // backend entirely.
  const opId = $derived(
    snapshot.operations.find((o) => o.isCurrent)?.id ??
      snapshot.operations[0]?.id ??
      "",
  );

  // The diff arrives lazily; the header renders from the snapshot
  // immediately. Raw: diffs are immutable once fetched, and a deep proxy
  // would wrap every hunk, line and segment the rows walk.
  let diff = $state.raw<ChangeDiff | null>(null);
  let loadedOp = $state("");
  let loadedRepo = $state("");
  let error = $state<string | null>(null);
  let slow = $state(false);
  let retry = $state(0);
  let requestSeq = 0;

  $effect(() => {
    void retry;
    const id = node.id;
    const from = compareFrom;
    const op = opId; // refetch when a new operation lands
    const path = snapshot.repoPath;
    const token = ++requestSeq;
    error = null;
    const hit = cachedDiff(path, op, from, id);
    if (hit) {
      diff = hit;
      loadedOp = op;
      loadedRepo = path;
      slow = false;
      return;
    }
    const slowTimer = setTimeout(() => {
      if (token === requestSeq) slow = true;
    }, 200);
    fetchDiff(path, op, from, id).then(
      (result) => {
        if (token !== requestSeq) return;
        clearTimeout(slowTimer);
        diff = result;
        loadedOp = op;
        loadedRepo = path;
        slow = false;
      },
      (err) => {
        if (token !== requestSeq) return;
        clearTimeout(slowTimer);
        error = api.errorMessage(err);
        slow = false;
      },
    );
    return () => {
      requestSeq++;
      clearTimeout(slowTimer);
    };
  });

  const current = $derived(
    diff && diff.id === node.id && (diff.from ?? null) === compareFrom
      ? diff
      : null,
  );
  // A previous diff can remain visible during a fetch, but it must never
  // supply file mutations for a different selection, repo or operation.
  const fresh = $derived(current !== null && loadedOp === opId && loadedRepo === snapshot.repoPath);

  // What the body renders. While a switch's fetch is in flight the
  // previous diff stays on screen (dimmed) instead of blanking to the
  // skeleton — swapping content in one frame is what makes switching read
  // as instant. The skeleton appears only when the fetch outlasts the
  // slow timer or nothing has loaded yet. An error clears the surface —
  // except mid-refetch of the same comparison, where the stale content is
  // more useful than a transient auto-refresh failure.
  const displayed = $derived(error ? current : (current ?? (slow ? null : diff)));

  // Publish what's on screen for the context menu (patch copy, line
  // lookup); cleared when the diff surface unmounts. While the evolution
  // pane covers the surface it publishes its own step diff instead —
  // the slot returns here when the pane closes.
  $effect(() => {
    if (!evolutionOpen) currentDiff.diff = displayed;
  });
  $effect(() => {
    return () => {
      currentDiff.diff = null;
    };
  });

  // Warm the cache for the selections one step away once the current diff
  // is on screen, so arrow-walking the graph hits the cache. The beat of
  // delay keeps the visible diff's paint ahead of the prefetch responses.
  // Neighbors of a pinned comparison get their parent diff — moving the
  // selection exits the pinned pair, so that is what they will show.
  $effect(() => {
    if (!fresh) return;
    const op = opId;
    const repo = snapshot;
    const mode: CompareMode =
      compare.kind === "change" ? { kind: "parent" } : compare;
    const targets = neighbors;
    const timer = setTimeout(() => {
      for (const id of targets) {
        prefetchDiff(repo.repoPath, op, resolveCompareFrom(repo, id, mode), id);
      }
    }, 50);
    return () => clearTimeout(timer);
  });

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

  // The file tree: the diff's map, docked on the left. A reading
  // preference like the layout — it persists — and it only renders when
  // there is something to navigate (two or more files) and room to spare.
  const TREE_KEY = "jiji.diff.tree";
  let treeOn = $state(localStorage.getItem(TREE_KEY) !== "0");

  function setTree(on: boolean) {
    treeOn = on;
    localStorage.setItem(TREE_KEY, on ? "1" : "0");
  }

  // The tree's width, drag-adjustable like the shell's split panes and
  // persisted the same way.
  const TREE_WIDTH_KEY = "jiji.pane.filetree";
  const TREE_MIN = 160;
  const TREE_MAX = 400;
  let treeWidth = $state(
    (() => {
      const saved = Number(localStorage.getItem(TREE_WIDTH_KEY));
      return Number.isFinite(saved) && saved >= TREE_MIN && saved <= TREE_MAX
        ? saved
        : 220;
    })(),
  );
  let treeDragging = $state(false);

  function startTreeDrag(event: PointerEvent) {
    treeDragging = true;
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
  }

  function onTreeDrag(event: PointerEvent) {
    if (!treeDragging || !viewEl) return;
    const raw = event.clientX - viewEl.getBoundingClientRect().left;
    treeWidth = Math.min(TREE_MAX, Math.max(TREE_MIN, raw));
  }

  function endTreeDrag() {
    if (!treeDragging) return;
    treeDragging = false;
    localStorage.setItem(TREE_WIDTH_KEY, String(Math.round(treeWidth)));
  }

  // Below these pane widths the tree would squeeze the diff into a ribbon
  // (side-by-side needs room for two columns of code); it steps aside and
  // the header's file menu carries navigation.
  let viewEl = $state<HTMLDivElement | undefined>();
  let viewWidth = $state(Infinity);
  const narrow = $derived(viewWidth < (layout === "split" ? 1040 : 760));

  $effect(() => {
    const el = viewEl;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      viewWidth = el.clientWidth;
    });
    ro.observe(el);
    return () => ro.disconnect();
  });

  const treeVisible = $derived(
    treeOn && !narrow && displayed !== null && displayed.files.length > 1,
  );

  // The command palette's layout and file-tree commands land here, and so
  // does ⌘F (routed as an intent so the palette row and the shortcut share
  // one path).
  $effect(() => {
    const intent = app.intent;
    if (intent?.kind === "layout") {
      setLayout(intent.layout);
      consumeIntent();
    } else if (intent?.kind === "filetree") {
      setTree(!treeOn);
      consumeIntent();
    } else if (intent?.kind === "find") {
      openFind();
      consumeIntent();
    }
  });

  // Single-change verbs act on the anchor, and their panels live in
  // ChangeHeader — which a range replaces. Collapse the range so the
  // header mounts and consumes the intent; otherwise it would sit stale
  // and fire on some later selection.
  $effect(() => {
    const kind = app.intent?.kind;
    if (!range || !kind) return;
    if (
      kind === "describe" ||
      kind === "bookmark" ||
      kind === "rebase" ||
      kind === "split" ||
      kind === "squash" ||
      kind === "abandon" ||
      kind === "compare" ||
      kind === "evolog"
    ) {
      oncollapse?.();
    }
  });

  // ---- Find in diff (⌘F) ----
  // The query persists while the bar is open, across selections: walking
  // a stack with a term in hand is the point. Matches recompute from
  // whatever diff is on screen; `findCurrent` indexes into them and
  // `findSeq` bumps on each navigation so the owning card re-reveals even
  // when the target match stays the same.
  let findOpen = $state(false);
  let findQuery = $state("");
  let findCurrent = $state(0);
  let findSeq = $state(0);
  let findInputEl = $state<HTMLInputElement | undefined>();

  const findMatches = $derived(
    findOpen && displayed && findQuery.length > 0
      ? findInDiff(displayed.files, findQuery)
      : [],
  );

  $effect(() => {
    if (findCurrent >= findMatches.length) findCurrent = 0;
  });

  // Another diff on screen: matches carry over, the position resets.
  $effect(() => {
    void displayed;
    untrack(() => (findCurrent = 0));
  });

  function openFind(): void {
    // A text selection in hand becomes the query — the "find this" flow.
    const selection = window.getSelection()?.toString() ?? "";
    if (selection && !selection.includes("\n")) {
      findQuery = selection;
      findCurrent = 0;
    }
    findOpen = true;
    void tick().then(() => {
      findInputEl?.focus();
      findInputEl?.select();
    });
  }

  function closeFind(): void {
    findOpen = false;
  }

  // Step to a match: its file expands if collapsed (the reveal needs the
  // body), then the card scrolls it into view.
  function gotoFind(delta: number): void {
    const count = findMatches.length;
    if (count === 0) return;
    const next = (((findCurrent + delta) % count) + count) % count;
    const path = displayed?.files[findMatches[next].file]?.path;
    if (path && collapsedFiles.delete(path)) rewindowCards();
    findCurrent = next;
    findSeq++;
  }

  function onFindInput(): void {
    findCurrent = 0;
    findSeq++;
  }

  function onFindKeydown(event: KeyboardEvent): void {
    if (event.key === "Enter") {
      event.preventDefault();
      gotoFind(event.shiftKey ? -1 : 1);
    } else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      closeFind();
    } else if (event.key === "f" && (event.metaKey || event.ctrlKey) && !event.shiftKey) {
      // ⌘F in the open bar re-selects the query instead of re-opening.
      event.preventDefault();
      event.stopPropagation();
      findInputEl?.select();
    }
  }

  // What each card needs: its own matches, whether it holds the current
  // one, and the navigation beat. Built once per matches/current change.
  const matchesByFile = $derived.by(() => {
    const map = new Map<number, FindMatch[]>();
    for (const m of findMatches) {
      const list = map.get(m.file);
      if (list) list.push(m);
      else map.set(m.file, [m]);
    }
    return map;
  });

  // Memoized so identities are stable across unrelated re-renders (the
  // scroll-position bookkeeping): a fresh prop object would re-fire every
  // card's reveal effect and yank the scroll back to the current match.
  const findProps = $derived.by(() => {
    const map = new Map<
      number,
      { matches: FindMatch[]; active: number; seq: number }
    >();
    if (!findOpen) return map;
    const current = findMatches[findCurrent];
    for (const [file, matches] of matchesByFile) {
      map.set(file, {
        matches,
        active:
          current && current.file === file ? matches.indexOf(current) : -1,
        seq: findSeq,
      });
    }
    return map;
  });

  let scroller = $state<HTMLDivElement | undefined>();

  // Which file section sits under the reading line (the scroller's top
  // edge): the tree's active row. Sections tile the scroll height, so the
  // last one starting above the edge is the one being read.
  let activeFile = $state(0);

  function updateActiveFile(view?: { top: number }) {
    const el = scroller;
    if (!el) return;
    const edge = (view?.top ?? el.getBoundingClientRect().top) + 2;
    const sections = el.querySelectorAll<HTMLElement>("[data-file-index]");
    let start = 0;
    let end = sections.length;
    while (start < end) {
      const mid = (start + end) >>> 1;
      if (sections[mid].getBoundingClientRect().top <= edge) start = mid + 1;
      else end = mid;
    }
    activeFile = Math.max(0, start - 1);
  }

  $effect(() => {
    const el = scroller;
    if (!el) return;
    return observeViewport(el, updateActiveFile);
  });

  // Content swaps (new diff, collapse toggles) move the sections without
  // scrolling; re-read once the DOM settles.
  $effect(() => {
    void displayed;
    void tick().then(() => updateActiveFile());
  });

  // Reading position and per-file collapse are per-comparison; selecting
  // another change or switching what it is measured against starts at the
  // top with every file expanded. Keyed on the comparison whose content
  // is on screen — the previous diff keeps its scroll position while the
  // next one is still in flight, and the reset lands in the same flush
  // as the content swap.
  const collapsedFiles = new SvelteSet<string>();
  let resetFor = "";

  $effect(() => {
    if (!current) return;
    const key = `${current.from ?? ""} ${current.id}`;
    if (key === resetFor) return;
    resetFor = key;
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

  // Resolve on conflicted file headers: hands the file to the external
  // merge tool, same flow and waiting state as the conflict inbox. Only on
  // the change's own diff — a comparison span can mark files conflicted at
  // its endpoints, but the action rewrites the selection.
  const resolveTool = $derived(
    compareFrom === null && snapshot.resolveTool && canResolve(snapshot, node.id)
      ? mergeToolLabel(snapshot.resolveTool)
      : null,
  );
  let resolveError = $state<string | null>(null);

  // Cleared when the selection moves to another change; keyed on the id
  // string because refreshes pass a new node object for the same change,
  // and an unrelated watcher tick must not wipe the error unread.
  const nodeId = $derived(node.id);
  $effect(() => {
    void nodeId;
    untrack(() => (resolveError = null));
  });

  async function resolveFile(path: string) {
    resolveError = null;
    try {
      await resolveConflict(node.id, path);
    } catch (error) {
      resolveError = api.errorMessage(error);
    }
  }

  async function jumpToFile(index: number) {
    const path = current?.files[index]?.path;
    if (path && collapsedFiles.delete(path)) await tick();
    scroller
      ?.querySelector(`[data-file-index="${index}"]`)
      ?.scrollIntoView({ block: "start" });
    rewindowCards();
  }

  // The annotate and evolution panes replace the diff surface (tree,
  // cards, find bar) while their subject stays selected; the workbench
  // clears the state on any selection move, and a range never coexists
  // with either. Opening one closes the other (actions.ts), so at most
  // one renders.
  const annotatePath = $derived(
    !range && app.annotate?.changeId === node.id ? app.annotate.path : null,
  );
  const evolutionOpen = $derived(
    !range && app.evolution?.changeId === node.id,
  );
</script>

<!-- The continuous multi-file diff for the current selection: every changed
     file in one scroll, with the file tree as its map and navigation, not
     a gate. -->
<!-- data-diff-change on the root names the change for context-menu
     targets outside the scroller (the file tree's rows). -->
<div class="diff-view" bind:this={viewEl} data-diff-change={fresh ? node.id : undefined}>
  {#if range}
    <!-- A range owns the header: what run is shown and what it is measured
         from, in place of the single-change details. Single-change verbs
         return with the anchor when the range collapses (esc / ×). -->
    {@const rootId = range.ids[range.ids.length - 1]}
    <header class="range-header">
      <Icon name="stack" size={13} />
      <span class="range-count">{range.ids.length} changes as one diff</span>
      <span
        class="range-span mono"
        title="Everything these {range.ids.length} changes add together — measured from {range.from.slice(0, 8)}, the parent of {rootId.slice(0, 8)}"
      >
        <b>{rootId.slice(0, 2)}</b>{rootId.slice(2, 8)}
        <span class="range-arrow">→</span>
        <b>{node.id.slice(0, 2)}</b>{node.id.slice(2, 8)}
      </span>
      <span class="range-note">vs {range.from.slice(0, 8)}</span>
      <button
        class="icon-btn"
        class:on={treeOn}
        aria-pressed={treeOn}
        title={treeOn ? "Hide the file tree" : "Show the file tree"}
        onclick={() => setTree(!treeOn)}
      >
        <Icon name="listTree" size={13} />
      </button>
      <div class="segmented range-layout" role="group" aria-label="Diff layout">
        <button
          class:active={layout === "unified"}
          title="Unified diff"
          aria-label="Unified diff"
          onclick={() => setLayout("unified")}
        >
          <Icon name="diffUnified" size={12} />
        </button>
        <button
          class:active={layout === "split"}
          title="Side-by-side diff"
          aria-label="Side-by-side diff"
          onclick={() => setLayout("split")}
        >
          <Icon name="diffSplit" size={12} />
        </button>
      </div>
      <button
        class="icon-btn range-close"
        title="Back to a single change (esc)"
        aria-label="Back to a single change"
        onclick={oncollapse}
      >
        <Icon name="close" size={13} />
      </button>
    </header>
  {:else}
    <ChangeHeader
      {snapshot}
      {node}
      files={current?.files ?? null}
      {layout}
      onlayout={setLayout}
      tree={treeOn}
      ontree={setTree}
      {compare}
      {compareFrom}
      oncompare={setCompare}
      {onjump}
      onjumpfile={jumpToFile}
      {onclose}
    />
  {/if}
  {#if resolveError}
    <p class="resolve-error" role="alert">{resolveError}</p>
  {/if}
  {#if error}
    <div class="load-error" role="alert" data-diff-error>
      <Icon name="conflicts" size={14} />
      <span>{error}</span>
      <button onclick={() => retry++}>Retry</button>
    </div>
  {/if}
  <div
    class="surface"
    class:stale={annotatePath === null &&
      !evolutionOpen &&
      displayed !== null &&
      (slow || !fresh)}
  >
  {#if annotatePath !== null}
    <AnnotatePane
      changeId={node.id}
      path={annotatePath}
      {opId}
      onclose={closeAnnotate}
    />
  {:else if evolutionOpen}
    <EvolutionPane {node} {opId} {layout} onclose={closeEvolution} />
  {:else}
  {#if findOpen}
    <!-- The find bar floats over the scroller's top-right, clear of the
         sticky file headers. Enter/⇧Enter cycle, esc closes. -->
    <div class="find-bar popover" role="search">
      <Icon name="search" size={12} />
      <input
        bind:this={findInputEl}
        bind:value={findQuery}
        class="find-input"
        placeholder="Find in diff"
        spellcheck="false"
        aria-label="Find in diff"
        onkeydown={onFindKeydown}
        oninput={onFindInput}
      />
      <span class="find-count" class:none={findQuery.length > 0 && findMatches.length === 0}>
        {#if findQuery.length > 0}
          {findMatches.length === 0
            ? "0/0"
            : `${findCurrent + 1}/${findMatches.length}`}
        {/if}
      </span>
      <button
        type="button"
        class="find-nav"
        aria-label="Previous match"
        title="Previous match (⇧↩)"
        disabled={findMatches.length === 0}
        onclick={() => gotoFind(-1)}
      >
        <Icon name="arrowUp" size={12} />
      </button>
      <button
        type="button"
        class="find-nav"
        aria-label="Next match"
        title="Next match (↩)"
        disabled={findMatches.length === 0}
        onclick={() => gotoFind(1)}
      >
        <Icon name="arrowDown" size={12} />
      </button>
      <button
        type="button"
        class="find-nav"
        aria-label="Close find"
        title="Close (esc)"
        onclick={closeFind}
      >
        <Icon name="close" size={12} />
      </button>
    </div>
  {/if}
  {#if treeVisible && displayed}
    <aside class="tree-pane" style:width="{treeWidth}px">
      <!-- Keyed like the cards: another comparison is another tree, with
           its own collapse state; same-comparison refetches keep it. -->
      {#key `${displayed.id} ${displayed.from ?? ""}`}
        <FileTree
          files={displayed.files}
          activeIndex={activeFile}
          onpick={jumpToFile}
        />
      {/key}
    </aside>
    <div
      class="tree-divider"
      class:dragging={treeDragging}
      role="separator"
      aria-orientation="vertical"
      onpointerdown={startTreeDrag}
      onpointermove={onTreeDrag}
      onpointerup={endTreeDrag}
      onpointercancel={endTreeDrag}
    ></div>
  {/if}
  <div class="scroller" bind:this={scroller} data-diff-change={fresh ? node.id : undefined}>
    {#if displayed}
      {#if displayed.files.length === 0}
        <div class="empty">
          <EmptyState
            icon="commit"
            title="No file changes"
            body={range
              ? "Nothing differs across this selection — its changes cancel out."
              : compareFrom
                ? "Nothing differs between these two changes."
                : node.isEmpty
                  ? "This change is empty — nothing differs from its parent."
                  : "Nothing differs from the parent tree."}
          />
        </div>
      {:else}
        <!-- Keyed on the comparison too: swapping to another diff must
             remount the cards rather than update them in place — reusing a
             card across two diffs re-renders its rows against the other
             diff's shape mid-flush (null cells in split layout). Same-
             comparison refetches keep the same key, so auto-refresh still
             updates in place without flicker. -->
        {#each displayed.files as file, index (`${displayed.id} ${displayed.from ?? ""} ${file.path}`)}
          <FileDiffCard
            {file}
            {index}
            {layout}
            collapsed={collapsedFiles.has(file.path)}
            find={findProps.get(index) ?? null}
            ontoggle={() => toggleFile(file.path)}
            resolve={file.hasConflict && resolveTool && fresh
              ? {
                  tool: resolveTool,
                  waiting:
                    app.resolvingConflict?.changeId === node.id &&
                    app.resolvingConflict?.path === file.path,
                  disabled: app.resolvingConflict !== null,
                }
              : null}
            onresolve={() => resolveFile(file.path)}
          />
        {/each}
        {#if displayed.truncated}
          <p class="trailing-note">
            File list truncated — this change touches more files than Jiji renders
          </p>
        {/if}
      {/if}
    {:else if error}
      <div class="empty">
        <EmptyState icon="diffUnified" title="Couldn’t load this diff" body="Retry to load the file changes." />
      </div>
    {:else}
      <div class="skeleton" aria-hidden="true">
        <i></i><i></i><i></i><i></i><i></i><i></i>
      </div>
    {/if}
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

  /* The range header: the change header's title bar, restated for a run
     of changes — same height and hairline, so swapping between the two
     reads as the same surface changing subject. */
  .range-header {
    position: relative;
    z-index: 3;
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: var(--toolbar-h);
    padding: 0 var(--sp-2) 0 var(--sp-4);
    border-bottom: 1px solid var(--clr-border-2);
    color: var(--clr-text-3);
  }

  .range-header > :global(svg) {
    color: var(--clr-accent-strong);
  }

  .range-count {
    flex-shrink: 0;
    font-size: var(--text-l);
    font-weight: 600;
    color: var(--clr-text-1);
  }

  .range-span {
    flex-shrink: 0;
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .range-span b {
    color: var(--clr-text-1);
    font-weight: 600;
  }

  .range-arrow {
    color: var(--clr-text-3);
    padding: 0 2px;
  }

  .range-note {
    flex: 1;
    min-width: 0;
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .range-layout > button {
    height: 20px;
    padding: 0 6px;
  }

  /* Tree and scroller share the pane; the tree is fixed-width, the diff
     takes the rest. Positioned so the find bar can float over it. */
  .surface {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
    transition: opacity var(--t-fast) var(--ease-out);
  }

  /* The find bar: a floating strip pinned to the scrollport's top-right,
     above the sticky file headers (z 2). */
  .find-bar {
    position: absolute;
    top: var(--sp-2);
    right: var(--sp-4);
    z-index: 5;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: 32px;
    padding: 0 var(--sp-1) 0 10px;
    color: var(--clr-text-3);
  }

  .find-input {
    width: 180px;
    background: none;
    border: none;
    outline: none;
    font: inherit;
    font-size: var(--text-m);
    color: var(--clr-text-1);
  }

  .find-input::placeholder {
    color: var(--clr-text-3);
  }

  .find-count {
    flex-shrink: 0;
    min-width: 4ch;
    text-align: right;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: var(--clr-text-3);
  }

  .find-count.none {
    color: var(--clr-warn);
  }

  .find-nav {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: var(--radius-s);
    color: var(--clr-text-2);
  }

  .find-nav:hover:not(:disabled) {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .find-nav:disabled {
    opacity: 0.4;
  }

  .surface.stale {
    opacity: 0.6;
  }

  .tree-pane {
    flex-shrink: 0;
    min-height: 0;
  }

  .tree-divider {
    position: relative;
    width: 1px;
    flex-shrink: 0;
    background: var(--clr-border-2);
    transition: background var(--t-fast) var(--ease-out);
  }

  /* Wider invisible hit area for the 1px divider. */
  .tree-divider::after {
    content: "";
    position: absolute;
    inset: 0 -4px;
    z-index: 3;
    cursor: col-resize;
  }

  .tree-divider:hover,
  .tree-divider.dragging {
    background: var(--clr-accent);
  }

  /* Windowed content: rows above the viewport come and go as blocks
     mount, and WebKit's scroll anchoring answers each swap by moving the
     scroll position a whole block (measured: ~1150px leaps mid-fling,
     landing the viewport on still-empty placeholders). Anchoring is for
     documents that reflow; this one has exact placeholder heights. */
  .scroller {
    flex: 1;
    min-width: 0;
    min-height: 0;
    overflow-y: auto;
    overflow-anchor: none;
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
  }

  .load-error {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: 6px var(--sp-4);
    border-bottom: 1px solid var(--clr-border-2);
    font-size: var(--text-s);
    color: var(--clr-warn);
    background: var(--clr-warn-dim);
  }

  .load-error span {
    flex: 1;
  }

  .load-error button {
    font-weight: 500;
    text-decoration: underline;
    text-underline-offset: 3px;
  }

  .resolve-error {
    flex-shrink: 0;
    padding: 6px var(--sp-4);
    font-size: var(--text-s);
    color: var(--clr-danger);
    background: color-mix(in srgb, var(--clr-danger) 8%, transparent);
    border-bottom: 1px solid var(--clr-border-2);
  }

  /* Loading: the shape of a diff, still. Only shown when a fetch outlasts
     the slow timer, so it never flashes on a fast switch. */
  .skeleton {
    display: flex;
    flex-direction: column;
    gap: 9px;
    padding: var(--sp-4);
    animation: breathe 1.6s ease-in-out infinite;
  }

  .skeleton i {
    height: 10px;
    border-radius: 3px;
    background: color-mix(in srgb, var(--clr-text-1) 7%, transparent);
  }

  .skeleton i:nth-child(1) {
    width: 36%;
    height: 14px;
    margin-bottom: 6px;
  }

  .skeleton i:nth-child(2) {
    width: 72%;
  }

  .skeleton i:nth-child(3) {
    width: 58%;
  }

  .skeleton i:nth-child(4) {
    width: 80%;
  }

  .skeleton i:nth-child(5) {
    width: 46%;
  }

  .skeleton i:nth-child(6) {
    width: 64%;
  }

  @keyframes breathe {
    50% {
      opacity: 0.55;
    }
  }
</style>
