<script lang="ts">
  // The app's right-click menu, replacing the webview's own. One window
  // listener owns every right-click: it resolves the DOM under the pointer
  // into a MenuTarget (a text field, a graph row, a diff line, a bookmark
  // chip, a PR badge, an operation row, a conflict path, the breadcrumb,
  // or the bare surface) and builds the matching commands via the pure
  // model in menu.ts. Right-clicking a graph row selects it first, so
  // intent commands land on the clicked change like they do from the
  // actions row.

  import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
  import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
  import Icon from "$lib/components/ui/Icon.svelte";
  import * as api from "$lib/api";
  import type { GraphNode } from "$lib/bindings/GraphNode";
  import type { PrSummary } from "$lib/bindings/PrSummary";
  import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
  import { canResolve } from "$lib/components/conflicts/conflicts";
  import { currentDiff } from "$lib/components/diff/current.svelte";
  import { hunkAt, unifiedPatch } from "$lib/components/diff/diff";
  import {
    findNode,
    resolveCompareFrom,
  } from "$lib/components/inspector/inspect";
  import { prForBookmark } from "$lib/components/publish/pr";
  import { bookmarkTaken, reviewBookmark } from "$lib/components/publish/review";
  import { shippableStacks } from "$lib/components/publish/ship";
  import { publishableStacks } from "$lib/components/publish/submit";
  import { app } from "$lib/state/app.svelte";
  import {
    absorbChange,
    chooseRepo,
    deleteBookmark,
    duplicateChange,
    editChange,
    fetchPr,
    goToSection,
    jumpToChange,
    newChange,
    openAnnotate,
    pushBookmark,
    refreshSnapshot,
    resolveConflict,
    restoreFileChanges,
    restoreOperation,
    revertChange,
    revertOperation,
    runQuiet,
    sendIntent,
    togglePalette,
    trackBookmark,
    undoLastMutation,
    untrackBookmark,
  } from "$lib/state/actions";
  import { editorChoices, openInEditor } from "$lib/state/editor.svelte";
  import { openInTerminal } from "$lib/state/terminal.svelte";
  import { forge, refreshForgePrs } from "$lib/state/forge.svelte";
  import {
    buildMenu,
    type ChangeTarget,
    type DiffTarget,
    type MenuItem,
    type MenuSection,
    type MenuTarget,
  } from "./menu";

  let open = $state(false);
  let x = $state(0);
  let y = $state(0);
  let sections = $state<MenuSection[]>([]);
  let active = $state(-1);
  let panel = $state<HTMLDivElement | undefined>();
  // The open flyout submenu: which top-level row spawned it and where.
  let sub = $state<{ row: number; x: number; y: number; active: number } | null>(
    null,
  );
  let subPanel = $state<HTMLDivElement | undefined>();
  // The text field that was right-clicked; clipboard ops act on it. Its
  // selection range survives the menu (menu items refuse focus), so cut
  // and paste work on exactly what the user had selected.
  let field: HTMLInputElement | HTMLTextAreaElement | null = null;
  // The diff region that was right-clicked, kept for lazy patch copies.
  let diffTarget: DiffTarget | null = null;

  interface Row {
    item: MenuItem;
    section: number;
  }

  function flatten(menuSections: MenuSection[]): Row[] {
    return menuSections.flatMap((section, s) =>
      section.map((item) => ({ item, section: s })),
    );
  }

  const rows = $derived(flatten(sections));
  const subRows = $derived(
    sub === null ? [] : flatten(rows[sub.row]?.item.children ?? []),
  );

  // ── Target resolution ──

  const TEXT_INPUTS = new Set(["text", "search", "url", "tel", "password"]);

  function editableFrom(
    target: EventTarget | null,
  ): HTMLInputElement | HTMLTextAreaElement | null {
    if (!(target instanceof Element)) return null;
    const el = target.closest("input, textarea");
    if (el instanceof HTMLTextAreaElement) return el.disabled ? null : el;
    if (el instanceof HTMLInputElement && TEXT_INPUTS.has(el.type) && !el.disabled) {
      return el;
    }
    return null;
  }

  function findPr(num: string | null | undefined): PrSummary | null {
    if (!num || !forge.prs) return null;
    const fromReport = forge.prs.report.prs.find(
      (pr) => String(pr.number) === num,
    );
    if (fromReport) return fromReport;
    for (const pr of Object.values(forge.prs.byBranch)) {
      if (pr && String(pr.number) === num) return pr;
    }
    return null;
  }

  function changeTargetFor(snapshot: RepoSnapshot, node: GraphNode): ChangeTarget {
    // Land needs a verified forge connection, ship only a detected GitHub
    // remote — the same gates the palette applies to its Publish rows.
    const forgeReady =
      forge.status?.repo != null && forge.status?.auth.login != null;
    const landable = forgeReady
      ? publishableStacks(
          snapshot,
          new Set(Object.keys(forge.prs?.byBranch ?? {})),
        ).find(
          (s) => s.headBookmark != null && node.bookmarks.includes(s.headBookmark),
        )
      : undefined;
    const shippable =
      forge.status?.repo != null &&
      shippableStacks(snapshot).some((s) => s.headChange === node.id);
    return {
      kind: "change",
      node,
      compare: {
        trunk: resolveCompareFrom(snapshot, node.id, { kind: "trunk" }) != null,
        base: resolveCompareFrom(snapshot, node.id, { kind: "base" }) != null,
        trunkName: snapshot.trunkBookmark,
      },
      landBookmark: landable?.headBookmark ?? null,
      shippable,
    };
  }

  function numAttr(el: Element, name: string): number | null {
    const raw = el.getAttribute(name);
    if (raw === null) return null;
    const parsed = Number(raw);
    return Number.isFinite(parsed) ? parsed : null;
  }

  function resolveTarget(eventTarget: EventTarget | null): MenuTarget {
    const snapshot = app.snapshot;
    const el = eventTarget instanceof Element ? eventTarget : null;
    if (!el || !snapshot) return { kind: "surface" };

    // PR badges anywhere: graph chips, the header chip, Publish rows.
    const prEl = el.closest("[data-pr], [data-review-pr]");
    if (prEl) {
      const pr = findPr(
        prEl.getAttribute("data-pr") ?? prEl.getAttribute("data-review-pr"),
      );
      if (pr) {
        return {
          kind: "pr",
          pr,
          remote: forge.status?.repo?.remote ?? null,
          reviewBookmarkTaken: bookmarkTaken(reviewBookmark(pr.number), snapshot),
        };
      }
    }

    // Bookmark chips (graph rows, change header) and Bookmarks-view rows.
    const bmEl = el.closest("[data-bookmark]");
    if (bmEl) {
      const name = bmEl.getAttribute("data-bookmark") ?? "";
      const remote = bmEl.getAttribute("data-bookmark-remote") || null;
      const bookmark = snapshot.bookmarks.find(
        (b) => b.name === name && (b.remote ?? null) === remote,
      );
      if (bookmark) {
        return {
          kind: "bookmark",
          bookmark,
          pr: prForBookmark(bookmark, forge.prs),
          hasRemotes: snapshot.gitRemotes.length > 0,
        };
      }
    }

    const nodeId = el.closest("[data-node-id]")?.getAttribute("data-node-id");
    if (nodeId) {
      const node = findNode(snapshot, nodeId);
      if (node) return changeTargetFor(snapshot, node);
    }

    const opId = el.closest("[data-op-id]")?.getAttribute("data-op-id");
    if (opId) {
      const op = snapshot.operations.find((o) => o.id === opId);
      if (op) return { kind: "operation", opId, isCurrent: op.isCurrent };
    }

    // A conflicted path in the Conflicts inbox.
    const conflictEl = el.closest("[data-conflict-path]");
    if (conflictEl) {
      const path = conflictEl.getAttribute("data-conflict-path") ?? "";
      const conflictNode =
        conflictEl.closest("[data-conflict-node]")?.getAttribute("data-conflict-node") ||
        null;
      const node = conflictNode ? findNode(snapshot, conflictNode) : undefined;
      return {
        kind: "conflict",
        changeId: conflictNode,
        path,
        isWorkingCopy: node?.kind === "workingCopy",
        resolveTool: canResolve(snapshot, conflictNode)
          ? snapshot.resolveTool
          : null,
      };
    }

    // Inside the diff pane: the file card names the path, the scroller
    // names the change, and the row (when the click hit one) the line.
    const fileEl = el.closest("[data-file-index][data-path]");
    const diffEl = el.closest("[data-diff-change]");
    if (fileEl && diffEl) {
      const path = fileEl.getAttribute("data-path") ?? "";
      const changeId = diffEl.getAttribute("data-diff-change") ?? "";
      const file = currentDiff.diff?.files.find((f) => f.path === path);
      const node = findNode(snapshot, changeId);
      if (file && node) {
        const lineEl = el.closest("[data-old], [data-new]");
        // A historical surface (the evolution pane) shows old versions'
        // diffs: reading verbs stay, but mutations and resolve would act
        // on the change's current content with stale coordinates — off.
        const readonly = el.closest("[data-diff-readonly]") !== null;
        return {
          kind: "diff",
          file,
          changeId,
          mutable: !readonly && node.kind !== "immutable",
          line: lineEl
            ? { old: numAttr(lineEl, "data-old"), new: numAttr(lineEl, "data-new") }
            : null,
          resolveTool:
            !readonly && file.hasConflict && canResolve(snapshot, changeId)
              ? snapshot.resolveTool
              : null,
        };
      }
    }

    if (el.closest("[data-breadcrumb]")) return { kind: "breadcrumb" };

    return { kind: "surface" };
  }

  function onContextMenu(event: MouseEvent) {
    // Dev escape hatch: ⌥-right-click keeps the native menu (Inspect
    // Element and friends). Release builds always get the custom menu.
    if (import.meta.env.DEV && event.altKey) return;
    event.preventDefault();
    const node = event.target as Node;
    if (panel?.contains(node) || subPanel?.contains(node)) return;

    const el = editableFrom(event.target);
    const target: MenuTarget = el
      ? {
          kind: "editable",
          hasSelection: (el.selectionStart ?? 0) !== (el.selectionEnd ?? 0),
          readOnly: el.readOnly,
        }
      : resolveTarget(event.target);
    // Right-click selects the row, like a left click would — intents act
    // on the selection.
    if (target.kind === "change") jumpToChange(target.node.id);

    const built = buildMenu({
      snapshot: app.snapshot,
      selection: el ? "" : (window.getSelection()?.toString() ?? ""),
      target,
      editors: editorChoices(),
      canUndo: app.lastMutation?.outcome.operationId != null,
    });
    if (built.length === 0) return;

    field = el;
    diffTarget = target.kind === "diff" ? target : null;
    sections = built;
    active = -1;
    sub = null;
    x = event.clientX;
    y = event.clientY;
    open = true;
  }

  function close() {
    open = false;
    sub = null;
    field = null;
    diffTarget = null;
  }

  // Keep the menu on screen: flip to the other side of the pointer when it
  // would overflow (like the native menu), then clamp to the viewport.
  $effect(() => {
    if (!open || !panel) return;
    const rect = panel.getBoundingClientRect();
    const pad = 8;
    if (x + rect.width > window.innerWidth - pad) {
      x = Math.max(pad, x - rect.width);
    }
    if (y + rect.height > window.innerHeight - pad) {
      y = Math.max(pad, y - rect.height);
    }
  });

  // The flyout hugs its parent item; flipped to the panel's left edge when
  // the right side runs out, clamped vertically.
  $effect(() => {
    if (!sub || !subPanel || !panel) return;
    const rect = subPanel.getBoundingClientRect();
    const panelRect = panel.getBoundingClientRect();
    const pad = 8;
    let nx = sub.x;
    let ny = sub.y;
    if (nx + rect.width > window.innerWidth - pad) {
      nx = Math.max(pad, panelRect.left - rect.width - 2);
    }
    if (ny + rect.height > window.innerHeight - pad) {
      ny = Math.max(pad, window.innerHeight - pad - rect.height);
    }
    if (nx !== sub.x || ny !== sub.y) sub = { ...sub, x: nx, y: ny };
  });

  function openSub(index: number) {
    const item = rows[index]?.item;
    if (!item?.children) {
      sub = null;
      return;
    }
    if (sub?.row === index) return;
    const itemEl = panel?.querySelector(`[data-mi="${index}"]`);
    const panelRect = panel?.getBoundingClientRect();
    const rect = itemEl?.getBoundingClientRect();
    if (!rect || !panelRect) return;
    sub = { row: index, x: panelRect.right + 2, y: rect.top - 5, active: -1 };
  }

  function insidePanels(node: Node | null): boolean {
    return (
      (node !== null && panel?.contains(node) === true) ||
      (node !== null && subPanel?.contains(node) === true)
    );
  }

  function onWindowPointerdown(event: PointerEvent) {
    if (open && !insidePanels(event.target as Node)) close();
  }

  function onWindowWheel(event: WheelEvent) {
    if (open && !insidePanels(event.target as Node)) close();
  }

  function move(delta: number) {
    const list = sub === null ? rows : subRows;
    if (list.length === 0) return;
    const current = sub === null ? active : sub.active;
    let i = current === -1 && delta < 0 ? 0 : current;
    for (let step = 0; step < list.length; step++) {
      i = (i + delta + list.length) % list.length;
      if (!list[i].item.disabled) break;
    }
    if (sub === null) active = i;
    else sub = { ...sub, active: i };
  }

  function onWindowKeydown(event: KeyboardEvent) {
    if (!open) return;
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      if (sub) sub = null;
      else close();
    } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      move(event.key === "ArrowDown" ? 1 : -1);
    } else if (event.key === "ArrowRight") {
      const item = rows[active]?.item;
      if (item?.children) {
        event.preventDefault();
        openSub(active);
        if (sub) sub = { ...sub, active: 0 };
      }
    } else if (event.key === "ArrowLeft") {
      if (sub) {
        event.preventDefault();
        sub = null;
      }
    } else if (event.key === "Enter") {
      // Swallowed even with no highlighted row, so a focused field under
      // the menu (the revset form, say) never receives the submit.
      event.preventDefault();
      const row = sub === null ? rows[active] : subRows[sub.active];
      if (row?.item.children) {
        openSub(active);
        if (sub) sub = { ...sub, active: 0 };
      } else if (row) {
        void run(row.item);
      } else {
        close();
      }
    } else if (event.key === "Tab") {
      event.preventDefault();
    }
  }

  // ── Execution ──

  // Clipboard ops go through the Tauri plugin: WKWebView's own
  // navigator.clipboard read is unreliable, and the write needs no
  // permission prompt this way.
  async function execClipboard(
    op: "cut" | "copy" | "paste" | "selectAll",
    el: HTMLInputElement | HTMLTextAreaElement | null,
  ) {
    if (!el) return;
    el.focus();
    if (op === "selectAll") {
      el.select();
      return;
    }
    const start = el.selectionStart ?? 0;
    const end = el.selectionEnd ?? 0;
    if (op === "copy" || op === "cut") {
      const text = el.value.slice(start, end);
      if (text === "") return;
      await writeText(text);
      if (op === "cut") {
        el.setRangeText("", start, end, "end");
        el.dispatchEvent(new Event("input", { bubbles: true }));
      }
    } else {
      // An empty or non-text clipboard rejects; that is "nothing to paste".
      const text = await readText().catch(() => null);
      if (!text) return;
      el.setRangeText(text, start, end, "end");
      el.dispatchEvent(new Event("input", { bubbles: true }));
    }
  }

  function copyPatch(target: DiffTarget | null, scope: "hunk" | "file") {
    if (!target || target.file.content.kind !== "text") return;
    const hunk =
      scope === "hunk" && target.line
        ? (hunkAt(target.file.content.hunks, target.line) ?? undefined)
        : undefined;
    if (scope === "hunk" && !hunk) return;
    void writeText(unifiedPatch(target.file, hunk)).catch((error) => {
      console.warn("clipboard write failed", error);
    });
  }

  async function run(item: MenuItem) {
    if (item.disabled || !item.action) return;
    const el = field;
    const diff = diffTarget;
    const action = item.action;
    close();
    switch (action.type) {
      case "clipboard":
        await execClipboard(action.op, el).catch((error) => {
          console.warn("clipboard operation failed", error);
        });
        break;
      case "copyText":
        void writeText(action.text).catch((error) => {
          console.warn("clipboard write failed", error);
        });
        break;
      case "copyPatch":
        copyPatch(diff, action.scope);
        break;
      case "newChild":
        void runQuiet(() => newChange(action.id));
        break;
      case "edit":
        void runQuiet(() => editChange(action.id));
        break;
      case "duplicate":
        void runQuiet(() => duplicateChange(action.id));
        break;
      case "revert":
        void runQuiet(() => revertChange(action.id));
        break;
      case "absorb":
        void runQuiet(() => absorbChange(action.id));
        break;
      case "intent":
        sendIntent(action.intent);
        break;
      case "palette":
        togglePalette();
        break;
      case "refresh":
        void refreshSnapshot();
        break;
      case "chooseRepo":
        void chooseRepo();
        break;
      case "undo":
        void undoLastMutation();
        break;
      case "section":
        goToSection(action.section);
        break;
      case "openUrl":
        void runQuiet(() => openUrl(action.url));
        break;
      case "openInEditor":
        void runQuiet(() =>
          openInEditor(action.editor, action.path, action.line ?? undefined),
        );
        break;
      case "revealInFinder":
        void runQuiet(() => revealItemInDir(action.path));
        break;
      case "openInTerminal":
        void runQuiet(() => openInTerminal(action.path));
        break;
      case "resolveConflict":
        void runQuiet(() => resolveConflict(action.changeId, action.path));
        break;
      case "annotateFile":
        openAnnotate(action.changeId, action.path);
        break;
      case "restoreFile":
        void runQuiet(() => restoreFileChanges(action.changeId, action.path));
        break;
      case "restoreHunk":
        void runQuiet(() =>
          restoreFileChanges(action.changeId, action.path, [action.hunk]),
        );
        break;
      case "restoreOperation":
        void runQuiet(() => restoreOperation(action.opId));
        break;
      case "revertOperation":
        void runQuiet(() => revertOperation(action.opId));
        break;
      case "pushBookmark":
        void runQuiet(() => pushBookmark(action.name));
        break;
      case "trackBookmark":
        void runQuiet(() => trackBookmark(action.name, action.remote));
        break;
      case "untrackBookmark":
        void runQuiet(() => untrackBookmark(action.name, action.remote));
        break;
      case "deleteBookmark":
        void runQuiet(() => deleteBookmark(action.name));
        break;
      case "rerunCi":
        void runQuiet(async () => {
          await api.rerunFailedCi(action.number);
          await refreshForgePrs();
        });
        break;
      case "fetchPr":
        void runQuiet(() => fetchPr(action.remote, action.number, action.bookmark));
        break;
    }
  }
</script>

{#snippet menuRow(row: Row, index: number, level: "top" | "sub")}
  {@const isActive = level === "top" ? index === active : index === sub?.active}
  <button
    class="item"
    class:active={isActive || (level === "top" && sub?.row === index)}
    class:danger={row.item.danger}
    role="menuitem"
    aria-haspopup={row.item.children ? "menu" : undefined}
    data-mi={level === "top" ? index : undefined}
    disabled={row.item.disabled}
    onpointerenter={() => {
      if (level === "top") {
        active = index;
        openSub(index);
      } else if (sub) {
        sub = { ...sub, active: index };
      }
    }}
    onpointerleave={() => {
      if (level === "top" && sub?.row !== index) active = -1;
    }}
    onpointerdown={(event) => event.preventDefault()}
    onclick={() => void run(row.item)}
  >
    <span class="item-icon">
      {#if row.item.icon}<Icon name={row.item.icon} size={13} />{/if}
    </span>
    <span class="label truncate">{row.item.label}</span>
    {#if row.item.children}
      <span class="sub-chev"><Icon name="chevronRight" size={12} /></span>
    {:else if row.item.shortcut}
      <kbd class="key">{row.item.shortcut}</kbd>
    {/if}
  </button>
{/snippet}

<svelte:window
  oncontextmenu={onContextMenu}
  onpointerdown={onWindowPointerdown}
  onwheel={onWindowWheel}
  onkeydown={onWindowKeydown}
  onblur={close}
  onresize={close}
/>

{#if open}
  <div
    class="menu"
    bind:this={panel}
    role="menu"
    aria-label="Context menu"
    style:left="{x}px"
    style:top="{y}px"
  >
    {#each rows as row, index (row.item.id)}
      {#if index > 0 && rows[index - 1].section !== row.section}
        <div class="sep" role="separator"></div>
      {/if}
      {@render menuRow(row, index, "top")}
    {/each}
  </div>
  {#if sub && subRows.length > 0}
    <div
      class="menu sub"
      bind:this={subPanel}
      role="menu"
      aria-label="Submenu"
      style:left="{sub.x}px"
      style:top="{sub.y}px"
    >
      {#each subRows as row, index (row.item.id)}
        {#if index > 0 && subRows[index - 1].section !== row.section}
          <div class="sep" role="separator"></div>
        {/if}
        {@render menuRow(row, index, "sub")}
      {/each}
    </div>
  {/if}
{/if}

<style>
  /* A native-feeling menu: opaque, one hairline, instant. */
  .menu {
    position: fixed;
    z-index: 100;
    min-width: 220px;
    max-width: 320px;
    max-height: min(60vh, 480px);
    overflow-y: auto;
    padding: var(--sp-1);
    background: var(--clr-bg-2);
    border: 1px solid var(--clr-border-1);
    box-shadow: var(--shadow-popover);
    border-radius: var(--radius-l);
  }

  .menu.sub {
    z-index: 101;
    min-width: 190px;
  }

  .item {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    height: 26px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-s);
    text-align: left;
    font-size: var(--text-m);
    color: var(--clr-text-1);
  }

  .item.active {
    background: var(--clr-accent);
    color: var(--clr-accent-contrast);
  }

  .item:disabled {
    color: var(--clr-text-3);
  }

  .item-icon {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    width: 16px;
    color: var(--clr-text-3);
  }

  .item.active .item-icon,
  .item.active .key,
  .item.active .sub-chev {
    color: inherit;
  }

  .item.danger {
    color: var(--clr-danger);
  }

  .item.danger .item-icon {
    color: inherit;
  }

  .item.danger.active {
    background: var(--clr-danger);
    color: #ffffff;
  }

  .label {
    flex: 1;
    min-width: 0;
  }

  .key {
    flex-shrink: 0;
    margin-left: var(--sp-3);
    font-family: var(--font-ui);
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .sub-chev {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    color: var(--clr-text-3);
  }

  .sep {
    height: 1px;
    margin: var(--sp-1) var(--sp-2);
    background: var(--clr-border-2);
  }
</style>
