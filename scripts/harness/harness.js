// Browser side of the visual-verification harness (see scripts/visual-harness).
//
// Injected into the built SPA's <head> ahead of the app bundle, it stubs the
// Tauri IPC surface with captured snapshot/diff JSON (from snapshots.js,
// generated at setup time) and drives one scenario per page load from query
// params:
//
//   ?snap=mock|real      which captured snapshot backs the app (default mock)
//   &theme=<mode|name>   preset the theme (localStorage, pre-boot): a mode
//                        (system|light|dark) or a palette name (midnight,
//                        graphite, abyss, moss, ember, paper, linen,
//                        glacier, meadow, dawn)
//   &pane=<px>           preset the graph pane width (localStorage, pre-boot)
//   &layout=split        preset the diff layout (localStorage, pre-boot)
//   &view=graph|focus    preset the workbench view mode (localStorage, pre-boot)
//   &section=<name>      click a sidebar section (e.g. operations)
//   &cgo=<conflictId>    click a conflict-inbox item (jump to its change)
//   &ctarget=<changeId>  click a conflicted bookmark's candidate chip
//   &resolve=<path>      click a Resolve button by conflicted path (inbox
//                        card with section=conflicts, or diff file header
//                        after click=<id>) and wait for the breadcrumb
//   &resolvewait=1       the stubbed merge tool never returns: the click
//                        settles into the "Waiting for …" state instead
//   &upws=1              click the stale-workspace card's Update workspace
//                        button (section=conflicts; the captured snapshot
//                        must carry a stale current workspace) and wait for
//                        the item to settle
//   &click=<changeId>    click a graph row
//   &collapse=<n>        click the nth (0-based) file header in the diff
//                        to collapse that file
//   &sibling=<n>         click the nth sibling workstream card (focus view)
//   &open=files|body     then open the files menu or description disclosure
//   &open=describe       then open the description editor on the selection
//   &open=squash|abandon open that action's confirm panel on the selection
//   &open=bookmark       open the bookmark panel on the selection
//   &open=rebase         open the rebase panel on the selection
//   &open=split          open the split panel on the selection
//   &sfiles=<p1,p2>      check those file rows in the open split panel
//   &shunks=<p@0.2,p2@1> open those files' hunk lists in the split panel
//                        and check the dot-separated hunk indices (a bare
//                        `p@` only opens the list; composes with sfiles —
//                        e.g. check the file, then untick one hunk)
//   &sdesc=<text>        type the carved change's description in the open
//                        split panel (confirm via &confirm=1; the stubbed
//                        split_change partitions the captured diff between
//                        the two halves, at hunk granularity for hunk
//                        selections)
//   &open=compare        open the compare panel on the selection
//   &compare=<v>         pick a row in the open compare panel: parent|trunk|
//                        base or a change id (compare_diff is stubbed from
//                        captured comparisons, else a per-change-diff union)
//   &mode=stack|single   pick the rebase scope in the open rebase panel
//   &dest=<changeId>     pick that destination row in the open rebase panel
//   &bookmark=<name>     type a name into the open bookmark panel and create
//   &movebm=<name>       move that bookmark onto the selection (panel row)
//   &manage=<name>       open a bookmark chip's rename/delete panel
//   &rename=<text>       type into the open manage panel and rename
//   &delete=1            delete the bookmark in the open manage panel
//   &obmrename=<name>    start inline rename on the overview's bookmark list
//   &obmdelete=<name>    start inline delete on the overview's bookmark list
//   &open=theme          open the theme menu in the top bar
//   &swatch=<label>      switch theme through the menu (e.g. Ember): runs
//                        the live selectTheme path, not the pre-boot preset
//   &palette=1           open the command palette (top-bar ⌘K button)
//   &pq=<text>           type into the open palette's input
//   &prun=<commandId>    click that palette row (data-command, e.g.
//                        change.rebase or goto.<id>) and wait for the
//                        palette to close; panel params (&dest, &confirm,
//                        &describe, …) then drive the opened panel
//   &describe=<text>     type into the open editor and save (stubbed
//                        describe_change mutates the captured snapshot)
//   &action=new|edit     run that action on the selection (stubbed mutation)
//   &confirm=1           confirm the open squash/abandon panel and wait for
//                        the stubbed mutation to land
//   &oprevert=<opId>     open the revert panel on that operation row
//   &oprestore=<opId>    open the restore panel on that operation row
//                        (both confirm via &confirm=1; ops recorded by an
//                        earlier stubbed mutation in the same scenario use
//                        the deterministic ids ad00…, ad01…)
//   &undo=1              click the breadcrumb's Undo and wait for the
//                        revert breadcrumb to land
//   &drag=<changeId>     press that graph row and start dragging it
//   &dragto=<changeId>   drag over that row (plan card + target ring show)
//   &dragalt=1           hold ⌥ through the drag (move-alone scope)
//   &drop=1              release over the dragto row and wait for the
//                        stubbed rebase/move breadcrumb (omit for refused
//                        targets — they record nothing)
//   &expand=1            open the first collapsed snapshot run (operations)
//   &scroll=<px>         scroll the main content scroller vertically
//   &scrollx=<px>        scroll diff code horizontally (every file)
//
// Steps run in order; each waits (100ms poll) for its target element, so the
// scenario settles correctly under Chrome's --virtual-time-budget.
(() => {
  const params = new URLSearchParams(location.search);
  const snapName = params.get("snap") || "mock";
  const data = (window.__HARNESS_DATA__ || {})[snapName] || {};

  // Pre-boot presets: components read these at init.
  const themePref = params.get("theme");
  if (themePref) {
    // Keep the palette lists in sync with $lib/state/theme.svelte.ts.
    const darkThemes = ["midnight", "graphite", "abyss", "moss", "ember"];
    const lightThemes = ["paper", "linen", "glacier", "meadow", "dawn"];
    let pref = null;
    if (["system", "light", "dark"].includes(themePref)) {
      pref = { mode: themePref, dark: darkThemes[0], light: lightThemes[0] };
    } else if (darkThemes.includes(themePref)) {
      pref = { mode: "dark", dark: themePref, light: lightThemes[0] };
    } else if (lightThemes.includes(themePref)) {
      pref = { mode: "light", dark: darkThemes[0], light: themePref };
    }
    if (pref) {
      localStorage.setItem("jiji-theme", JSON.stringify(pref));
      // Themes are a supporter perk; an unregistered copy boots to the
      // default light palette, so presetting a theme implies registration.
      localStorage.setItem("jiji-registered", "1");
    }
  }
  const pane = params.get("pane");
  if (pane) localStorage.setItem("jiji.pane.graph", pane);
  const layout = params.get("layout");
  if (layout) localStorage.setItem("jiji.diff.layout", layout);
  const view = params.get("view");
  if (view) localStorage.setItem("jiji.workbench.view", view);

  // Mutation stubs operate on the captured snapshot in place, mirroring the
  // Rust mock backend so action → refresh → breadcrumb flows render fully.
  let mutationIndex = 0;
  const reject = (code, message) => Promise.reject({ code, message });
  const requireNode = (snap, id) => (snap?.nodes || []).find((n) => n.id === id);
  // Snapshot clones bracketing every stubbed mutation, so revert/restore
  // can put earlier state back; `pristine` is the state before any of them.
  const pristine = data.snapshot ? structuredClone(data.snapshot) : null;
  const opHistory = new Map();
  let pendingBefore = null;
  const pushOp = (snap, description, effects = []) => {
    const id = `ad${String(mutationIndex++).padStart(2, "0")}e5c4be00`;
    snap.operations.forEach((op) => (op.isCurrent = false));
    snap.operations.unshift({
      id,
      description,
      timestamp: "2026-06-10T13:01:00Z",
      isCurrent: true,
      user: "harness@local",
      isSnapshot: false,
      effects,
      moreEffects: 0,
    });
    opHistory.set(id, { before: pendingBefore, after: structuredClone(snap) });
    return id;
  };
  const wcMoved = { kind: "workingCopy", label: "working copy moved" };
  const setWorkingCopy = (snap, id) => {
    const old = requireNode(snap, snap.workingCopy);
    if (old && old.kind === "workingCopy") old.kind = "mutable";
    snap.workingCopy = id;
    snap.workspaces.forEach((ws) => {
      if (ws.isDefault) ws.workingCopyNode = id;
    });
  };
  // The empty change `jj new` (or abandoning @) leaves you on.
  const spawnedIds = new Set();
  const spawnWorkingCopy = (snap, parent) => {
    const id = `wn${String(mutationIndex).padStart(2, "0")}pqzu`;
    spawnedIds.add(id);
    snap.nodes.unshift({
      id,
      changeId: id,
      commitId: `0e${String(mutationIndex).padStart(2, "0")}4af9`,
      description: "",
      author: "harness",
      timestamp: "2026-06-10T13:01:00Z",
      kind: "workingCopy",
      parents: [parent],
      elidedParents: [],
      bookmarks: [],
      isEmpty: true,
      hasConflict: false,
      isDivergent: false,
    });
    setWorkingCopy(snap, id);
    snap.workstreams.forEach((ws) => (ws.isActive = false));
    const owner = snap.workstreams.find((ws) => ws.nodeIds[0] === parent);
    if (owner) {
      owner.nodeIds.unshift(id);
      owner.isActive = true;
    } else {
      snap.workstreams.unshift({
        id: `ws-${id}`,
        title: "Anonymous work",
        nodeIds: [id],
        bookmark: null,
        isActive: true,
        behindTrunk: 0,
      });
    }
    return id;
  };
  const isAncestorOf = (snap, ancestor, descendant) => {
    const queue = [descendant];
    const seen = new Set();
    while (queue.length > 0) {
      const id = queue.pop();
      if (id === ancestor) return true;
      if (seen.has(id)) continue;
      seen.add(id);
      const node = requireNode(snap, id);
      if (node) queue.push(...node.parents, ...node.elidedParents);
    }
    return false;
  };
  const countDescendants = (snap, id) => {
    const seen = new Set([id]);
    const queue = [id];
    let count = 0;
    while (queue.length > 0) {
      const current = queue.pop();
      snap.nodes.forEach((n) => {
        if (n.parents.includes(current) && !seen.has(n.id)) {
          seen.add(n.id);
          count += 1;
          queue.push(n.id);
        }
      });
    }
    return count;
  };
  const removeNode = (snap, id, newParents) => {
    const node = requireNode(snap, id);
    snap.nodes = snap.nodes.filter((n) => n.id !== id);
    snap.nodes.forEach((n) => {
      if (n.parents.includes(id)) {
        n.parents = n.parents
          .flatMap((p) => (p === id ? newParents : [p]))
          .filter((p, i, all) => all.indexOf(p) === i);
      }
    });
    // Removing one copy of a divergent change can settle the survivor.
    const copies = snap.nodes.filter((n) => n.changeId === node.changeId);
    if (copies.length === 1) copies[0].isDivergent = false;
    snap.workstreams.forEach((ws) => {
      ws.nodeIds = ws.nodeIds.filter((n) => n !== id);
    });
    snap.workstreams = snap.workstreams.filter((ws) => ws.nodeIds.length > 0);
    return node;
  };

  window.__TAURI_INTERNALS__ = {
    transformCallback: () => 0,
    invoke: (cmd, args) => {
      const snap = data.snapshot;
      if (snap && (cmd.endsWith("_change") || cmd.endsWith("_bookmark"))) {
        pendingBefore = structuredClone(snap);
      }
      const target = (id) => {
        const node = requireNode(snap, id);
        if (!node)
          return reject(
            "change_missing",
            `Change ${id} is not in the repository anymore`,
          );
        if (node.kind === "immutable")
          return reject(
            "immutable_change",
            `Change ${id} is immutable and cannot be modified`,
          );
        return node;
      };
      switch (cmd) {
        case "current_snapshot":
          return snap
            ? Promise.resolve(snap)
            : reject("harness", `no captured snapshot named "${snapName}"`);
        case "change_diff": {
          const diff = (data.diffs || {})[args?.changeId];
          if (diff) return Promise.resolve(diff);
          // Nodes added by stubbed mutations render as contentless changes;
          // captured nodes without a diff keep the loud setup hint.
          if (spawnedIds.has(args?.changeId))
            return Promise.resolve({
              id: args?.changeId,
              from: null,
              files: [],
              truncated: false,
            });
          return reject(
            "harness",
            `no captured diff for ${args?.changeId} — pass --diff ${args?.changeId} to setup`,
          );
        }
        case "compare_diff": {
          const from = args?.fromChangeId;
          const to = args?.toChangeId;
          const captured = (data.compares || {})[`${from}..${to}`];
          if (captured) return Promise.resolve(captured);
          for (const id of [from, to]) {
            if (!requireNode(snap, id))
              return reject(
                "change_missing",
                `Change ${id} is not in the repository anymore`,
              );
          }
          // Fallback mirrors the Rust mock: union the captured per-change
          // diffs along the first-parent chain from `to` down to `from`,
          // newest touch winning per path (adds staying adds). Real
          // comparisons should be captured via setup's --compare FROM:TO.
          const chain = [];
          let cursor = to;
          let reached = false;
          while (cursor && !chain.includes(cursor)) {
            if (cursor === from) {
              reached = true;
              break;
            }
            chain.push(cursor);
            const node = requireNode(snap, cursor);
            cursor = node ? (node.parents[0] ?? node.elidedParents[0]) : null;
          }
          const files = [];
          for (const id of (reached ? chain : [to]).reverse()) {
            for (const file of ((data.diffs || {})[id] || {}).files || []) {
              const index = files.findIndex((f) => f.path === file.path);
              if (index === -1) files.push(file);
              else if (files[index].status === "added" && file.status !== "removed")
                files[index] = { ...file, status: "added" };
              else files[index] = file;
            }
          }
          files.sort((a, b) => a.path.localeCompare(b.path));
          return Promise.resolve({ id: to, from, files, truncated: false });
        }
        case "describe_change": {
          const node = target(args?.changeId);
          if (node instanceof Promise) return node;
          node.description = (args?.description ?? "").trim();
          const opId = pushOp(snap, `describe commit ${node.commitId}`);
          return Promise.resolve({
            operationId: opId,
            summary: `Described ${node.id}`,
            targetChange: node.id,
          });
        }
        case "new_change": {
          const parent = requireNode(snap, args?.parentChangeId);
          if (!parent)
            return reject(
              "change_missing",
              `Change ${args?.parentChangeId} is not in the repository anymore`,
            );
          const newId = spawnWorkingCopy(snap, parent.id);
          const opId = pushOp(snap, "new empty commit", [wcMoved]);
          return Promise.resolve({
            operationId: opId,
            summary: `Started ${newId} on ${parent.id}`,
            targetChange: newId,
          });
        }
        case "edit_change": {
          const node = target(args?.changeId);
          if (node instanceof Promise) return node;
          if (snap.workingCopy === node.id)
            return Promise.resolve({
              operationId: null,
              summary: `${node.id} is already the working copy`,
              targetChange: node.id,
            });
          setWorkingCopy(snap, node.id);
          node.kind = "workingCopy";
          snap.workstreams.forEach((ws) => {
            ws.isActive = ws.nodeIds.includes(node.id);
          });
          const opId = pushOp(snap, `edit commit ${node.commitId}`, [wcMoved]);
          return Promise.resolve({
            operationId: opId,
            summary: `Editing ${node.id}`,
            targetChange: node.id,
          });
        }
        case "abandon_change": {
          const node = target(args?.changeId);
          if (node instanceof Promise) return node;
          const effects = snap.bookmarks
            .filter((b) => b.target === node.id)
            .map((b) => ({ kind: "bookmark", label: `${b.name} deleted` }));
          snap.bookmarks = snap.bookmarks.filter((b) => b.target !== node.id);
          removeNode(snap, node.id, node.parents);
          if (snap.workingCopy === node.id && node.parents[0]) {
            spawnWorkingCopy(snap, node.parents[0]);
            effects.push(wcMoved);
          }
          const opId = pushOp(snap, `abandon commit ${node.commitId}`, effects);
          return Promise.resolve({
            operationId: opId,
            summary: `Abandoned ${node.id}`,
            targetChange: node.parents[0] ?? null,
          });
        }
        case "squash_change": {
          const node = target(args?.changeId);
          if (node instanceof Promise) return node;
          if (node.parents.length !== 1)
            return reject(
              "mutation_failed",
              `${node.id} is a merge; squashing into multiple parents is ambiguous`,
            );
          const parent = target(node.parents[0]);
          if (parent instanceof Promise) return parent;
          const dest = parent.description.trim();
          const src = node.description.trim();
          parent.description =
            dest === "" ? src : src === "" ? dest : `${dest}\n\n${src}`;
          parent.bookmarks.push(...node.bookmarks);
          parent.isEmpty = false;
          const effects = snap.bookmarks
            .filter((b) => b.target === node.id)
            .map((b) => ({ kind: "bookmark", label: `${b.name} moved` }));
          snap.bookmarks.forEach((b) => {
            if (b.target === node.id) b.target = parent.id;
          });
          removeNode(snap, node.id, [parent.id]);
          if (snap.workingCopy === node.id) {
            spawnWorkingCopy(snap, parent.id);
            effects.push(wcMoved);
          }
          const opId = pushOp(
            snap,
            `squash commits into ${parent.commitId}`,
            effects,
          );
          return Promise.resolve({
            operationId: opId,
            summary: `Squashed ${node.id} into ${parent.id}`,
            targetChange: parent.id,
          });
        }
        case "split_change": {
          const node = target(args?.changeId);
          if (node instanceof Promise) return node;
          const diff = (data.diffs || {})[node.id];
          const all = (diff?.files || []).map((f) => f.path);
          const selection = args?.selection || [];
          const wholePaths = selection.filter((s) => !s.hunks).map((s) => s.path);
          const partial = selection.filter((s) => s.hunks);
          const whole = all.filter((p) => wholePaths.includes(p));
          const partialSelected = all.filter((p) =>
            partial.some((s) => s.path === p),
          );
          if (whole.length + partialSelected.length === 0)
            return reject(
              "mutation_failed",
              `none of the selected files change anything in ${node.id}`,
            );
          if (whole.length === all.length)
            return reject(
              "mutation_failed",
              `the selection covers every change in ${node.id}; there would be nothing left to split off`,
            );
          // jj's split rule: the checked files stay in the change (same id,
          // new description); a new change on top takes everything else and
          // inherits bookmarks, children, and working-copy status.
          const newId = `wn${String(mutationIndex).padStart(2, "0")}pqzu`;
          spawnedIds.add(newId);
          const remainder = {
            id: newId,
            changeId: newId,
            commitId: `e5${String(mutationIndex).padStart(2, "0")}9b2c`,
            description: node.description,
            author: node.author,
            timestamp: node.timestamp,
            kind: node.kind,
            parents: [node.id],
            elidedParents: [],
            bookmarks: node.bookmarks,
            isEmpty: false,
            hasConflict: false,
            isDivergent: false,
          };
          node.description = (args?.description ?? "").trim();
          node.bookmarks = [];
          if (node.kind === "workingCopy") node.kind = "mutable";
          snap.nodes.forEach((n) => {
            if (n !== node && n.parents.includes(node.id)) {
              n.parents = n.parents.map((p) => (p === node.id ? newId : p));
            }
          });
          const effects = snap.bookmarks
            .filter((b) => b.target === node.id)
            .map((b) => ({ kind: "bookmark", label: `${b.name} moved` }));
          snap.bookmarks.forEach((b) => {
            if (b.target === node.id) b.target = newId;
          });
          snap.nodes.splice(snap.nodes.indexOf(node), 0, remainder);
          if (snap.workingCopy === node.id) {
            setWorkingCopy(snap, newId);
            effects.push(wcMoved);
          }
          snap.workstreams.forEach((ws) => {
            const at = ws.nodeIds.indexOf(node.id);
            if (at !== -1) ws.nodeIds.splice(at, 0, newId);
          });
          // Partition the captured diff so each half renders its own
          // files — at hunk granularity for hunk selections, matched by
          // the same coordinates the backend verifies.
          if (diff) {
            const coords = (h) => ({
              oldStart: h.oldStart,
              newStart: h.newStart,
              oldLines: h.lines.filter((l) => l.kind !== "added").length,
              newLines: h.lines.filter((l) => l.kind !== "removed").length,
            });
            const matches = (h, want) => {
              const c = coords(h);
              return (
                c.oldStart === want.oldStart &&
                c.newStart === want.newStart &&
                c.oldLines === want.oldLines &&
                c.newLines === want.newLines
              );
            };
            const carved = [];
            const rest = [];
            for (const f of diff.files) {
              const sel = partial.find((s) => s.path === f.path);
              if (wholePaths.includes(f.path)) {
                carved.push(f);
              } else if (sel && f.content?.kind === "text") {
                const chosen = f.content.hunks.filter((h) =>
                  sel.hunks.some((want) => matches(h, want)),
                );
                const left = f.content.hunks.filter((h) => !chosen.includes(h));
                if (chosen.length)
                  carved.push({ ...f, content: { ...f.content, hunks: chosen } });
                if (left.length || !chosen.length)
                  rest.push({ ...f, content: { ...f.content, hunks: left } });
              } else {
                rest.push(f);
              }
            }
            data.diffs[newId] = {
              id: newId,
              from: null,
              files: rest,
              truncated: false,
            };
            data.diffs[node.id] = { ...diff, files: carved };
          }
          const kept =
            partialSelected.length === 0
              ? `${whole.length} file${whole.length === 1 ? "" : "s"}`
              : whole.length === 0
                ? `parts of ${partialSelected.length} file${
                    partialSelected.length === 1 ? "" : "s"
                  }`
                : `${whole.length} file${
                    whole.length === 1 ? "" : "s"
                  } and parts of ${partialSelected.length} more`;
          const opId = pushOp(snap, `split commit ${node.commitId}`, effects);
          return Promise.resolve({
            operationId: opId,
            summary: `Split ${node.id}: kept ${kept}, the rest moved to ${newId}`,
            targetChange: node.id,
          });
        }
        case "rebase_change":
        case "move_change": {
          const node = target(args?.changeId);
          if (node instanceof Promise) return node;
          const dest = requireNode(snap, args?.destinationId);
          if (!dest)
            return reject(
              "change_missing",
              `Change ${args?.destinationId} is not in the repository anymore`,
            );
          if (dest.id === node.id)
            return reject(
              "mutation_failed",
              `cannot rebase ${node.id} onto itself`,
            );
          const withDescendants = cmd === "rebase_change";
          if (withDescendants && isAncestorOf(snap, node.id, dest.id))
            return reject(
              "mutation_failed",
              `cannot rebase ${node.id} onto its own descendant ${dest.id}`,
            );
          const alreadyInPlace =
            node.parents.length === 1 && node.parents[0] === dest.id;
          const childCount = countDescendants(snap, node.id);
          if (alreadyInPlace && (withDescendants || childCount === 0))
            return Promise.resolve({
              operationId: null,
              summary: `${node.id} is already on ${dest.id}`,
              targetChange: node.id,
            });
          if (!withDescendants) {
            // A lone move extracts the change: its children adopt its
            // current parents before it lands on the destination.
            const oldParents = [...node.parents];
            snap.nodes.forEach((n) => {
              if (n !== node && n.parents.includes(node.id)) {
                n.parents = n.parents
                  .flatMap((p) => (p === node.id ? oldParents : [p]))
                  .filter((p, i, all) => all.indexOf(p) === i);
              }
            });
          }
          node.parents = [dest.id];
          // A rebase rewrites the same change ids in place, so bookmarks and
          // the working copy report no effects — matching the real backend.
          const opId = pushOp(
            snap,
            `rebase commit ${node.commitId}${withDescendants ? " and descendants" : ""}`,
          );
          const plural = (n) => (n === 1 ? "" : "s");
          const summary = withDescendants
            ? childCount === 0
              ? `Rebased ${node.id} onto ${dest.id}`
              : `Rebased ${node.id} and ${childCount} descendant${plural(childCount)} onto ${dest.id}`
            : alreadyInPlace
              ? `Moved ${childCount} descendant${plural(childCount)} of ${node.id} onto ${dest.id}`
              : `Moved ${node.id} onto ${dest.id}`;
          return Promise.resolve({
            operationId: opId,
            summary,
            targetChange: node.id,
          });
        }
        case "create_bookmark": {
          const name = (args?.name ?? "").trim();
          if (!name)
            return reject("mutation_failed", "bookmark name cannot be empty");
          if (snap.bookmarks.some((b) => b.name === name))
            return reject(
              "mutation_failed",
              `bookmark “${name}” already exists`,
            );
          const node = requireNode(snap, args?.changeId);
          if (!node)
            return reject(
              "change_missing",
              `Change ${args?.changeId} is not in the repository anymore`,
            );
          snap.bookmarks.push({
            name,
            target: node.id,
            remote: null,
            sync: "localOnly",
            isTrunk: false,
            isLocal: true,
          });
          node.bookmarks.push(name);
          const owner = snap.workstreams.find(
            (ws) => !ws.bookmark && ws.nodeIds.includes(node.id),
          );
          if (owner) owner.bookmark = name;
          const opId = pushOp(
            snap,
            `create bookmark ${name} pointing to commit ${node.commitId}`,
            [{ kind: "bookmark", label: `${name} created` }],
          );
          return Promise.resolve({
            operationId: opId,
            summary: `Created ${name} on ${node.id}`,
            targetChange: node.id,
          });
        }
        case "move_bookmark": {
          const bookmark = snap.bookmarks.find((b) => b.name === args?.name);
          if (!bookmark)
            return reject(
              "bookmark_missing",
              `There is no local bookmark named “${args?.name}”`,
            );
          const node = requireNode(snap, args?.changeId);
          if (!node)
            return reject(
              "change_missing",
              `Change ${args?.changeId} is not in the repository anymore`,
            );
          if (bookmark.target === node.id)
            return Promise.resolve({
              operationId: null,
              summary: `${bookmark.name} already points at ${node.id}`,
              targetChange: node.id,
            });
          snap.nodes.forEach(
            (n) => (n.bookmarks = n.bookmarks.filter((b) => b !== bookmark.name)),
          );
          node.bookmarks.push(bookmark.name);
          bookmark.target = node.id;
          if (bookmark.remote && bookmark.sync === "synced")
            bookmark.sync = "ahead";
          snap.workstreams.forEach((ws) => {
            if (ws.bookmark === bookmark.name && !ws.nodeIds.includes(node.id))
              ws.bookmark = null;
          });
          const adopter = snap.workstreams.find(
            (ws) => !ws.bookmark && ws.nodeIds.includes(node.id),
          );
          if (adopter) adopter.bookmark = bookmark.name;
          const opId = pushOp(
            snap,
            `point bookmark ${bookmark.name} to commit ${node.commitId}`,
            [{ kind: "bookmark", label: `${bookmark.name} moved` }],
          );
          return Promise.resolve({
            operationId: opId,
            summary: `Moved ${bookmark.name} to ${node.id}`,
            targetChange: node.id,
          });
        }
        case "rename_bookmark": {
          const bookmark = snap.bookmarks.find((b) => b.name === args?.oldName);
          if (!bookmark)
            return reject(
              "bookmark_missing",
              `There is no local bookmark named “${args?.oldName}”`,
            );
          const next = (args?.newName ?? "").trim();
          if (!next)
            return reject("mutation_failed", "bookmark name cannot be empty");
          if (snap.bookmarks.some((b) => b.name === next))
            return reject(
              "mutation_failed",
              `bookmark “${next}” already exists`,
            );
          const old = bookmark.name;
          bookmark.name = next;
          // Like the CLI: tracked remote bookmarks keep the old name until
          // push, so the renamed bookmark starts local-only.
          bookmark.remote = null;
          bookmark.sync = "localOnly";
          snap.nodes.forEach(
            (n) => (n.bookmarks = n.bookmarks.map((b) => (b === old ? next : b))),
          );
          snap.workstreams.forEach((ws) => {
            if (ws.bookmark === old) ws.bookmark = next;
          });
          const opId = pushOp(snap, `rename bookmark ${old} to ${next}`, [
            { kind: "bookmark", label: `${next} created` },
            { kind: "bookmark", label: `${old} deleted` },
          ]);
          return Promise.resolve({
            operationId: opId,
            summary: `Renamed ${old} to ${next}`,
            targetChange: bookmark.target,
          });
        }
        case "delete_bookmark": {
          const bookmark = snap.bookmarks.find((b) => b.name === args?.name);
          if (!bookmark)
            return reject(
              "bookmark_missing",
              `There is no local bookmark named “${args?.name}”`,
            );
          snap.bookmarks = snap.bookmarks.filter((b) => b !== bookmark);
          snap.nodes.forEach(
            (n) => (n.bookmarks = n.bookmarks.filter((b) => b !== bookmark.name)),
          );
          snap.workstreams.forEach((ws) => {
            if (ws.bookmark === bookmark.name) ws.bookmark = null;
          });
          const opId = pushOp(snap, `delete bookmark ${bookmark.name}`, [
            { kind: "bookmark", label: `${bookmark.name} deleted` },
          ]);
          return Promise.resolve({
            operationId: opId,
            summary: `Deleted ${bookmark.name}`,
            targetChange: bookmark.target,
          });
        }
        case "revert_operation": {
          const opRow = (snap?.operations || []).find((o) => o.id === args?.opId);
          if (!opRow)
            return reject(
              "operation_missing",
              `Operation ${args?.opId} is not in the repository's operation log`,
            );
          const rec = opHistory.get(args?.opId);
          if (!rec || !rec.before)
            return reject(
              "mutation_failed",
              "the harness can only revert operations it recorded",
            );
          const label = opRow.description
            ? `“${opRow.description}”`
            : `operation ${opRow.id}`;
          Object.assign(snap, structuredClone(rec.before));
          // jj's op log keeps the reverted operation's row.
          snap.operations.unshift({ ...opRow, isCurrent: false });
          const opId = pushOp(snap, `revert operation ${args.opId}`);
          return Promise.resolve({
            operationId: opId,
            summary: `Reverted ${label}`,
            targetChange: snap.workingCopy,
          });
        }
        case "restore_operation": {
          const index = (snap?.operations || []).findIndex(
            (o) => o.id === args?.opId,
          );
          if (index === -1)
            return reject(
              "operation_missing",
              `Operation ${args?.opId} is not in the repository's operation log`,
            );
          const opRow = snap.operations[index];
          if (opRow.isCurrent)
            return Promise.resolve({
              operationId: null,
              summary: "The repo is already in this state",
              targetChange: null,
            });
          const label = opRow.description
            ? `“${opRow.description}”`
            : `operation ${opRow.id}`;
          // State comes from the clone taken right after the target op (or
          // the pristine capture for ops older than the scenario); rows
          // newer than the target stay listed, like jj's op log.
          const newer = snap.operations
            .slice(0, index)
            .map((o) => ({ ...o, isCurrent: false }));
          const rec = opHistory.get(args?.opId);
          Object.assign(
            snap,
            structuredClone(rec ? rec.after : pristine),
          );
          const seen = new Set();
          snap.operations = [...newer, ...snap.operations].filter(
            (o) => !seen.has(o.id) && seen.add(o.id),
          );
          snap.operations.forEach((o) => (o.isCurrent = false));
          const opId = pushOp(snap, `restore to operation ${args.opId}`);
          return Promise.resolve({
            operationId: opId,
            summary: `Restored to ${label}`,
            targetChange: snap.workingCopy,
          });
        }
        case "resolve_conflict": {
          // `resolvewait=1` keeps the call pending forever: the real command
          // blocks while the external merge tool's window is open, and this
          // is how the waiting state gets screenshotted.
          if (params.get("resolvewait")) return new Promise(() => {});
          const node = requireNode(snap, args?.changeId);
          if (!node)
            return reject(
              "change_missing",
              `Change ${args?.changeId} is not in the repository anymore`,
            );
          const path = args?.filePath;
          const item = (snap.conflicts || []).find(
            (c) =>
              c.kind === "file" &&
              c.nodeId === node.id &&
              (c.paths || []).includes(path),
          );
          if (!item)
            return reject(
              "mutation_failed",
              `${path} has no conflict in ${node.id}`,
            );
          // Mirror the Rust mock: the path leaves its item; an emptied item
          // is done and its node stops rendering as conflicted.
          item.paths = item.paths.filter((p) => p !== path);
          if (item.paths.length === 0 && !item.morePaths) {
            snap.conflicts = snap.conflicts.filter((c) => c !== item);
            node.hasConflict = false;
          }
          const opId = pushOp(
            snap,
            `Resolve conflicts in commit ${node.commitId}`,
          );
          return Promise.resolve({
            operationId: opId,
            summary: `Resolved ${path} in ${node.id}`,
            targetChange: node.id,
          });
        }
        case "update_stale_workspace": {
          // Mirror the real backend: only the current workspace's item can
          // recover, the checkout records no operation, and the selection
          // follows the fresh working copy.
          const current = (snap?.workspaces || []).find((w) => w.isCurrent);
          const item = (snap?.conflicts || []).find(
            (c) => c.kind === "staleWorkspace" && c.workspace === current?.name,
          );
          if (!current || !item)
            return Promise.resolve({
              operationId: null,
              summary: "The workspace is not stale",
              targetChange: null,
            });
          current.isStale = false;
          snap.conflicts = snap.conflicts.filter((c) => c !== item);
          return Promise.resolve({
            operationId: null,
            summary: `Updated the workspace to ${snap.workingCopy}`,
            targetChange: snap.workingCopy,
          });
        }
        case "plugin:event|listen":
          return Promise.resolve(0);
        case "plugin:store|load":
          // The path doubles as the resource id so gets can tell the
          // license store from the recent-repos store.
          return Promise.resolve(args?.path);
        case "plugin:store|get":
          // The built site runs without dev-mode license simulation, so
          // answer the license read like a registered copy — matching the
          // dev default (themes unlocked) instead of the unregistered gate.
          if (args?.rid === "license.json" && args?.key === "state") {
            return Promise.resolve([
              {
                key: "JIJI-HARNESS",
                activationId: "harness",
                deviceId: "harness",
                status: "granted",
                plan: "personal",
                limitActivations: null,
                expiresAt: null,
                registeredAt: 1765000000000,
                lastValidatedAt: Date.now(),
              },
              true,
            ]);
          }
          return Promise.resolve([null, false]);
        default:
          return Promise.resolve(null);
      }
    },
  };

  const click = (selector) => {
    const el = document.querySelector(selector);
    if (el) el.click();
    return el !== null;
  };

  const steps = [];
  const section = params.get("section");
  if (section) {
    const label = section[0].toUpperCase() + section.slice(1);
    steps.push(() => click(`button[aria-label="${label}"]:not(:disabled)`));
  }
  const sibling = params.get("sibling");
  if (sibling) {
    steps.push(() => click(`.siblings .sibling:nth-child(${sibling})`));
  }
  const row = params.get("click");
  if (row) steps.push(() => click(`[data-node-id="${row}"]`));
  // Conflict-inbox flows: click a jumpable item card, or one candidate
  // chip of a conflicted bookmark (combine with section=conflicts).
  const conflictGo = params.get("cgo");
  if (conflictGo) {
    steps.push(() => click(`button[data-conflict-id="${conflictGo}"]`));
  }
  const conflictTarget = params.get("ctarget");
  if (conflictTarget) {
    steps.push(() => click(`[data-conflict-target="${conflictTarget}"]`));
  }
  const collapse = params.get("collapse");
  if (collapse !== null) {
    steps.push(() => !document.querySelector(".diff-view .skeleton"));
    steps.push(() =>
      click(`.diff-view .file[data-file-index="${collapse}"] .head-toggle`),
    );
  }
  // Resolve flows: click a Resolve button by its conflicted path — the
  // inbox card's (section=conflicts) or the diff file header's (click=<id>
  // first). Waits for the stubbed mutation's breadcrumb, or for the waiting
  // state when resolvewait=1 keeps the fake tool open.
  const resolvePath = params.get("resolve");
  if (resolvePath) {
    steps.push(() => {
      const button = document.querySelector(
        `.resolve[data-resolve-path="${resolvePath}"]`,
      );
      if (!button || button.disabled) return false;
      button.click();
      return true;
    });
    steps.push(() =>
      params.get("resolvewait")
        ? document.querySelector(".resolve.waiting") !== null
        : document.querySelector(".breadcrumb") !== null,
    );
  }
  // Stale-workspace recovery: click the inbox card's Update workspace
  // button and wait for the stubbed mutation to settle the item.
  if (params.get("upws")) {
    steps.push(() => {
      const button = document.querySelector("[data-update-workspace]");
      if (!button || button.disabled) return false;
      button.click();
      return true;
    });
    steps.push(() => !document.querySelector("[data-update-workspace]"));
  }
  const open = params.get("open");
  if (open === "files") steps.push(() => click(".files-button"));
  if (open === "body") steps.push(() => click(".disclose"));
  if (open === "theme") steps.push(() => click('button[aria-label="Theme"]'));
  const swatch = params.get("swatch");
  if (swatch) {
    steps.push(() => click('button[aria-label="Theme"]'));
    steps.push(() => click(`.swatch-item[title="${swatch}"]`));
  }
  if (open === "describe") {
    steps.push(() => click(".describe-button") || click(".edit"));
  }
  if (open === "squash" || open === "abandon") {
    steps.push(() => click(`[data-action="${open}"]`));
  }
  if (open === "bookmark") steps.push(() => click('[data-action="bookmark"]'));
  if (open === "rebase") steps.push(() => click('[data-action="rebase"]'));
  if (open === "split") steps.push(() => click('[data-action="split"]'));
  if (open === "compare") steps.push(() => click('[data-action="compare"]'));
  if (params.get("palette")) {
    steps.push(() => click('[data-action="palette"]'));
  }
  const paletteQuery = params.get("pq");
  if (paletteQuery) {
    steps.push(() => {
      const input = document.querySelector(".palette-input");
      if (!input) return false;
      input.value = paletteQuery;
      input.dispatchEvent(new Event("input", { bubbles: true }));
      return true;
    });
  }
  const paletteRun = params.get("prun");
  if (paletteRun) {
    steps.push(() => click(`.palette-row[data-command="${paletteRun}"]`));
    steps.push(() => !document.querySelector(".palette-input"));
  }
  const compareTo = params.get("compare");
  if (compareTo) {
    const selector = ["parent", "trunk", "base"].includes(compareTo)
      ? `.dest-row[data-compare="${compareTo}"]`
      : `.dest-row[data-compare-from="${compareTo}"]`;
    steps.push(() => click(selector));
    // Wait for the comparison to land: the chip flips state immediately,
    // then the refetched diff replaces the loading skeleton.
    steps.push(() =>
      compareTo === "parent"
        ? !document.querySelector(".compare-group.active")
        : document.querySelector(".compare-group.active") !== null,
    );
    steps.push(() => !document.querySelector(".diff-view .skeleton"));
  }
  const rebaseMode = params.get("mode");
  if (rebaseMode) {
    steps.push(() => click(`.mode-toggle [data-mode="${rebaseMode}"]`));
  }
  // Split panel: check file rows and hunk rows, then type the carved
  // change's description.
  const splitFiles = params.get("sfiles");
  if (splitFiles) {
    for (const path of splitFiles.split(",")) {
      steps.push(() => click(`.dest-row[data-splitfile="${path}"]`));
    }
  }
  const splitHunks = params.get("shunks");
  if (splitHunks) {
    for (const entry of splitHunks.split(",")) {
      const [path, indices] = entry.split("@");
      steps.push(() => click(`.hunk-toggle[data-splitexpand="${path}"]`));
      for (const index of (indices || "").split(".").filter(Boolean)) {
        steps.push(() => click(`.dest-row[data-splithunk="${path}@${index}"]`));
      }
    }
  }
  const splitDesc = params.get("sdesc");
  if (splitDesc) {
    steps.push(() => {
      const textarea = document.querySelector(".split-desc");
      if (!textarea) return false;
      textarea.value = splitDesc;
      textarea.dispatchEvent(new Event("input", { bubbles: true }));
      return true;
    });
  }
  const rebaseDest = params.get("dest");
  if (rebaseDest) {
    steps.push(() => click(`.dest-row[data-dest="${rebaseDest}"]`));
  }
  const manageName = params.get("manage");
  if (manageName) {
    steps.push(() => click(`.bookmark-chip[data-bookmark="${manageName}"]`));
  }
  const fillBookmarkInput = (text) => () => {
    const input = document.querySelector(".bookmark-panel .name-input");
    if (!input) return false;
    input.value = text;
    input.dispatchEvent(new Event("input", { bubbles: true }));
    return true;
  };
  // The panel button stays disabled until the input binding flushes, so the
  // click step retries until it is actually clickable.
  const clickPanelGo = () => {
    const button = document.querySelector(".bookmark-panel .confirm-go");
    if (!button || button.disabled) return false;
    button.click();
    return true;
  };
  const createName = params.get("bookmark");
  if (createName) {
    steps.push(fillBookmarkInput(createName));
    steps.push(clickPanelGo);
    steps.push(() => document.querySelector(".breadcrumb") !== null);
  }
  const moveName = params.get("movebm");
  if (moveName) {
    steps.push(() => click(`.move-row[data-bookmark="${moveName}"]`));
    steps.push(() => document.querySelector(".breadcrumb") !== null);
  }
  const renameText = params.get("rename");
  if (renameText) {
    steps.push(fillBookmarkInput(renameText));
    steps.push(clickPanelGo);
    steps.push(() => document.querySelector(".breadcrumb") !== null);
  }
  if (params.get("delete")) {
    steps.push(() => click(".bm-delete"));
    steps.push(() => document.querySelector(".breadcrumb") !== null);
  }
  const obmRename = params.get("obmrename");
  if (obmRename) {
    steps.push(() => click(`.bm-row button[title="Rename ${obmRename}"]`));
  }
  const obmDelete = params.get("obmdelete");
  if (obmDelete) {
    steps.push(() => click(`.bm-row button[title="Delete ${obmDelete}"]`));
  }
  const action = params.get("action");
  if (action === "new" || action === "edit") {
    steps.push(() => click(`[data-action="${action}"]`));
    // Wait for the stubbed mutation round-trip: the breadcrumb appears in
    // the status bar once the refreshed snapshot lands.
    steps.push(() => document.querySelector(".breadcrumb") !== null);
  }
  // Row drag-and-drop, driven with synthetic PointerEvents: the controller
  // reads coordinates (elementFromPoint), not event targets, so dispatching
  // pointermove/up on window with real clientX/Y behaves like a real drag.
  const dragId = params.get("drag");
  const dragTo = params.get("dragto");
  if (dragId && dragTo) {
    const alt = params.get("dragalt") === "1";
    const center = (el) => {
      const r = el.getBoundingClientRect();
      return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
    };
    const pev = (type, target, pos) =>
      target.dispatchEvent(
        new PointerEvent(type, {
          bubbles: true,
          pointerId: 1,
          isPrimary: true,
          button: 0,
          buttons: type === "pointerup" ? 0 : 1,
          clientX: pos.x,
          clientY: pos.y,
          altKey: alt,
        }),
      );
    steps.push(() => {
      const source = document.querySelector(`[data-node-id="${dragId}"]`);
      const target = document.querySelector(`[data-node-id="${dragTo}"]`);
      if (!source || !target) return false;
      pev("pointerdown", source, center(source));
      pev("pointermove", window, center(target));
      return true;
    });
    // The plan card confirms the drag session is live before any release.
    steps.push(() => document.querySelector(".drag-card") !== null);
    if (params.get("drop")) {
      steps.push(() => {
        const target = document.querySelector(`[data-node-id="${dragTo}"]`);
        if (!target) return false;
        pev("pointerup", window, center(target));
        return true;
      });
      steps.push(() => document.querySelector(".breadcrumb") !== null);
    }
  }
  const opRevert = params.get("oprevert");
  if (opRevert) {
    steps.push(() =>
      click(`[data-op-id="${opRevert}"] [data-op-action="revert"]`),
    );
  }
  const opRestore = params.get("oprestore");
  if (opRestore) {
    steps.push(() =>
      click(`[data-op-id="${opRestore}"] [data-op-action="restore"]`),
    );
  }
  if (params.get("confirm")) {
    // Retry until the button is actually clickable: panels that need more
    // input first (e.g. a rebase destination) keep it disabled until the
    // bindings flush.
    steps.push(() => {
      const button = document.querySelector(".confirm-go");
      if (!button || button.disabled) return false;
      button.click();
      return true;
    });
    steps.push(() => !document.querySelector(".confirm-panel"));
  }
  const describeText = params.get("describe");
  if (describeText) {
    steps.push(() => {
      const textarea = document.querySelector(".describe-editor textarea");
      if (!textarea) return false;
      textarea.value = describeText;
      textarea.dispatchEvent(new Event("input", { bubbles: true }));
      return true;
    });
    steps.push(() => click(".editor-save"));
    // Wait for the save round-trip: the editor closes once the stubbed
    // mutation resolves and the snapshot re-renders.
    steps.push(() => !document.querySelector(".describe-editor"));
  }
  if (params.get("undo")) {
    steps.push(() => {
      const button = document.querySelector(".bc-undo");
      if (!button || button.disabled) return false;
      button.click();
      return true;
    });
    // The undo round-trip lands when the breadcrumb swaps to the revert.
    steps.push(
      () =>
        document
          .querySelector(".breadcrumb")
          ?.textContent.includes("Reverted") ?? false,
    );
  }
  if (params.get("expand")) steps.push(() => click(".run-toggle"));
  const scroll = params.get("scroll");
  if (scroll) {
    steps.push(() => {
      const el = document.querySelector(".diff-view .scroller, .view");
      if (el) el.scrollTop = Number(scroll);
      return el !== null;
    });
  }
  const scrollx = params.get("scrollx");
  if (scrollx) {
    steps.push(() => {
      const els = document.querySelectorAll(".lines");
      els.forEach((el) => (el.scrollLeft = Number(scrollx)));
      return els.length > 0;
    });
  }

  const poll = setInterval(() => {
    while (steps.length > 0 && steps[0]()) steps.shift();
    if (steps.length === 0) {
      clearInterval(poll);
      window.__HARNESS_DONE__ = true;
    }
  }, 100);
})();
