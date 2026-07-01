<script lang="ts">
  import { tick } from "svelte";
  import Icon from "$lib/components/ui/Icon.svelte";
  import type { FileDiff } from "$lib/bindings/FileDiff";
  import type { FileStatus } from "$lib/bindings/FileStatus";
  import type { GraphNode } from "$lib/bindings/GraphNode";
  import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
  import { errorMessage } from "$lib/api";
  import {
    abandonChange,
    createBookmark,
    deleteBookmark,
    describeChange,
    editChange,
    moveBookmark,
    moveChange,
    newChange,
    rebaseChange,
    renameBookmark,
    squashChange,
  } from "$lib/state/actions";
  import { fromNow } from "$lib/time";
  import {
    bookmarksAt,
    childrenOf,
    combinedDescription,
    descendantsOf,
    divergentSiblings,
    findNode,
    moveDirection,
    rebaseDestinations,
    resolveCompareFrom,
    splitPath,
    stackPosition,
    SYNC_LABEL,
    type CompareMode,
  } from "$lib/components/inspector/inspect";
  import { fileStats, totalStats, type DiffLayout } from "./diff";

  let {
    snapshot,
    node,
    files,
    layout,
    onlayout,
    compare,
    compareFrom,
    oncompare,
    onjump,
    onjumpfile,
    onclose,
  }: {
    snapshot: RepoSnapshot;
    node: GraphNode;
    // Loaded diff files, for the stats chip and jump menu; null while loading.
    files: FileDiff[] | null;
    layout: DiffLayout;
    onlayout: (layout: DiffLayout) => void;
    // What the diff is measured against, and the change id that resolves
    // to for this selection (null = the plain parent diff).
    compare: CompareMode;
    compareFrom: string | null;
    oncompare: (mode: CompareMode) => void;
    // Move the workbench selection to another change.
    onjump: (id: string) => void;
    // Scroll the diff to one file section.
    onjumpfile: (index: number) => void;
    onclose: () => void;
  } = $props();

  const isWorkingCopy = $derived(node.kind === "workingCopy");
  const kindLabel = $derived(
    isWorkingCopy ? "working copy" : node.kind === "immutable" ? "immutable" : "mutable",
  );

  const descriptionLines = $derived(node.description.split("\n"));
  const title = $derived(descriptionLines[0] ?? "");
  const body = $derived(descriptionLines.slice(1).join("\n").trim());
  let bodyOpen = $state(false);
  $effect(() => {
    void node.id;
    bodyOpen = false;
    editing = false;
    saving = false;
    editError = null;
    confirm = null;
    actionError = null;
    bookmarkOpen = false;
    bookmarkError = null;
    manage = null;
    manageError = null;
    rebaseOpen = false;
    rebaseAlone = false;
    rebaseDest = null;
    rebaseFilter = "";
    rebaseError = null;
    compareOpen = false;
    compareFilter = "";
  });

  // The describe editor: the first mutation surface. Immutable changes get
  // no affordance; the backend refuses them anyway.
  const canDescribe = $derived(node.kind !== "immutable");
  let editing = $state(false);
  let draft = $state("");
  let saving = $state(false);
  let editError = $state<string | null>(null);
  let editorEl = $state<HTMLTextAreaElement | undefined>();

  function openEditor() {
    draft = node.description;
    editError = null;
    editing = true;
    tick().then(() => editorEl?.focus());
  }

  async function saveDescription() {
    if (saving) return;
    saving = true;
    editError = null;
    try {
      await describeChange(node.id, draft);
      editing = false;
    } catch (error) {
      editError = errorMessage(error);
    } finally {
      saving = false;
    }
  }

  function onEditorKeydown(event: KeyboardEvent) {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      saveDescription();
    } else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      editing = false;
    }
  }

  // Mutation actions on the selection. Immutable changes only get "New
  // child" — everything else would rewrite them, so the affordances hide,
  // matching the describe button. Squash also needs a single mutable parent.
  const isImmutable = $derived(node.kind === "immutable");
  const parentNode = $derived(
    node.parents.length === 1 ? findNode(snapshot, node.parents[0]) : undefined,
  );
  const canEdit = $derived(node.kind === "mutable");
  const canSquash = $derived(
    !isImmutable && parentNode !== undefined && parentNode.kind !== "immutable",
  );
  const canAbandon = $derived(!isImmutable);

  // Squash and abandon restructure the graph, so they get the spec's
  // explicit plan step: an inline panel stating the consequences (computed
  // from the same snapshot the graph renders) before anything runs.
  type ConfirmAction = "squash" | "abandon";
  let confirm = $state<ConfirmAction | null>(null);
  let acting = $state(false);
  let actionError = $state<string | null>(null);
  let confirmEl = $state<HTMLDivElement | undefined>();

  const descendants = $derived(descendantsOf(snapshot, node.id));
  const isWcOrAbove = $derived(
    node.id === snapshot.workingCopy ||
      descendants.some((d) => d.id === snapshot.workingCopy),
  );
  const parentTitle = $derived(
    parentNode?.description.split("\n")[0] || "no description",
  );
  const squashedDescription = $derived(
    parentNode ? combinedDescription(parentNode.description, node.description) : "",
  );

  function toggleConfirm(action: ConfirmAction) {
    confirm = confirm === action ? null : action;
    actionError = null;
    bookmarkOpen = false;
    manage = null;
    rebaseOpen = false;
    compareOpen = false;
    if (confirm) tick().then(() => confirmEl?.focus());
  }

  // One busy flag covers every action surface so panels cannot race each
  // other; each panel keeps its own inline error.
  async function runPanel(
    action: () => Promise<unknown>,
    setError: (message: string | null) => void,
    onDone: () => void,
  ) {
    if (acting) return;
    acting = true;
    setError(null);
    try {
      await action();
      onDone();
    } catch (error) {
      setError(errorMessage(error));
    } finally {
      acting = false;
    }
  }

  function run(action: () => Promise<unknown>) {
    return runPanel(
      action,
      (message) => (actionError = message),
      () => (confirm = null),
    );
  }

  function runConfirmed() {
    if (confirm === "squash") run(() => squashChange(node.id));
    else if (confirm === "abandon") run(() => abandonChange(node.id));
  }

  function onConfirmKeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      confirm = null;
    } else if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      runConfirmed();
    }
  }

  const marks = $derived(bookmarksAt(snapshot, node.id));
  const children = $derived(childrenOf(snapshot, node.id));
  const position = $derived(stackPosition(snapshot, node.id));
  const stats = $derived(files ? totalStats(files) : null);

  // jj's ?? state: the other visible copies of this divergent change, for
  // the callout's jump chips. Abandoning or rewriting the copies not wanted
  // is how divergence resolves.
  const siblings = $derived(divergentSiblings(snapshot, node));

  // Bookmark management: the Bookmark action opens a panel that creates a
  // new bookmark on the selection or moves an existing one here; clicking a
  // local non-trunk chip opens rename/delete for that bookmark. Both panels
  // are the spec's plan step — consequences (move direction, what happens
  // on the remote at the next push) are stated before anything runs.
  let bookmarkOpen = $state(false);
  let bookmarkName = $state("");
  let bookmarkError = $state<string | null>(null);
  let bookmarkEl = $state<HTMLInputElement | undefined>();
  let manage = $state<string | null>(null);
  let renameDraft = $state("");
  let manageError = $state<string | null>(null);
  let renameEl = $state<HTMLInputElement | undefined>();

  const managedMark = $derived(marks.find((m) => m.name === manage));
  // Local bookmarks elsewhere in the repo that could move onto this change.
  const movableMarks = $derived(
    snapshot.bookmarks.filter((b) => b.isLocal && b.target !== node.id),
  );

  function toggleBookmarkPanel() {
    bookmarkOpen = !bookmarkOpen;
    confirm = null;
    manage = null;
    rebaseOpen = false;
    compareOpen = false;
    bookmarkName = "";
    bookmarkError = null;
    if (bookmarkOpen) tick().then(() => bookmarkEl?.focus());
  }

  function toggleManage(name: string) {
    manage = manage === name ? null : name;
    confirm = null;
    bookmarkOpen = false;
    rebaseOpen = false;
    compareOpen = false;
    renameDraft = name;
    manageError = null;
    if (manage) tick().then(() => renameEl?.focus());
  }

  function submitCreate() {
    const name = bookmarkName.trim();
    if (!name) return;
    runPanel(
      () => createBookmark(name, node.id),
      (message) => (bookmarkError = message),
      () => (bookmarkOpen = false),
    );
  }

  function submitMove(name: string) {
    runPanel(
      () => moveBookmark(name, node.id),
      (message) => (bookmarkError = message),
      () => (bookmarkOpen = false),
    );
  }

  function submitRename() {
    const name = manage;
    const next = renameDraft.trim();
    if (!name || !next || next === name) return;
    runPanel(
      () => renameBookmark(name, next),
      (message) => (manageError = message),
      () => (manage = null),
    );
  }

  function submitDelete() {
    const name = manage;
    if (!name) return;
    runPanel(
      () => deleteBookmark(name),
      (message) => (manageError = message),
      () => (manage = null),
    );
  }

  function onBookmarkKeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      bookmarkOpen = false;
      manage = null;
    }
  }

  // Rebase: the plan step for moving work. Pick a destination from the
  // graph's own changes, choose whether descendants come along, read the
  // consequences, then confirm. Drag-and-drop arrives in M3 — this is the
  // explicit-action form.
  const canRebase = $derived(!isImmutable);
  let rebaseOpen = $state(false);
  // false = jj rebase -s (descendants follow); true = jj rebase -r (the
  // change moves alone and descendants reparent onto its parents).
  let rebaseAlone = $state(false);
  let rebaseDest = $state<string | null>(null);
  let rebaseFilter = $state("");
  let rebaseError = $state<string | null>(null);
  let rebaseFilterEl = $state<HTMLInputElement | undefined>();
  let rebasePanelEl = $state<HTMLDivElement | undefined>();

  const rebaseCandidates = $derived(
    rebaseDestinations(snapshot, node.id, !rebaseAlone),
  );
  const visibleDestinations = $derived.by(() => {
    const query = rebaseFilter.trim().toLowerCase();
    if (!query) return rebaseCandidates;
    return rebaseCandidates.filter(
      (candidate) =>
        candidate.id.toLowerCase().startsWith(query) ||
        candidate.description.toLowerCase().includes(query) ||
        candidate.bookmarks.some((b) => b.toLowerCase().includes(query)),
    );
  });
  const destNode = $derived(
    rebaseDest !== null ? findNode(snapshot, rebaseDest) : undefined,
  );
  const destTitle = $derived(
    destNode?.description.split("\n")[0] || "no description",
  );
  const parentIdsLabel = $derived(
    node.parents.map((p) => p.slice(0, 4)).join(", "),
  );

  function toggleRebase() {
    rebaseOpen = !rebaseOpen;
    confirm = null;
    bookmarkOpen = false;
    manage = null;
    compareOpen = false;
    rebaseAlone = false;
    rebaseDest = null;
    rebaseFilter = "";
    rebaseError = null;
    if (rebaseOpen) {
      tick().then(() => (rebaseFilterEl ?? rebasePanelEl)?.focus());
    }
  }

  function setRebaseAlone(alone: boolean) {
    rebaseAlone = alone;
    // The exclusion set depends on the scope; a destination no longer
    // offered cannot stay selected.
    if (
      rebaseDest !== null &&
      !rebaseDestinations(snapshot, node.id, !alone).some(
        (candidate) => candidate.id === rebaseDest,
      )
    ) {
      rebaseDest = null;
    }
  }

  function submitRebase() {
    const dest = rebaseDest;
    if (!dest) return;
    const action = rebaseAlone ? moveChange : rebaseChange;
    runPanel(
      () => action(node.id, dest),
      (message) => (rebaseError = message),
      () => (rebaseOpen = false),
    );
  }

  function onRebaseKeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      rebaseOpen = false;
    } else if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      submitRebase();
    }
  }

  // Compare: what the diff is measured against. Read-only — picking a row
  // applies immediately, no plan/confirm step. Presets stay relative
  // (trunk, stack base) so walking the stack keeps the comparison; the
  // any-change list is the commit-to-commit form.
  let compareOpen = $state(false);
  let compareFilter = $state("");
  let compareFilterEl = $state<HTMLInputElement | undefined>();
  let comparePanelEl = $state<HTMLDivElement | undefined>();

  const trunkFromId = $derived(
    resolveCompareFrom(snapshot, node.id, { kind: "trunk" }),
  );
  const baseFromId = $derived(
    resolveCompareFrom(snapshot, node.id, { kind: "base" }),
  );
  const fromNode = $derived(
    compareFrom !== null ? findNode(snapshot, compareFrom) : undefined,
  );
  // The chip states what the surface actually shows: an unresolvable mode
  // (no trunk, the trunk node itself selected) falls back to the parent
  // diff and the chip says so.
  const compareLabel = $derived.by(() => {
    if (compareFrom === null) return "vs parent";
    if (compare.kind === "trunk") return `vs ${snapshot.trunkBookmark || "trunk"}`;
    if (compare.kind === "base") return "vs stack base";
    return `vs ${compareFrom.slice(0, 4)}`;
  });
  // Orientation note for comparisons that do not read top-down.
  const compareNote = $derived.by(() => {
    if (compareFrom === null) return null;
    const direction = moveDirection(snapshot, compareFrom, node.id);
    if (direction === "backwards")
      return `Comparing against a descendant — the diff reads in reverse: what going back to ${node.id.slice(0, 4)} would undo.`;
    if (direction === "sideways")
      return "Comparing across branches — everything that differs between the two trees.";
    return null;
  });

  const compareCandidates = $derived(
    snapshot.nodes.filter((n) => n.id !== node.id),
  );
  const visibleCompareCandidates = $derived.by(() => {
    const query = compareFilter.trim().toLowerCase();
    if (!query) return compareCandidates;
    return compareCandidates.filter(
      (candidate) =>
        candidate.id.toLowerCase().startsWith(query) ||
        candidate.description.toLowerCase().includes(query) ||
        candidate.bookmarks.some((b) => b.toLowerCase().includes(query)),
    );
  });

  function toggleCompare() {
    compareOpen = !compareOpen;
    confirm = null;
    bookmarkOpen = false;
    manage = null;
    rebaseOpen = false;
    compareFilter = "";
    if (compareOpen) {
      tick().then(() => (compareFilterEl ?? comparePanelEl)?.focus());
    }
  }

  function pickCompare(mode: CompareMode) {
    oncompare(mode);
    compareOpen = false;
  }

  function onCompareKeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      compareOpen = false;
    }
  }

  const KIND_GLYPH = { workingCopy: "@", mutable: "○", immutable: "◆" } as const;

  function relationTitle(id: string, role: string): string {
    const description = findNode(snapshot, id)?.description.split("\n")[0];
    return `Go to ${role} ${id}${description ? ` — ${description}` : ""}`;
  }

  let filesOpen = $state(false);
  let menu: HTMLDivElement | undefined;

  function onWindowPointerDown(event: PointerEvent) {
    if (filesOpen && menu && !menu.contains(event.target as Node)) {
      filesOpen = false;
    }
  }

  const STATUS_GLYPH: Record<FileStatus, string> = {
    added: "A",
    modified: "M",
    removed: "D",
    renamed: "R",
    copied: "C",
  };
</script>

<svelte:window onpointerdown={onWindowPointerDown} />

<!-- The selection's details, docked compactly so the diff keeps the pane. -->
<header class="change-header">
  <div class="row top">
    {#if node.isDivergent}
      <span
        class="ids mono selectable divergent"
        title="Divergent change: several visible commits share this change id"
        >{node.changeId}<b class="qq">??</b></span
      >
      <span class="ids mono selectable commit" title="This copy's commit id">
        {node.commitId}
      </span>
    {:else}
      <span class="ids mono selectable">
        <b>{node.id.slice(0, 2)}</b>{node.id.slice(2)}
      </span>
    {/if}
    <span class="kind {node.kind}">{kindLabel}</span>
    {#if title}
      <h3 class="title truncate selectable" {title}>{title}</h3>
      {#if body}
        <button
          class="disclose"
          class:open={bodyOpen}
          title={bodyOpen ? "Hide full description" : "Show full description"}
          onclick={() => (bodyOpen = !bodyOpen)}
        >
          <Icon name="chevronRight" size={12} />
        </button>
      {/if}
      {#if canDescribe && !editing}
        <button class="edit" title="Edit description" onclick={openEditor}>
          <Icon name="edit" size={11} />
        </button>
      {/if}
    {:else}
      <span class="undescribed">
        No description yet
        {#if canDescribe}
          <button class="describe-button" onclick={openEditor}>
            <Icon name="edit" size={10} />
            Describe
          </button>
        {:else}
          <span class="hint-cmd mono">jj describe</span>
        {/if}
      </span>
    {/if}
    <button class="close" title="Clear selection (esc)" onclick={onclose}>
      <Icon name="close" size={13} />
    </button>
  </div>

  {#if editing}
    <div class="describe-editor">
      <textarea
        bind:this={editorEl}
        bind:value={draft}
        rows="4"
        placeholder={"Summary line\n\nOptional details"}
        onkeydown={onEditorKeydown}
        disabled={saving}
      ></textarea>
      <div class="editor-row">
        <span class="editor-hint">First line becomes the summary · ⌘↵ to save</span>
        {#if editError}
          <span class="editor-error truncate" title={editError}>{editError}</span>
        {/if}
        <button class="editor-cancel" onclick={() => (editing = false)} disabled={saving}>
          Cancel
        </button>
        <button class="editor-save" onclick={saveDescription} disabled={saving}>
          {saving ? "Saving…" : "Save"}
        </button>
      </div>
    </div>
  {/if}

  {#if bodyOpen && body && !editing}
    <p class="body selectable">{body}</p>
  {/if}

  <div class="row meta">
    <span class="author truncate">{node.author}</span>
    <span class="dot">·</span>
    <span class="age" title={node.timestamp}>{fromNow(node.timestamp)}</span>
    {#if node.isEmpty}
      <span class="dot">·</span>
      <span class="empty-note">empty</span>
    {/if}
    {#if node.hasConflict}
      <span class="conflict-chip">
        <Icon name="conflicts" size={11} />
        conflicts
      </span>
    {/if}
    {#each marks as mark (mark.name)}
      {@const sync = SYNC_LABEL[mark.sync]}
      {#if mark.isLocal && !mark.isTrunk}
        <button
          class="bookmark-chip manageable"
          class:armed={manage === mark.name}
          data-bookmark={mark.name}
          title="{mark.name}{mark.remote ? `@${mark.remote}` : ""} — {sync.text} · rename or delete"
          onclick={() => toggleManage(mark.name)}
        >
          <Icon name="bookmark" size={10} />
          <span class="truncate">{mark.name}</span>
          <span class="sync {sync.tone}">{sync.text}</span>
        </button>
      {:else}
        <span class="bookmark-chip" class:trunk={mark.isTrunk} title="{mark.name}{mark.remote ? `@${mark.remote}` : ""} — {sync.text}">
          <Icon name="bookmark" size={10} />
          <span class="truncate">{mark.name}</span>
          <span class="sync {sync.tone}">{sync.text}</span>
        </span>
      {/if}
    {/each}
    {#if position}
      <span class="stack-note truncate">
        {position.index + 1} of {position.workstream.nodeIds.length} in “{position.workstream.title}”
      </span>
    {:else if node.kind === "immutable"}
      <span class="stack-note">immutable base</span>
    {/if}
    {#each children as child (child.id)}
      <button class="rel" title={relationTitle(child.id, "child")} onclick={() => onjump(child.id)}>
        <Icon name="arrowUp" size={11} />
        <span class="mono">{child.id.slice(0, 4)}</span>
      </button>
    {/each}
    {#each node.parents as parent (parent)}
      <button class="rel" title={relationTitle(parent, "parent")} onclick={() => onjump(parent)}>
        <Icon name="arrowDown" size={11} />
        <span class="mono">{parent.slice(0, 4)}</span>
      </button>
    {/each}

    <div class="compare-group" class:active={compareFrom !== null}>
      <button
        class="compare-chip"
        class:armed={compareOpen}
        data-action="compare"
        title={compareFrom !== null
          ? `Comparing against ${compareFrom} “${fromNode?.description.split("\n")[0] || "no description"}” — click to change`
          : "Compare against trunk, the stack base, or any change"}
        onclick={toggleCompare}
      >
        <Icon name="compare" size={11} />
        {compareLabel}
      </button>
      {#if compareFrom !== null}
        <button
          class="compare-reset"
          title="Back to the parent diff"
          onclick={() => oncompare({ kind: "parent" })}
        >
          <Icon name="close" size={10} />
        </button>
      {/if}
    </div>

    <div class="files-menu" bind:this={menu}>
      {#if files && stats}
        <button
          class="files-button"
          class:open={filesOpen}
          disabled={files.length === 0}
          onclick={() => (filesOpen = !filesOpen)}
        >
          {files.length}
          {files.length === 1 ? "file" : "files"}
          {#if stats.added > 0}<span class="add mono">+{stats.added}</span>{/if}
          {#if stats.removed > 0}<span class="del mono">−{stats.removed}</span>{/if}
        </button>
      {/if}
      {#if filesOpen && files}
        <div class="menu" role="menu">
          {#each files as file, index (file.path)}
            {@const parts = splitPath(file.path)}
            {@const fStats = fileStats(file)}
            <button
              class="menu-row"
              role="menuitem"
              title={file.renamedFrom
                ? `${file.renamedFrom} → ${file.path}`
                : file.path}
              onclick={() => {
                filesOpen = false;
                onjumpfile(index);
              }}
            >
              <span class="status {file.status} mono">{STATUS_GLYPH[file.status]}</span>
              <span class="path mono">
                {#if parts.dir}<span class="dir">{parts.dir}</span>{/if}<span
                  class="fname">{parts.name}</span>
              </span>
              <span class="row-stats mono">
                {#if fStats.added > 0}<span class="add">+{fStats.added}</span>{/if}
                {#if fStats.removed > 0}<span class="del">−{fStats.removed}</span>{/if}
              </span>
            </button>
          {/each}
        </div>
      {/if}
    </div>

    <div class="layout-toggle" role="group" aria-label="Diff layout">
      <button
        class:active={layout === "unified"}
        title="Unified diff"
        onclick={() => onlayout("unified")}
      >
        <Icon name="diffUnified" size={11} />
      </button>
      <button
        class:active={layout === "split"}
        title="Side-by-side diff"
        onclick={() => onlayout("split")}
      >
        <Icon name="diffSplit" size={11} />
      </button>
    </div>
  </div>

  {#if node.isDivergent}
    <div class="divergence-note" role="note">
      <span class="note-text">
        Divergent change — {siblings.length + 1} visible commits share
        <span class="mono">{node.changeId}</span>, so each copy goes by its
        commit id. Keep one and abandon the
        other{siblings.length === 1 ? "" : "s"} to resolve.
      </span>
      {#each siblings as sibling (sibling.id)}
        <button
          class="sibling-chip mono"
          title={relationTitle(sibling.id, "copy")}
          onclick={() => onjump(sibling.id)}
        >
          {sibling.commitId.slice(0, 8)}
        </button>
      {/each}
    </div>
  {/if}

  {#if compareNote}
    <p class="compare-note">{compareNote}</p>
  {/if}

  {#if !editing}
    <div class="row actions">
      <button
        class="action"
        data-action="new"
        disabled={acting}
        title="Start a new change on top of this one (jj new)"
        onclick={() => run(() => newChange(node.id))}
      >
        <Icon name="plus" size={11} />
        New child
      </button>
      {#if canEdit}
        <button
          class="action"
          data-action="edit"
          disabled={acting}
          title="Make this the working copy (jj edit)"
          onclick={() => run(() => editChange(node.id))}
        >
          <Icon name="atSign" size={11} />
          Edit
        </button>
      {/if}
      <button
        class="action"
        class:armed={bookmarkOpen}
        data-action="bookmark"
        disabled={acting}
        title="Create or move a bookmark onto this change (jj bookmark)"
        onclick={toggleBookmarkPanel}
      >
        <Icon name="bookmark" size={11} />
        Bookmark
      </button>
      {#if canRebase}
        <button
          class="action"
          class:armed={rebaseOpen}
          data-action="rebase"
          disabled={acting}
          title="Move this change onto a different parent (jj rebase)"
          onclick={toggleRebase}
        >
          <Icon name="rebase" size={11} />
          Rebase
        </button>
      {/if}
      {#if canSquash}
        <button
          class="action"
          class:armed={confirm === "squash"}
          data-action="squash"
          disabled={acting}
          title="Move this change's content into its parent (jj squash)"
          onclick={() => toggleConfirm("squash")}
        >
          <Icon name="squash" size={11} />
          Squash into parent
        </button>
      {/if}
      {#if canAbandon}
        <button
          class="action danger"
          class:armed={confirm === "abandon"}
          data-action="abandon"
          disabled={acting}
          title="Abandon this change (jj abandon)"
          onclick={() => toggleConfirm("abandon")}
        >
          <Icon name="trash" size={11} />
          Abandon
        </button>
      {/if}
      {#if actionError && confirm === null}
        <span class="action-error truncate" title={actionError}>{actionError}</span>
      {/if}
    </div>

    {#if compareOpen}
      <div
        class="confirm-panel compare-panel"
        role="dialog"
        aria-label="Compare this change against another point"
        tabindex="-1"
        bind:this={comparePanelEl}
        onkeydown={onCompareKeydown}
      >
        <p class="confirm-title">
          Compare <b class="mono">{node.id.slice(0, 4)}</b>
          <span class="confirm-context truncate">
            pick what the diff is measured against — read-only, nothing is rewritten
          </span>
        </p>
        <div class="dest-list preset-list" role="listbox" aria-label="Comparison preset">
          <button
            class="dest-row"
            class:selected={compareFrom === null}
            role="option"
            aria-selected={compareFrom === null}
            data-compare="parent"
            onclick={() => pickCompare({ kind: "parent" })}
          >
            <span class="dest-glyph mono">·</span>
            <span class="preset-name">Parent</span>
            <span class="dest-title truncate quiet">what this change itself did</span>
          </button>
          {#if trunkFromId}
            <button
              class="dest-row"
              class:selected={compare.kind !== "change" && compareFrom === trunkFromId}
              role="option"
              aria-selected={compare.kind !== "change" && compareFrom === trunkFromId}
              data-compare="trunk"
              onclick={() => pickCompare({ kind: "trunk" })}
            >
              <span class="dest-glyph mono">◆</span>
              <span class="preset-name">Trunk</span>
              <span class="dest-title truncate quiet">
                everything between <span class="mono">{trunkFromId.slice(0, 4)}</span> and here
              </span>
              <span class="dest-bookmark trunk">{snapshot.trunkBookmark}</span>
            </button>
          {/if}
          {#if baseFromId && baseFromId !== trunkFromId}
            {@const baseNode = findNode(snapshot, baseFromId)}
            <button
              class="dest-row"
              class:selected={compare.kind === "base" && compareFrom === baseFromId}
              role="option"
              aria-selected={compare.kind === "base" && compareFrom === baseFromId}
              data-compare="base"
              onclick={() => pickCompare({ kind: "base" })}
            >
              <span class="dest-glyph mono {baseNode?.kind ?? ''}">
                {baseNode ? KIND_GLYPH[baseNode.kind] : "○"}
              </span>
              <span class="preset-name">Stack base</span>
              <span class="dest-title truncate quiet">
                everything this stack changes up to here, from
                <span class="mono">{baseFromId.slice(0, 4)}</span>
              </span>
            </button>
          {/if}
        </div>
        <span class="result-label dest-label">Or against any change</span>
        {#if compareCandidates.length > 6}
          <input
            bind:this={compareFilterEl}
            bind:value={compareFilter}
            class="name-input mono dest-filter compare-filter"
            placeholder="filter by id, title, or bookmark"
            spellcheck="false"
          />
        {/if}
        <div class="dest-list" role="listbox" aria-label="Comparison base change">
          {#each visibleCompareCandidates as candidate (candidate.id)}
            <button
              class="dest-row"
              class:selected={compare.kind === "change" && compareFrom === candidate.id}
              role="option"
              aria-selected={compare.kind === "change" && compareFrom === candidate.id}
              data-compare-from={candidate.id}
              onclick={() => pickCompare({ kind: "change", id: candidate.id })}
            >
              <span class="dest-glyph mono {candidate.kind}">{KIND_GLYPH[candidate.kind]}</span>
              <span class="dest-id mono"><b>{candidate.id.slice(0, 2)}</b>{candidate.id.slice(2, 4)}</span>
              <span class="dest-title truncate">
                {candidate.description.split("\n")[0] || "no description"}
              </span>
              {#each candidate.bookmarks as name (name)}
                <span
                  class="dest-bookmark"
                  class:trunk={name === snapshot.trunkBookmark}
                >{name}</span>
              {/each}
            </button>
          {:else}
            <span class="dest-empty">No matching change</span>
          {/each}
        </div>
        <div class="confirm-row">
          <span class="editor-hint">
            The comparison follows the selection until you switch back
          </span>
          <button class="editor-cancel" onclick={() => (compareOpen = false)}>
            Close
          </button>
        </div>
      </div>
    {/if}

    {#if confirm === "squash" && parentNode}
      <div
        class="confirm-panel"
        role="alertdialog"
        aria-label="Confirm squash"
        tabindex="-1"
        bind:this={confirmEl}
        onkeydown={onConfirmKeydown}
      >
        <p class="confirm-title">
          Squash <b class="mono">{node.id.slice(0, 4)}</b> into its parent
          <b class="mono">{parentNode.id.slice(0, 4)}</b>
          <span class="confirm-context truncate">“{parentTitle}”</span>
        </p>
        <ul class="consequences">
          <li>
            Everything this change touches moves into the parent; the change
            itself is abandoned.
          </li>
          {#if marks.length > 0}
            <li>
              Bookmark{marks.length === 1 ? "" : "s"}
              {marks.map((m) => m.name).join(", ")}
              move{marks.length === 1 ? "s" : ""} to the parent.
            </li>
          {/if}
          {#if descendants.length > 0}
            <li>
              {descendants.length} descendant change{descendants.length === 1
                ? " rebases"
                : "s rebase"} onto the parent.
            </li>
          {/if}
          {#if node.id === snapshot.workingCopy}
            <li>The working copy restarts as a new empty change on the parent.</li>
          {:else if isWcOrAbove}
            <li>The working copy follows the rebase.</li>
          {/if}
        </ul>
        <div class="result-desc">
          <span class="result-label">Resulting description</span>
          {#if squashedDescription}
            <pre class="selectable">{squashedDescription}</pre>
          {:else}
            <pre class="empty-desc">(no description — describe it afterwards)</pre>
          {/if}
        </div>
        <div class="confirm-row">
          <span class="editor-hint">⌘↵ to confirm</span>
          {#if actionError}
            <span class="editor-error truncate" title={actionError}>{actionError}</span>
          {/if}
          <button class="editor-cancel" onclick={() => (confirm = null)} disabled={acting}>
            Cancel
          </button>
          <button class="confirm-go" onclick={runConfirmed} disabled={acting}>
            {acting ? "Squashing…" : "Squash"}
          </button>
        </div>
      </div>
    {:else if confirm === "abandon"}
      <div
        class="confirm-panel danger"
        role="alertdialog"
        aria-label="Confirm abandon"
        tabindex="-1"
        bind:this={confirmEl}
        onkeydown={onConfirmKeydown}
      >
        <p class="confirm-title">
          Abandon <b class="mono">{node.id.slice(0, 4)}</b>
          <span class="confirm-context truncate"
            >“{title || "no description"}”</span
          >
        </p>
        <ul class="consequences">
          <li>
            Its changes disappear from the graph — the operation log can still
            restore them.
          </li>
          {#if marks.length > 0}
            <li>
              Bookmark{marks.length === 1 ? "" : "s"}
              {marks.map((m) => m.name).join(", ")} will be deleted.
            </li>
          {/if}
          {#if descendants.length > 0}
            <li>
              {descendants.length} descendant change{descendants.length === 1
                ? " rebases"
                : "s rebase"} onto its parent.
            </li>
          {/if}
          {#if node.id === snapshot.workingCopy}
            <li>The working copy restarts as a new empty change on its parent.</li>
          {:else if isWcOrAbove}
            <li>The working copy follows the rebase.</li>
          {/if}
          {#if siblings.length === 1}
            <li>
              Only commit <span class="mono">{siblings[0].commitId.slice(0, 8)}</span>
              remains for <span class="mono">{node.changeId}</span> — the
              divergence resolves.
            </li>
          {:else if siblings.length > 1}
            <li>
              {siblings.length} other copies of
              <span class="mono">{node.changeId}</span> stay divergent.
            </li>
          {/if}
        </ul>
        <div class="confirm-row">
          <span class="editor-hint">⌘↵ to confirm</span>
          {#if actionError}
            <span class="editor-error truncate" title={actionError}>{actionError}</span>
          {/if}
          <button class="editor-cancel" onclick={() => (confirm = null)} disabled={acting}>
            Cancel
          </button>
          <button class="confirm-go danger" onclick={runConfirmed} disabled={acting}>
            {acting ? "Abandoning…" : "Abandon change"}
          </button>
        </div>
      </div>
    {/if}

    {#if rebaseOpen}
      <div
        class="confirm-panel rebase-panel"
        role="dialog"
        aria-label="Rebase this change"
        tabindex="-1"
        bind:this={rebasePanelEl}
        onkeydown={onRebaseKeydown}
      >
        <p class="confirm-title">
          Rebase <b class="mono">{node.id.slice(0, 4)}</b>
          <span class="confirm-context truncate">“{title || "no description"}”</span>
        </p>
        <div class="rebase-controls">
          {#if descendants.length > 0}
            <div class="mode-toggle" role="group" aria-label="Rebase scope">
              <button
                class:active={!rebaseAlone}
                data-mode="stack"
                disabled={acting}
                onclick={() => setRebaseAlone(false)}
              >
                With {descendants.length} descendant{descendants.length === 1 ? "" : "s"}
              </button>
              <button
                class:active={rebaseAlone}
                data-mode="single"
                disabled={acting}
                onclick={() => setRebaseAlone(true)}
              >
                Only this change
              </button>
            </div>
          {/if}
          {#if rebaseCandidates.length > 6}
            <input
              bind:this={rebaseFilterEl}
              bind:value={rebaseFilter}
              class="name-input mono dest-filter"
              placeholder="filter by id, title, or bookmark"
              spellcheck="false"
              disabled={acting}
            />
          {/if}
        </div>
        <span class="result-label dest-label">Destination — the new parent</span>
        <div class="dest-list" role="listbox" aria-label="Rebase destination">
          {#each visibleDestinations as candidate (candidate.id)}
            <button
              class="dest-row"
              class:selected={rebaseDest === candidate.id}
              role="option"
              aria-selected={rebaseDest === candidate.id}
              data-dest={candidate.id}
              disabled={acting}
              onclick={() => (rebaseDest = candidate.id)}
            >
              <span class="dest-glyph mono {candidate.kind}">{KIND_GLYPH[candidate.kind]}</span>
              <span class="dest-id mono"><b>{candidate.id.slice(0, 2)}</b>{candidate.id.slice(2, 4)}</span>
              <span class="dest-title truncate">
                {candidate.description.split("\n")[0] || "no description"}
              </span>
              {#each candidate.bookmarks as name (name)}
                <span
                  class="dest-bookmark"
                  class:trunk={name === snapshot.trunkBookmark}
                >{name}</span>
              {/each}
            </button>
          {:else}
            <span class="dest-empty">No matching destination</span>
          {/each}
        </div>
        {#if destNode}
          <ul class="consequences">
            {#if rebaseAlone}
              <li>
                Only this change moves onto
                <b class="mono">{destNode.id.slice(0, 4)}</b>
                <span class="quiet">“{destTitle}”</span>.
              </li>
              {#if descendants.length > 0}
                <li>
                  {descendants.length} descendant change{descendants.length === 1
                    ? " stays"
                    : "s stay"} behind, reparented onto
                  <span class="mono">{parentIdsLabel}</span>.
                </li>
              {/if}
            {:else}
              <li>
                This change{descendants.length > 0
                  ? ` and its ${descendants.length} descendant${descendants.length === 1 ? "" : "s"}`
                  : ""} move{descendants.length > 0 ? "" : "s"} onto
                <b class="mono">{destNode.id.slice(0, 4)}</b>
                <span class="quiet">“{destTitle}”</span>.
              </li>
            {/if}
            {#if node.parents.length > 1}
              <li>
                Its {node.parents.length} parents are replaced by the
                destination — the merge is dissolved.
              </li>
            {/if}
            {#if node.id === snapshot.workingCopy}
              <li>The working copy moves with it.</li>
            {:else if isWcOrAbove && !rebaseAlone}
              <li>The working copy follows the rebase.</li>
            {:else if isWcOrAbove && rebaseAlone}
              <li>The working copy stays behind with the reparented descendants.</li>
            {/if}
            <li>
              Changes that no longer apply cleanly become conflicts instead of
              stopping the rebase; the operation log can undo it.
            </li>
          </ul>
        {/if}
        <div class="confirm-row">
          <span class="editor-hint">
            {destNode ? "⌘↵ to confirm" : "Pick the destination this change moves onto"}
          </span>
          {#if rebaseError}
            <span class="editor-error truncate" title={rebaseError}>{rebaseError}</span>
          {/if}
          <button class="editor-cancel" onclick={() => (rebaseOpen = false)} disabled={acting}>
            Cancel
          </button>
          <button
            class="confirm-go"
            onclick={submitRebase}
            disabled={acting || !destNode}
          >
            {acting ? "Rebasing…" : rebaseAlone ? "Move here" : "Rebase here"}
          </button>
        </div>
      </div>
    {/if}

    {#if bookmarkOpen}
      <div
        class="confirm-panel bookmark-panel"
        role="dialog"
        aria-label="Bookmark this change"
        tabindex="-1"
        onkeydown={onBookmarkKeydown}
      >
        <p class="confirm-title">
          Bookmark <b class="mono">{node.id.slice(0, 4)}</b>
          <span class="confirm-context truncate">“{title || "no description"}”</span>
        </p>
        <div class="name-row">
          <input
            bind:this={bookmarkEl}
            bind:value={bookmarkName}
            class="name-input mono"
            placeholder="new-bookmark-name"
            spellcheck="false"
            disabled={acting}
            onkeydown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                submitCreate();
              }
            }}
          />
          <button
            class="confirm-go"
            onclick={submitCreate}
            disabled={acting || !bookmarkName.trim()}
          >
            {acting ? "Working…" : "Create"}
          </button>
        </div>
        {#if movableMarks.length > 0}
          <span class="result-label move-label">Or move an existing bookmark here</span>
          <div class="move-list">
            {#each movableMarks as mark (mark.name)}
              {@const direction = moveDirection(snapshot, mark.target, node.id)}
              {@const notes = [
                ...(direction && direction !== "forward" ? [`moves ${direction}`] : []),
                ...(mark.remote ? [`${mark.remote} updates on push`] : []),
              ]}
              <button
                class="move-row"
                data-bookmark={mark.name}
                disabled={acting}
                onclick={() => submitMove(mark.name)}
              >
                <Icon name="bookmark" size={10} />
                <span class="move-name truncate">{mark.name}</span>
                {#if mark.isTrunk}<span class="move-trunk">trunk</span>{/if}
                <span class="move-note truncate">
                  from <span class="mono">{mark.target.slice(0, 4)}</span>{["", ...notes].join(" · ")}
                </span>
              </button>
            {/each}
          </div>
        {/if}
        <div class="confirm-row">
          <span class="editor-hint">
            Bookmarks never rewrite changes; the operation log can restore them
          </span>
          {#if bookmarkError}
            <span class="editor-error truncate" title={bookmarkError}>{bookmarkError}</span>
          {/if}
          <button class="editor-cancel" onclick={() => (bookmarkOpen = false)} disabled={acting}>
            Cancel
          </button>
        </div>
      </div>
    {:else if managedMark}
      <div
        class="confirm-panel bookmark-panel"
        role="dialog"
        aria-label="Manage bookmark {managedMark.name}"
        tabindex="-1"
        onkeydown={onBookmarkKeydown}
      >
        <p class="confirm-title">
          <b class="mono">{managedMark.name}</b>
          <span class="confirm-context truncate">
            {managedMark.remote
              ? `tracked on ${managedMark.remote} — ${SYNC_LABEL[managedMark.sync].text}`
              : "local-only bookmark"}
          </span>
        </p>
        <div class="name-row">
          <input
            bind:this={renameEl}
            bind:value={renameDraft}
            class="name-input mono"
            spellcheck="false"
            disabled={acting}
            onkeydown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                submitRename();
              }
            }}
          />
          <button
            class="confirm-go"
            onclick={submitRename}
            disabled={acting || !renameDraft.trim() || renameDraft.trim() === managedMark.name}
          >
            {acting ? "Working…" : "Rename"}
          </button>
        </div>
        <div class="confirm-row">
          <span class="editor-hint">
            {managedMark.remote
              ? `Renaming starts a local-only bookmark; deleting removes it from ${managedMark.remote} on the next push`
              : "Only exists locally; the operation log can restore it"}
          </span>
          {#if manageError}
            <span class="editor-error truncate" title={manageError}>{manageError}</span>
          {/if}
          <button class="action danger bm-delete" onclick={submitDelete} disabled={acting}>
            <Icon name="trash" size={11} />
            Delete
          </button>
          <button class="editor-cancel" onclick={() => (manage = null)} disabled={acting}>
            Close
          </button>
        </div>
      </div>
    {/if}
  {/if}
</header>

<style>
  .change-header {
    position: relative;
    z-index: 3;
    flex-shrink: 0;
    padding: var(--sp-2) var(--sp-4);
    border-bottom: 1px solid var(--clr-border-2);
    background: var(--clr-bg-1);
  }

  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }

  .row.top {
    gap: var(--sp-3);
  }

  .ids {
    flex-shrink: 0;
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .ids b {
    color: var(--clr-accent-strong);
    font-weight: 600;
  }

  /* jj's ?? state: the change id no longer names one commit, so it reads
     alarmed and the commit id carries the identity. */
  .ids.divergent,
  .ids.divergent .qq {
    color: var(--clr-danger);
  }

  .ids.divergent .qq {
    font-weight: 600;
  }

  .kind {
    flex-shrink: 0;
    font-size: var(--text-xs);
    border-radius: 999px;
    padding: 1px 8px;
    border: 1px solid var(--clr-border-2);
    color: var(--clr-text-3);
  }

  .kind.workingCopy {
    background: var(--clr-working-copy-dim);
    color: var(--clr-working-copy);
    border-color: transparent;
  }

  .kind.mutable {
    background: var(--clr-accent-dim);
    color: var(--clr-accent-strong);
    border-color: transparent;
  }

  .title {
    flex: 1;
    min-width: 0;
    font-size: var(--text-m);
    font-weight: 600;
    letter-spacing: -0.1px;
    color: var(--clr-text-1);
  }

  .disclose {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    border-radius: var(--radius-s);
    color: var(--clr-text-3);
    transition: all var(--t-fast) var(--ease-out);
  }

  .disclose.open {
    transform: rotate(90deg);
  }

  .disclose:hover {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  /* Spec: undescribed changes must be visually obvious. */
  .undescribed {
    flex: 1;
    min-width: 0;
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-s);
    font-style: italic;
    color: var(--clr-warn);
  }

  .hint-cmd {
    font-style: normal;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
    border: 1px solid var(--clr-border-2);
    border-radius: 999px;
    padding: 0 7px;
  }

  /* The explicit describe affordance the spec asks for on the callout. */
  .describe-button {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
    font-style: normal;
    font-size: var(--text-xs);
    font-weight: 500;
    color: var(--clr-warn);
    border: 1px solid color-mix(in srgb, var(--clr-warn) 35%, transparent);
    border-radius: 999px;
    padding: 1px 9px;
    transition: all var(--t-fast) var(--ease-out);
  }

  .describe-button:hover {
    background: color-mix(in srgb, var(--clr-warn) 14%, transparent);
  }

  .edit {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    border-radius: var(--radius-s);
    color: var(--clr-text-3);
    transition: all var(--t-fast) var(--ease-out);
  }

  .edit:hover {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .describe-editor {
    margin: var(--sp-2) 0 var(--sp-1);
  }

  .describe-editor textarea {
    width: 100%;
    min-height: 72px;
    max-height: 220px;
    resize: vertical;
    padding: var(--sp-2) var(--sp-3);
    font: inherit;
    font-size: var(--text-s);
    line-height: 1.5;
    color: var(--clr-text-1);
    background: var(--clr-bg-2);
    border: 1px solid var(--clr-border-1);
    border-radius: var(--radius-m);
    transition: border-color var(--t-fast) var(--ease-out);
  }

  .describe-editor textarea:focus {
    outline: none;
    border-color: var(--clr-accent-strong);
  }

  .describe-editor textarea:disabled {
    opacity: 0.6;
  }

  .editor-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin-top: var(--sp-1);
  }

  .editor-hint {
    flex: 1;
    min-width: 0;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .editor-error {
    min-width: 0;
    max-width: 28em;
    font-size: var(--text-xs);
    color: var(--clr-danger);
  }

  .editor-cancel,
  .editor-save {
    flex-shrink: 0;
    font-size: var(--text-xs);
    font-weight: 500;
    border-radius: 999px;
    padding: 2px 11px;
    transition: all var(--t-fast) var(--ease-out);
  }

  .editor-cancel {
    color: var(--clr-text-3);
    border: 1px solid var(--clr-border-2);
  }

  .editor-cancel:hover:not(:disabled) {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .editor-save {
    color: var(--clr-accent-strong);
    background: var(--clr-accent-dim);
    border: 1px solid transparent;
  }

  .editor-save:hover:not(:disabled) {
    background: color-mix(in srgb, var(--clr-accent-strong) 24%, transparent);
  }

  .editor-cancel:disabled,
  .editor-save:disabled {
    cursor: default;
    opacity: 0.6;
  }

  .close {
    flex-shrink: 0;
    margin-left: auto;
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: var(--radius-s);
    color: var(--clr-text-3);
    transition: all var(--t-fast) var(--ease-out);
  }

  .close:hover {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .body {
    margin: var(--sp-1) 0 var(--sp-2);
    padding-left: var(--sp-1);
    font-size: var(--text-s);
    line-height: 1.5;
    color: var(--clr-text-2);
    white-space: pre-wrap;
    overflow-wrap: break-word;
    max-height: 180px;
    overflow-y: auto;
  }

  .row.meta {
    margin-top: 3px;
    font-size: var(--text-s);
    color: var(--clr-text-3);
    flex-wrap: wrap;
    row-gap: 2px;
  }

  .author {
    max-width: 12em;
  }

  .age,
  .dot {
    flex-shrink: 0;
  }

  .empty-note {
    font-style: italic;
  }

  .conflict-chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
    font-size: var(--text-xs);
    border-radius: 999px;
    padding: 1px 7px;
    background: color-mix(in srgb, var(--clr-danger) 14%, transparent);
    color: var(--clr-danger);
  }

  .bookmark-chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
    max-width: 16em;
    font-size: var(--text-xs);
    border-radius: 999px;
    padding: 1px 8px;
    background: var(--clr-bg-3);
    color: var(--clr-text-2);
    border: 1px solid var(--clr-border-2);
  }

  .bookmark-chip.trunk {
    background: transparent;
    border-color: var(--clr-border-1);
    color: var(--clr-text-1);
  }

  .bookmark-chip.manageable {
    transition: all var(--t-fast) var(--ease-out);
  }

  .bookmark-chip.manageable:hover,
  .bookmark-chip.manageable.armed {
    background: var(--clr-bg-hover);
    border-color: var(--clr-border-1);
    color: var(--clr-text-1);
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

  .stack-note {
    min-width: 0;
    max-width: 22em;
  }

  .rel {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    flex-shrink: 0;
    padding: 1px 6px;
    border-radius: 999px;
    border: 1px solid var(--clr-border-2);
    color: var(--clr-text-3);
    font-size: var(--text-xs);
    transition: all var(--t-fast) var(--ease-out);
  }

  .rel:hover {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .rel .mono {
    color: var(--clr-accent-strong);
  }

  /* Actions stay close to the change without crowding it: quiet pills in
     their own row, the destructive one toned apart. */
  .row.actions {
    margin-top: var(--sp-2);
    gap: var(--sp-2);
    flex-wrap: wrap;
  }

  .action {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    flex-shrink: 0;
    font-size: var(--text-xs);
    font-weight: 500;
    border-radius: 999px;
    padding: 2px 10px;
    border: 1px solid var(--clr-border-2);
    color: var(--clr-text-2);
    transition: all var(--t-fast) var(--ease-out);
  }

  .action:hover:not(:disabled),
  .action.armed {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
    border-color: var(--clr-border-1);
  }

  .action.danger {
    color: var(--clr-danger);
    border-color: color-mix(in srgb, var(--clr-danger) 30%, transparent);
  }

  .action.danger:hover:not(:disabled),
  .action.danger.armed {
    background: color-mix(in srgb, var(--clr-danger) 12%, transparent);
    color: var(--clr-danger);
    border-color: color-mix(in srgb, var(--clr-danger) 45%, transparent);
  }

  .action:disabled {
    cursor: default;
    opacity: 0.6;
  }

  .action-error {
    min-width: 0;
    max-width: 28em;
    font-size: var(--text-xs);
    color: var(--clr-danger);
  }

  /* The plan step for structural mutations: consequences first, then the
     explicit confirm. Docked inline, like the describe editor. */
  .confirm-panel {
    margin: var(--sp-2) 0 var(--sp-1);
    padding: var(--sp-3);
    background: var(--clr-bg-2);
    border: 1px solid var(--clr-border-1);
    border-radius: var(--radius-m);
    outline: none;
  }

  .confirm-panel.danger {
    border-color: color-mix(in srgb, var(--clr-danger) 35%, transparent);
  }

  .confirm-title {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    min-width: 0;
    font-size: var(--text-s);
    font-weight: 600;
    color: var(--clr-text-1);
  }

  .confirm-title .mono {
    color: var(--clr-accent-strong);
  }

  .confirm-context {
    min-width: 0;
    font-weight: 400;
    color: var(--clr-text-3);
  }

  .consequences {
    margin: var(--sp-2) 0 0;
    padding-left: 1.3em;
    display: grid;
    gap: 3px;
    font-size: var(--text-s);
    line-height: 1.45;
    color: var(--clr-text-2);
  }

  .result-desc {
    margin-top: var(--sp-2);
  }

  .result-label {
    display: block;
    font-size: var(--text-xs);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--clr-text-3);
    margin-bottom: 3px;
  }

  .result-desc pre {
    margin: 0;
    padding: var(--sp-2) var(--sp-3);
    font: inherit;
    font-size: var(--text-s);
    line-height: 1.5;
    white-space: pre-wrap;
    overflow-wrap: break-word;
    max-height: 120px;
    overflow-y: auto;
    color: var(--clr-text-2);
    background: var(--clr-bg-1);
    border: 1px solid var(--clr-border-2);
    border-radius: var(--radius-s);
  }

  .result-desc .empty-desc {
    font-style: italic;
    color: var(--clr-text-3);
  }

  .confirm-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin-top: var(--sp-2);
  }

  .confirm-go {
    flex-shrink: 0;
    font-size: var(--text-xs);
    font-weight: 500;
    border-radius: 999px;
    padding: 2px 11px;
    color: var(--clr-accent-strong);
    background: var(--clr-accent-dim);
    border: 1px solid transparent;
    transition: all var(--t-fast) var(--ease-out);
  }

  .confirm-go:hover:not(:disabled) {
    background: color-mix(in srgb, var(--clr-accent-strong) 24%, transparent);
  }

  .confirm-go.danger {
    color: var(--clr-danger);
    background: color-mix(in srgb, var(--clr-danger) 14%, transparent);
  }

  .confirm-go.danger:hover:not(:disabled) {
    background: color-mix(in srgb, var(--clr-danger) 24%, transparent);
  }

  .confirm-go:disabled {
    cursor: default;
    opacity: 0.6;
  }

  /* Bookmark panels: a name input beside its action, then the optional
     move-an-existing-bookmark list. */
  .name-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin-top: var(--sp-2);
  }

  .name-input {
    flex: 1;
    min-width: 0;
    max-width: 24em;
    padding: 3px var(--sp-3);
    font-size: var(--text-s);
    color: var(--clr-text-1);
    background: var(--clr-bg-1);
    border: 1px solid var(--clr-border-1);
    border-radius: 999px;
    transition: border-color var(--t-fast) var(--ease-out);
  }

  .name-input:focus {
    outline: none;
    border-color: var(--clr-accent-strong);
  }

  .name-input:disabled {
    opacity: 0.6;
  }

  .move-label {
    margin-top: var(--sp-3);
    margin-bottom: 0;
  }

  .move-list {
    display: flex;
    flex-direction: column;
    margin-top: 2px;
  }

  .move-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    min-width: 0;
    text-align: left;
    padding: 3px var(--sp-2);
    border-radius: var(--radius-s);
    color: var(--clr-text-2);
    font-size: var(--text-s);
    transition: all var(--t-fast) var(--ease-out);
  }

  .move-row:hover:not(:disabled) {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .move-row:disabled {
    cursor: default;
    opacity: 0.6;
  }

  .move-name {
    font-weight: 500;
    color: var(--clr-text-1);
  }

  .move-trunk {
    flex-shrink: 0;
    font-size: var(--text-xs);
    border: 1px solid var(--clr-border-1);
    border-radius: 999px;
    padding: 0 7px;
    color: var(--clr-text-1);
  }

  .move-note {
    min-width: 0;
    margin-left: auto;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  /* The rebase panel: scope toggle and filter above a selectable
     destination list, consequences appearing once a destination is picked. */
  .rebase-controls {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin-top: var(--sp-2);
    flex-wrap: wrap;
  }

  .mode-toggle {
    flex-shrink: 0;
    display: inline-flex;
    border: 1px solid var(--clr-border-2);
    border-radius: 999px;
    overflow: hidden;
  }

  .mode-toggle button {
    font-size: var(--text-xs);
    font-weight: 500;
    padding: 2px 10px;
    color: var(--clr-text-3);
    transition: all var(--t-fast) var(--ease-out);
  }

  .mode-toggle button + button {
    border-left: 1px solid var(--clr-border-2);
  }

  .mode-toggle button:hover:not(:disabled) {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .mode-toggle button.active {
    background: var(--clr-bg-3);
    color: var(--clr-text-1);
  }

  .mode-toggle button:disabled {
    cursor: default;
    opacity: 0.6;
  }

  .dest-filter {
    flex: 1;
    max-width: 20em;
  }

  .dest-label {
    margin-top: var(--sp-3);
    margin-bottom: 0;
  }

  .dest-list {
    display: flex;
    flex-direction: column;
    margin-top: 2px;
    max-height: 180px;
    overflow-y: auto;
  }

  .dest-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    min-width: 0;
    text-align: left;
    padding: 3px var(--sp-2);
    border-radius: var(--radius-s);
    color: var(--clr-text-2);
    font-size: var(--text-s);
    transition: all var(--t-fast) var(--ease-out);
  }

  .dest-row:hover:not(:disabled) {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .dest-row.selected {
    background: var(--clr-accent-dim);
    color: var(--clr-text-1);
  }

  .dest-row:disabled {
    cursor: default;
    opacity: 0.6;
  }

  .dest-glyph {
    flex-shrink: 0;
    width: 12px;
    text-align: center;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .dest-glyph.workingCopy {
    color: var(--clr-working-copy);
  }

  .dest-glyph.mutable {
    color: var(--clr-accent);
  }

  .dest-id {
    flex-shrink: 0;
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .dest-id b {
    color: var(--clr-accent-strong);
    font-weight: 600;
  }

  .dest-title {
    min-width: 0;
    flex: 1;
  }

  .dest-bookmark {
    flex-shrink: 0;
    max-width: 12em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-xs);
    border-radius: 999px;
    padding: 0 7px;
    background: var(--clr-bg-3);
    border: 1px solid var(--clr-border-2);
    color: var(--clr-text-2);
  }

  .dest-bookmark.trunk {
    background: transparent;
    border-color: var(--clr-border-1);
    color: var(--clr-text-1);
  }

  .dest-empty {
    padding: 3px var(--sp-2);
    font-size: var(--text-s);
    font-style: italic;
    color: var(--clr-text-3);
  }

  .quiet {
    color: var(--clr-text-3);
  }

  /* The diff-surface controls cluster right: comparison, files, layout.
     The compare group carries the auto margin that used to live on the
     files menu, since it always renders. */
  .compare-group {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    margin-left: auto;
    flex-shrink: 0;
  }

  .compare-chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 1px 8px;
    border-radius: 999px;
    border: 1px solid var(--clr-border-2);
    color: var(--clr-text-3);
    font-size: var(--text-xs);
    font-weight: 500;
    transition: all var(--t-fast) var(--ease-out);
  }

  .compare-chip:hover,
  .compare-chip.armed {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  /* An active comparison reframes the whole surface; the chip is the one
     loud cue that this is not the plain parent diff. */
  .compare-group.active .compare-chip {
    background: var(--clr-accent-dim);
    border-color: color-mix(in srgb, var(--clr-accent-strong) 35%, transparent);
    color: var(--clr-accent-strong);
  }

  .compare-reset {
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    border-radius: 999px;
    color: var(--clr-text-3);
    transition: all var(--t-fast) var(--ease-out);
  }

  .compare-reset:hover {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .compare-note {
    margin-top: 2px;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
    font-style: italic;
  }

  /* The divergence callout: what happened, and jump chips to the other
     visible copies of the change. */
  .divergence-note {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--sp-2);
    margin-top: var(--sp-2);
    padding: var(--sp-2) var(--sp-3);
    font-size: var(--text-s);
    border-radius: var(--radius-m);
    background: color-mix(in srgb, var(--clr-danger) 8%, transparent);
    border: 1px solid color-mix(in srgb, var(--clr-danger) 22%, transparent);
    color: var(--clr-text-2);
  }

  .divergence-note .note-text .mono {
    color: var(--clr-danger);
  }

  .sibling-chip {
    flex-shrink: 0;
    height: 18px;
    padding: 0 8px;
    font-size: var(--text-xs);
    border-radius: 999px;
    border: 1px solid color-mix(in srgb, var(--clr-danger) 30%, transparent);
    color: var(--clr-danger);
    transition: background var(--t-fast) var(--ease-out);
  }

  .sibling-chip:hover {
    background: color-mix(in srgb, var(--clr-danger) 12%, transparent);
  }

  .preset-list {
    margin-top: var(--sp-2);
    max-height: none;
    overflow-y: visible;
  }

  .preset-name {
    flex-shrink: 0;
    font-weight: 500;
    color: var(--clr-text-1);
  }

  .compare-filter {
    margin-top: var(--sp-1);
  }

  .files-menu {
    position: relative;
    flex-shrink: 0;
  }

  .files-button {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    padding: 1px 8px;
    border-radius: 999px;
    border: 1px solid var(--clr-border-2);
    color: var(--clr-text-2);
    font-size: var(--text-xs);
    transition: all var(--t-fast) var(--ease-out);
  }

  .files-button:hover:not(:disabled),
  .files-button.open {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .files-button:disabled {
    cursor: default;
    opacity: 0.6;
  }

  .add {
    color: var(--clr-ok);
  }

  .del {
    color: var(--clr-danger);
  }

  .layout-toggle {
    flex-shrink: 0;
    display: inline-flex;
    border: 1px solid var(--clr-border-2);
    border-radius: 999px;
    overflow: hidden;
  }

  .layout-toggle button {
    display: grid;
    place-items: center;
    padding: 2px 7px;
    color: var(--clr-text-3);
    transition: all var(--t-fast) var(--ease-out);
  }

  .layout-toggle button + button {
    border-left: 1px solid var(--clr-border-2);
  }

  .layout-toggle button:hover {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .layout-toggle button.active {
    background: var(--clr-bg-3);
    color: var(--clr-text-1);
  }

  .menu {
    position: absolute;
    top: calc(100% + 4px);
    right: 0;
    z-index: 5;
    min-width: 260px;
    max-width: 380px;
    max-height: 320px;
    overflow-y: auto;
    padding: var(--sp-1);
    background: var(--clr-bg-2);
    border: 1px solid var(--clr-border-1);
    border-radius: var(--radius-m);
    box-shadow: var(--shadow-2);
  }

  .menu-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    min-width: 0;
    text-align: left;
    padding: 3px var(--sp-2);
    border-radius: var(--radius-s);
    transition: background var(--t-fast) var(--ease-out);
  }

  .menu-row:hover {
    background: var(--clr-bg-hover);
  }

  .status {
    flex-shrink: 0;
    width: 14px;
    text-align: center;
    font-size: var(--text-xs);
    font-weight: 600;
  }

  .status.added {
    color: var(--clr-ok);
  }

  .status.modified {
    color: var(--clr-warn);
  }

  .status.removed {
    color: var(--clr-danger);
  }

  .status.renamed,
  .status.copied {
    color: var(--clr-accent);
  }

  .path {
    display: flex;
    min-width: 0;
    flex: 1;
    font-size: var(--text-s);
    white-space: nowrap;
  }

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
  }

  .row-stats {
    flex-shrink: 0;
    display: flex;
    gap: 6px;
    font-size: var(--text-xs);
  }
</style>
