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
//   &fetch=wait|fail|moved the stubbed git_fetch (the upstream check that
//                        runs on open): never answers (the chip's checking
//                        state), rejects like an unreachable remote (the
//                        failed state), or finds the remote moved on every
//                        fetch — recording a fetch operation with a
//                        remote-bookmark effect (default: fetches nothing,
//                        records nothing)
//   &fetchnow=1          click the upstream chip's manual fetch and wait
//                        for the breadcrumb (pair with fetch=moved)
//   &forge=keychain|env|gh preset the GitHub connection's token source
//                        (default: none — the Publish section's connect
//                        state); the stubbed repo detection reads the
//                        captured snapshot's gitRemotes
//   &noremote=1          strip the captured snapshot's git remotes, so the
//                        Publish section's no-GitHub-remote empty state
//                        (and the remote-less shell) render on any capture
//   &prswait=1           the stubbed forge_prs never answers, capturing
//                        the PR-list loading skeleton
//   &planwait=1          the stubbed submit_plan/land_plan never answer,
//                        capturing the plan-card loading skeleton (pair
//                        with &splan= or &lplan=; the wait step is skipped
//                        since no plan card will arrive)
//   &fuser=<login>       the login the stubbed token verifies as
//                        (default jiji-dev)
//   &fwait=1             the stubbed forge_verify never resolves,
//                        capturing the "Verifying the token…" state
//   &ftoken=<text>       type into the Publish section's token input
//                        (the literal token "bad" is refused like an
//                        invalid PAT)
//   &fconnect=1          submit the token and wait for the connected card
//                        (or the inline error when the token was "bad")
//   &fafter=env|gh       which fallback token remains after disconnect
//                        (default: none)
//   &fdisconnect=1       click Disconnect on the connected card and wait
//                        for the connect state (or the managed-outside
//                        note when a fallback token remains)
//   &prs=<b:flags,...>   fabricate open-PR state for the stubbed forge_prs,
//                        one entry per bookmark/branch name with dot-joined
//                        flags: draft|merged|closed, approved|changes|
//                        review, passing|failing|pending, fork (excluded
//                        from the badge map like a real cross-fork PR),
//                        plus the PR-text states stale (the PR body carries
//                        Jiji's managed section for an older commit body —
//                        a re-submit plans the description update),
//                        edited (the user rewrote the managed section —
//                        respected, nothing plans) and conflict (both
//                        sides moved — the plan warns and leaves it).
//                        Fabricated titles mirror the bookmark's change
//                        title so plans stay quiet by default; retitled
//                        fabricates a hand-renamed PR instead (the
//                        markerless title-drift warning).
//                        Needs &forge= — badges only render once the
//                        connection is verified
//   &prstrunc=1          the fabricated report says it was capped at 100
//                        PRs (the Publish section states it)
//   &splan=<bookmark>    click that stack row in Publish and wait for its
//                        submit plan (a JS twin of plan_submit over the
//                        captured snapshot + &prs= state, incl. the
//                        reconcile fingerprints and stack-comment actions).
//                        Needs &forge=
//   &sprev=1             open the plan card's stack-comment preview
//                        disclosure
//   &sgo=1               click Publish on the derived plan and wait for
//                        the executed per-step results (stubbed: pushes
//                        mark bookmarks synced, created PRs join the
//                        fabricated open-PR state, text updates and stack
//                        comments persist so a replan settles idle)
//   &lplan=<bookmark>    click that stack row in Land a stack and wait for
//                        its land plan (a JS twin of plan_land over the
//                        captured snapshot + &prs= state). The candidate's
//                        readiness reads the &prs= review/CI flags; a
//                        `merged` flag on a lower bookmark is the
//                        merged-PR-recognition path (reconcile-only plan);
//                        the extra flag `unmergeable` fabricates GitHub's
//                        CONFLICTING mergeability. Needs &forge=
//   &lqueue=1            the trunk branch is protected by a merge queue
//                        (the plan enqueues instead of merging)
//   &lamon=1             auto-merge is already enabled on the candidate
//                        (nothing to run; the plan says GitHub is driving)
//   &lnoauto=1           the repo disallows auto-merge (wait states become
//                        blockers instead of an auto-merge hand-off)
//   &lgo=1               click Land on the derived plan and wait for the
//                        executed per-step results (stubbed: the merge
//                        marks the PR merged, the fetch fabricates the
//                        squash commit arriving on trunk, rebases reparent
//                        live, cleanup removes the landed bookmark and
//                        changes, so the refreshed graph and PR state
//                        follow the executed flow)
//   &alqueue=1           click the plan card's "Auto-land when ready" (pair
//                        with &lplan=) and wait for the job card. The
//                        stubbed job mirrors the engine's first poll:
//                        blocked plans wait (attention when they need the
//                        user), an actionable plan runs one round through
//                        the shared land executor, a hand-off or a still-
//                        stacked segment keeps watching, and a completed
//                        stack reads done — the status-bar activity chip
//                        follows the same state
//   &alstop=1            click the running job card's Stop and wait for
//                        the stopped state (only meaningful on a waiting
//                        job — a finished card's button is Dismiss)
//   &rfetch=<number>     click that PR row's "Fetch for review" and wait
//                        for the panel (needs &forge= + &prs=)
//   &rlookup=<number>    type into the by-number lookup and submit; waits
//                        for the panel (a fabricated number) or the
//                        lookup error (an unknown one)
//   &rname=<text>        retype the panel's bookmark name (a taken name
//                        shows the inline warning and disables Fetch)
//   &rgo=1               click Fetch in the open panel and wait for the
//                        done note (the stubbed fetch_pr lands the PR
//                        head as a fresh change on trunk wearing the
//                        bookmark, like the Rust mock)
//   &rrun=<number>       click that PR row's "Re-run failed CI" and wait
//                        for the outcome note (a failing fabricated PR
//                        re-runs one workflow; others answer the honest
//                        empty report)
//   &rrunhdr=1           click the workbench header's Re-run failed CI
//                        chip (after &click selects a change whose
//                        bookmark wears a failing PR) and wait for the
//                        requested state
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
//   &mode=new|into       pick where the split panel's selection goes (the
//                        same param drives the rebase scope toggle — only
//                        one panel is open at a time)
//   &sdest=<changeId>    pick that destination row in the split panel's
//                        into-mode (the stubbed squash_into moves the
//                        selected files/hunks into the destination's
//                        captured diff; a full selection abandons the
//                        emptied source like the real backend)
//   &open=compare        open the compare panel on the selection
//   &compare=<v>         pick a row in the open compare panel: parent|trunk|
//                        base or a change id (compare_diff is stubbed from
//                        captured comparisons, else a per-change-diff union)
//   &mode=stack|single   pick the rebase scope in the open rebase panel
//   &dest=<changeId>     pick that destination row in the open rebase panel
//   &desthover=<changeId> hover a destination row (rebase or split-into
//                        list) without picking it: the graph's rewrite
//                        preview scrubs to it
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
  // &noremote=1: a repo with no git remotes, so remote-less states (the
  // Publish empty state, the hidden upstream chip) render on any capture.
  if (params.get("noremote") && data.snapshot) data.snapshot.gitRemotes = [];

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

  // Forge connection stub state: which token source is active and who it
  // verifies as. Mirrors ForgeState + the keychain, not the snapshot — the
  // real connection also lives outside repo state.
  const forgeSourceByParam = { keychain: "keychain", env: "environment", gh: "ghCli" };
  const forge = {
    source: forgeSourceByParam[params.get("forge")] || null,
    login: params.get("fuser") || "jiji-dev",
    // PRs the stubbed submit_stack opened this session; forge_prs folds
    // them in so the refreshed count and badges follow the executed flow.
    createdPrs: [],
    // Stack comments and PR overrides (text rewrites, retargeted bases)
    // the stubbed submit/land flows made, keyed by PR number, so a replan
    // after an executed flow settles.
    comments: new Map(),
    prOverrides: new Map(),
    // Branches whose PR the stubbed land flow merged this session: their
    // fabricated PR reads merged and drops out of the byBranch attach map.
    landedBranches: new Set(),
    // The stubbed auto-land job's latest state (one job, like the host).
    autoland: null,
  };
  // JS twins of jiji-forge's reconcile fingerprints (FNV-1a 64) and
  // managed-body markers, enough for the plan twin to mirror the Rust
  // engine over fabricated PR bodies.
  const fnvFp = (text) => {
    let hash = 0xcbf29ce484222325n;
    for (const byte of new TextEncoder().encode(text)) {
      hash ^= BigInt(byte);
      hash = (hash * 0x100000001b3n) & 0xffffffffffffffffn;
    }
    return hash.toString(16).padStart(16, "0");
  };
  const DESC_START = "<!-- jiji:description -->";
  const DESC_END = "<!-- /jiji:description -->";
  const wrapManaged = (body, title) =>
    `${DESC_START}\n${body}\n${DESC_END}\n<!-- jiji:body-fp ${fnvFp(body)} -->` +
    (title != null ? `\n<!-- jiji:title-fp ${fnvFp(title)} -->` : "");
  const managedOf = (body) => {
    const start = body.indexOf(DESC_START);
    if (start === -1) return null;
    const end = body.indexOf(DESC_END, start + DESC_START.length);
    if (end === -1) return null;
    return body.slice(start + DESC_START.length, end).trim();
  };
  const storedFp = (body, kind) =>
    body.match(new RegExp(`<!-- jiji:${kind}-fp ([0-9a-f]+) -->`))?.[1] ?? null;
  const TRAILER_KEYS = new Set([
    "co-authored-by", "co-developed-by", "signed-off-by", "helped-by",
    "reviewed-by", "acked-by", "tested-by", "reported-by", "suggested-by",
    "change-id",
  ]);
  const titleBodyOf = (description, fallback) => {
    const text = (description || "").trim();
    if (!text) return { title: fallback, body: "" };
    const title = text.split("\n")[0];
    const lines = text.slice(title.length).trim().split("\n");
    let end = lines.length;
    while (end > 0) {
      const line = lines[end - 1].trim();
      if (!line) {
        end -= 1;
        continue;
      }
      const key = line.split(":")[0]?.trim().toLowerCase();
      if (line.includes(":") && TRAILER_KEYS.has(key) && line.split(":")[1]?.trim()) end -= 1;
      else break;
    }
    return { title, body: lines.slice(0, end).join("\n").trimEnd() };
  };
  // The visible half of the Rust engine's stack comment (the base64 data
  // line is an implementation detail the UI strips anyway).
  const renderStackComment = (entries, current) => {
    let body = "<!-- jiji:stack-info -->\n";
    body += "This pull request is part of a stack, in merge order:\n\n";
    for (const entry of entries) {
      if (current === entry.bookmark) body += `1. **\`${entry.bookmark}\` ← this PR**\n`;
      else if (entry.url) body += `1. [\`${entry.bookmark}\`](${entry.url})\n`;
      else body += `1. \`${entry.bookmark}\`\n`;
    }
    body += "\n---\n*This comment is kept up to date by Jiji.*\n";
    return body;
  };
  // A JS twin of the backend's GitHub detection, just enough for shots:
  // github.com HTTPS/SSH forms, origin > upstream > name order.
  const forgeRepoOf = (snap) => {
    const rank = (name) =>
      name === "origin" ? 0 : name === "upstream" ? 1 : 2;
    const remotes = [...(snap?.gitRemotes || [])].sort(
      (a, b) => rank(a.name) - rank(b.name) || a.name.localeCompare(b.name),
    );
    for (const remote of remotes) {
      const match =
        /^(?:https?:\/\/|git@|ssh:\/\/git@)((?:[\w-]+\.)?github\.com)[/:]([^/]+)\/([^/]+?)(?:\.git)?(?:\/.*)?$/.exec(
          remote.url,
        );
      if (match)
        return {
          provider: "gitHub",
          remote: remote.name,
          host: match[1],
          owner: match[2],
          name: match[3],
        };
    }
    return null;
  };
  const forgeStatusOf = (snap, login) => ({
    repo: forgeRepoOf(snap),
    auth: forge.source
      ? { source: forge.source, login }
      : { source: null, login: null },
  });
  // Fabricated open-PR state from the &prs= DSL, keyed to the captured
  // snapshot's bookmark names: `prs=branch:flag.flag,branch2:flag`, with
  // flags draft|merged|closed (state), approved|changes|review (review
  // decision), passing|failing|pending (CI rollup), and fork (a cross-fork
  // PR — excluded from byBranch like the backend's prs_by_branch rule).
  // Dot-joined because `+` in a query string decodes to a space (both are
  // tolerated anyway).
  const humanize = (branch) =>
    (branch.charAt(0).toUpperCase() + branch.slice(1)).replace(/-/g, " ");
  const forgePrsOf = (snap) => {
    const repo = forgeRepoOf(snap);
    // The commit-derived title/body a Jiji-created PR for this bookmark
    // would carry — the fabrication default, so plans stay quiet unless a
    // flag asks for drift. Like the engine, the text comes from the
    // segment's BOTTOM change: walk first parents down from the bookmark
    // until another local bookmark's change or immutable history.
    const expectedTextOf = (branch) => {
      const locals = new Set(
        (snap?.bookmarks || [])
          .filter((b) => b.isLocal && !b.isTrunk)
          .map((b) => b.name),
      );
      const nodes = new Map((snap?.nodes || []).map((n) => [n.id, n]));
      const target = (snap?.bookmarks || []).find((b) => b.name === branch)?.target;
      let bottom = nodes.get(target) ?? null;
      let cursor = bottom ? nodes.get(bottom.parents[0]) : null;
      while (
        cursor &&
        cursor.kind !== "immutable" &&
        !cursor.bookmarks.some((name) => locals.has(name))
      ) {
        bottom = cursor;
        cursor = nodes.get(cursor.parents[0]);
      }
      return titleBodyOf(bottom?.description, humanize(branch));
    };
    const prs = (params.get("prs") || "")
      .split(",")
      .filter(Boolean)
      .map((entry, index) => {
        const [branch, flagStr] = entry.split(":");
        const flags = new Set((flagStr || "").split(/[.+\s]+/).filter(Boolean));
        const state = flags.has("merged")
          ? "merged"
          : flags.has("closed")
            ? "closed"
            : "open";
        const review = flags.has("approved")
          ? "approved"
          : flags.has("changes")
            ? "changesRequested"
            : flags.has("review")
              ? "reviewRequired"
              : "none";
        const checks = flags.has("passing")
          ? "passing"
          : flags.has("failing")
            ? "failing"
            : flags.has("pending")
              ? "pending"
              : "none";
        const number = 101 + index;
        const expected = expectedTextOf(branch);
        const title = flags.has("retitled")
          ? `${humanize(branch)} (hand-renamed)`
          : expected.title;
        let body = null;
        if (flags.has("stale"))
          body = wrapManaged("An earlier take on this change.", title);
        else if (flags.has("edited"))
          // The managed text was rewritten by hand; the stored fingerprint
          // still matches the commit, so Jiji leaves it alone.
          body =
            `${DESC_START}\nA description someone rewrote on GitHub.\n${DESC_END}\n` +
            `<!-- jiji:body-fp ${fnvFp(expected.body)} -->\n` +
            `<!-- jiji:title-fp ${fnvFp(title)} -->`;
        else if (flags.has("conflict"))
          body = `${DESC_START}\nAn earlier take on this change.\n${DESC_END}\n<!-- jiji:body-fp 0000000000000000 -->`;
        // A `base-<branch>` flag fabricates a stacked PR's non-trunk base
        // (what the land reconcile's retarget row acts on).
        const baseFlag = [...flags].find((flag) => flag.startsWith("base-"));
        return {
          number,
          title,
          url: `https://github.com/${repo ? `${repo.owner}/${repo.name}` : "o/r"}/pull/${number}`,
          state,
          isDraft: flags.has("draft"),
          headBranch: branch,
          headCommit: "ad".repeat(20),
          headOwner: flags.has("fork") ? "someone-else" : (repo?.owner ?? null),
          baseBranch: baseFlag ? baseFlag.slice(5) : (snap?.trunkBookmark || "main"),
          body,
          review,
          checks,
        };
      })
      .map((pr) => ({ ...pr, ...(forge.prOverrides.get(pr.number) || {}) }));
    // Branches the stubbed land flow merged drop out entirely, like the
    // real batched query no longer answering the merged PR.
    const all = prs
      .concat(forge.createdPrs)
      .filter((pr) => !forge.landedBranches.has(pr.headBranch));
    const byBranch = {};
    for (const pr of all) {
      const sameRepo =
        repo &&
        pr.headOwner &&
        pr.headOwner.toLowerCase() === repo.owner.toLowerCase();
      if (sameRepo && !(pr.headBranch in byBranch)) byBranch[pr.headBranch] = pr;
    }
    return {
      report: { prs: all, truncated: Boolean(params.get("prstrunc")) },
      byBranch,
    };
  };

  // A JS twin of jiji-forge's plan_submit, just enough for shots: walk
  // first parents from the bookmark to the immutable base, segment at
  // local non-trunk bookmarks, compare against the fabricated PR state.
  // Blockers/warnings mirror the Rust wording the panel renders.
  const submitPlanOf = (snap, head) => {
    const repo = forgeRepoOf(snap);
    const prState = forgePrsOf(snap);
    const local = new Map(
      (snap.bookmarks || [])
        .filter((b) => b.isLocal && !b.isTrunk)
        .map((b) => [b.name, b]),
    );
    const state = local.get(head);
    if (!state) return null;
    const nodes = new Map(snap.nodes.map((n) => [n.id, n]));
    const chain = [];
    let cursor = nodes.get(state.target);
    while (cursor && cursor.kind !== "immutable") {
      chain.push(cursor);
      cursor = nodes.get(cursor.parents[0]);
    }
    chain.reverse();
    const segments = [];
    const actions = [];
    const prActions = [];
    const textActions = [];
    const blockers = [];
    const warnings = [];
    const live = [];
    let base = snap.trunkBookmark;
    let pending = [];
    for (const node of chain) {
      pending.push(node);
      const names = node.bookmarks
        .filter((name) => local.has(name))
        .sort(
          (a, b) =>
            (a !== head) - (b !== head) ||
            !(a in prState.byBranch) - !(b in prState.byBranch) ||
            a.localeCompare(b),
        );
      if (!names.length) continue;
      const name = names[0];
      const segNodes = pending;
      pending = [];
      const bottom = segNodes[0];
      const expected = titleBodyOf(bottom.description, name);
      const title = expected.title;
      const pr = prState.byBranch[name] || null;
      segments.push({
        bookmark: name,
        base,
        changeIds: segNodes.map((n) => n.id),
        title,
        pr,
      });
      if (segNodes.every((n) => n.isEmpty)) {
        warnings.push(
          `every change under \u{201c}${name}\u{201d} is empty; skipping its push and PR`,
        );
        base = name;
        continue;
      }
      const bm = local.get(name);
      if (bm.sync !== "synced" || !bm.remote) {
        for (const n of segNodes) {
          if (!n.description)
            blockers.push(`${n.changeId} has no description; describe it first`);
          if (n.hasConflict)
            blockers.push(`${n.changeId} has conflicts; resolve them first`);
        }
        actions.push({ kind: "push", bookmark: name, create: bm.sync === "localOnly" });
      }
      if (pr) {
        if (pr.baseBranch !== base)
          prActions.push({
            kind: "retargetPr",
            number: pr.number,
            bookmark: name,
            fromBase: pr.baseBranch,
            toBase: base,
          });
        // The text-reconcile twin: mirrors jiji-forge's plan_pr_text over
        // the fabricated bodies (markerless empty/identical adoption, the
        // fingerprint three-way, drift warnings).
        if (bottom.description) {
          const body = pr.body || "";
          const managed = managedOf(body);
          const titleFp = storedFp(body, "title");
          let newTitle = null;
          if (pr.title !== expected.title) {
            if (titleFp === null) {
              if (segNodes.length === 1)
                warnings.push(
                  `#${pr.number} (${name}): the PR title (\u{201c}${pr.title}\u{201d}) ` +
                    `differs from the commit (\u{201c}${expected.title}\u{201d}) and ` +
                    `Jiji does not know which is intended`,
                );
            } else if (titleFp === fnvFp(pr.title)) newTitle = expected.title;
            else if (titleFp !== fnvFp(expected.title))
              warnings.push(
                `#${pr.number} (${name}): the PR title and the commit's first line ` +
                  `both changed; leaving the title alone`,
              );
          }
          let newBody = null;
          let seed = false;
          const claimTitle = pr.title === expected.title || newTitle !== null;
          const wrapNew = (text) => wrapManaged(text, claimTitle ? expected.title : null);
          if (managed === null) {
            if (!body.trim()) {
              if (expected.body) newBody = wrapNew(expected.body);
            } else if (body.trim() === expected.body) {
              newBody = wrapNew(expected.body);
              seed = newTitle === null;
            }
          } else {
            const bodyFp = storedFp(body, "body");
            if (managed === expected.body) {
              if (bodyFp === null) {
                newBody = wrapNew(expected.body);
                seed = newTitle === null;
              } else if (newTitle !== null) newBody = wrapNew(expected.body);
            } else if (bodyFp === null || (fnvFp(managed) !== bodyFp && fnvFp(expected.body) !== bodyFp)) {
              warnings.push(
                bodyFp === null
                  ? `#${pr.number} (${name}): the PR description was edited on GitHub ` +
                      `(or predates Jiji's tracking); leaving it alone`
                  : `#${pr.number} (${name}): the PR description and the commit ` +
                      `description both changed since Jiji last wrote it; leaving ` +
                      `the PR text alone`,
              );
            } else if (fnvFp(managed) === bodyFp) newBody = wrapNew(expected.body);
            // fp matches the commit: the user edited the PR — leave it.
          }
          if (newBody !== null)
            textActions.push({
              kind: "updatePrText",
              number: pr.number,
              bookmark: name,
              title: newTitle,
              body: newBody,
              seed,
            });
        }
      } else {
        prActions.push({
          kind: "createPr",
          bookmark: name,
          base,
          title,
          body: wrapManaged(expected.body, title),
        });
      }
      live.push({ bookmark: name, number: pr?.number ?? null, url: pr?.url ?? null });
      base = name;
    }
    actions.push(...prActions.filter((a) => a.kind === "createPr"));
    actions.push(...prActions.filter((a) => a.kind === "retargetPr"));
    actions.push(...textActions);
    // The stack-comment twin: pending creations sync every live PR; a
    // fully-known stack compares rendered bodies against the stub's
    // comment store so an executed flow replans to quiet.
    const creating = prActions.some((a) => a.kind === "createPr");
    const commentActions = [];
    for (const entry of live) {
      if (entry.number != null) {
        const existing = forge.comments.get(entry.number) ?? null;
        if (creating) {
          commentActions.push({
            kind: "syncStackComment",
            bookmark: entry.bookmark,
            number: entry.number,
            create: existing === null,
          });
        } else {
          const rendered = renderStackComment(live, entry.bookmark);
          if (existing !== null && existing !== rendered)
            commentActions.push({
              kind: "syncStackComment",
              bookmark: entry.bookmark,
              number: entry.number,
              create: false,
            });
          else if (existing === null && live.length >= 2)
            commentActions.push({
              kind: "syncStackComment",
              bookmark: entry.bookmark,
              number: entry.number,
              create: true,
            });
        }
      } else if (live.length >= 2) {
        commentActions.push({
          kind: "syncStackComment",
          bookmark: entry.bookmark,
          number: null,
          create: true,
        });
      }
    }
    actions.push(...commentActions);
    return {
      headBookmark: head,
      remote: repo ? repo.remote : "origin",
      baseBranch: snap.trunkBookmark,
      segments,
      actions,
      blockers,
      warnings,
      stackCommentPreview: commentActions.length
        ? renderStackComment(live, null)
        : null,
      // The captured snapshots carry no PR template (the Rust mock's
      // trunk_text_file answers None), so the plan-card note never shows
      // here — a documented approximation.
      prTemplatePath: null,
    };
  };

  // A JS twin of jiji-forge's plan_land, just enough for shots: the same
  // segment walk as the submit twin, then bottom-up placement — fabricated
  // merged PRs (the `merged` &prs= flag) reconcile, the first open PR is
  // the landing candidate read from its fabricated review/CI flags, and
  // everything above stacks. Wording mirrors the Rust engine's.
  const landPlanOf = (snap, head) => {
    const repo = forgeRepoOf(snap);
    const prState = forgePrsOf(snap);
    const local = new Map(
      (snap.bookmarks || [])
        .filter((b) => b.isLocal && !b.isTrunk)
        .map((b) => [b.name, b]),
    );
    const state = local.get(head);
    if (!state) return null;
    const trunk = snap.trunkBookmark;
    const nodes = new Map(snap.nodes.map((n) => [n.id, n]));
    const chain = [];
    let cursor = nodes.get(state.target);
    while (cursor && cursor.kind !== "immutable") {
      chain.push(cursor);
      cursor = nodes.get(cursor.parents[0]);
    }
    chain.reverse();
    const rawSegments = [];
    let pending = [];
    for (const node of chain) {
      pending.push(node);
      const names = node.bookmarks
        .filter((name) => local.has(name))
        .sort(
          (a, b) =>
            (a !== head) - (b !== head) ||
            !(a in prState.byBranch) - !(b in prState.byBranch) ||
            a.localeCompare(b),
        );
      if (!names.length) continue;
      rawSegments.push({ bookmark: names[0], nodes: pending });
      pending = [];
    }
    const warnings = [];
    const blockers = [];
    const segments = [];
    let mergedTop = -1;
    let candidate = null;
    let noPr = null;
    let bottomResolved = true;
    rawSegments.forEach((raw, index) => {
      const changeIds = raw.nodes.map((n) => n.id);
      const pr = prState.byBranch[raw.bookmark] || null;
      const openPr = pr && pr.state === "open" ? pr : null;
      if (!bottomResolved) {
        segments.push({
          bookmark: raw.bookmark,
          changeIds,
          pr: openPr,
          status: { kind: "stacked" },
        });
        return;
      }
      if (pr && pr.state === "merged") {
        segments.push({
          bookmark: raw.bookmark,
          changeIds,
          pr: null,
          status: { kind: "merged", number: pr.number, url: pr.url },
        });
        mergedTop = index;
      } else if (openPr) {
        bottomResolved = false;
        if (mergedTop >= 0) {
          candidate = null; // reconcile-only run; this PR stays put
          segments.push({
            bookmark: raw.bookmark,
            changeIds,
            pr: openPr,
            status: { kind: "stacked" },
          });
        } else {
          candidate = { index, pr: openPr };
          segments.push({
            bookmark: raw.bookmark,
            changeIds,
            pr: openPr,
            status: { kind: "waiting" },
          });
        }
      } else {
        noPr = raw.bookmark;
        bottomResolved = false;
        segments.push({
          bookmark: raw.bookmark,
          changeIds,
          pr: null,
          status: { kind: "waiting" },
        });
      }
    });
    const actions = [];
    const subtreeOf = (id) =>
      1 +
      snap.nodes
        .filter((n) => n.parents.includes(id))
        .reduce((sum, child) => sum + subtreeOf(child.id), 0);
    const reconcile = (landedIdx, method) => {
      actions.push({ kind: "fetchRemote", remote: repo?.remote ?? "origin" });
      const top = Math.max(...landedIdx);
      const landedHead = rawSegments[top].nodes[rawSegments[top].nodes.length - 1];
      snap.nodes
        .filter((n) => n.parents.includes(landedHead.id))
        .forEach((child) => {
          actions.push({
            kind: "rebaseOntoTrunk",
            rootChange: child.id,
            moves: subtreeOf(child.id),
          });
        });
      const live = segments
        .filter(
          (segment, index) =>
            index > top &&
            snap.bookmarks.find((b) => b.name === segment.bookmark)?.remote,
        )
        .map((segment) => segment.bookmark);
      if (live.length) actions.push({ kind: "pushStack", bookmarks: live });
      const next = segments
        .filter((_, index) => index > top)
        .map((segment) => segment.pr)
        .find(Boolean);
      if (next && next.baseBranch !== trunk)
        actions.push({
          kind: "retargetPr",
          number: next.number,
          bookmark: next.headBranch,
          toBase: trunk,
        });
      for (const index of landedIdx)
        actions.push({
          kind: "cleanupBookmark",
          bookmark: rawSegments[index].bookmark,
        });
      if (method !== "merge")
        for (const index of landedIdx) {
          const raw = rawSegments[index];
          if (raw.nodes.some((n) => n.id === snap.workingCopy))
            warnings.push(
              `the working copy sits on \u{201c}${raw.bookmark}\u{201d}'s landed ` +
                `changes; it respawns as a fresh empty change when they are swept`,
            );
          actions.push({
            kind: "abandonLanded",
            bookmark: raw.bookmark,
            changeIds: raw.nodes.map((n) => n.id).reverse(),
          });
        }
    };
    if (mergedTop >= 0) {
      if (noPr) {
        warnings.push(
          `\u{201c}${noPr}\u{201d} has no pull request yet — publish the stack ` +
            `once this reconcile lands`,
        );
        const waiting = segments.find((segment) => segment.bookmark === noPr);
        if (waiting) waiting.status = { kind: "stacked" };
      }
      reconcile(
        segments.flatMap((segment, index) =>
          segment.status.kind === "merged" ? [index] : [],
        ),
        null,
      );
    } else if (noPr) {
      blockers.push({
        message: `\u{201c}${noPr}\u{201d} has no pull request — publish the stack first`,
        wait: false,
      });
    } else if (candidate) {
      const { index, pr } = candidate;
      const bm = local.get(rawSegments[index].bookmark);
      if (bm.sync !== "synced")
        blockers.push({
          message:
            `\u{201c}${bm.name}\u{201d} and its GitHub branch differ — publish the ` +
            `stack first, so what merges is what you see here`,
          wait: false,
        });
      const flags = new Set(
        ((params.get("prs") || "")
          .split(",")
          .map((entry) => entry.split(":"))
          .find(([branch]) => branch === pr.headBranch)?.[1] || "")
          .split(/[.+\s]+/)
          .filter(Boolean),
      );
      if (params.get("lamon")) {
        warnings.push(
          `auto-merge is already enabled on #${pr.number}; GitHub merges it ` +
            `once its requirements are met — run Land again afterwards to reconcile`,
        );
      } else {
        if (pr.isDraft)
          blockers.push({
            message: `#${pr.number} is still a draft — mark it ready for review on GitHub first`,
            wait: false,
          });
        if (pr.baseBranch !== trunk)
          blockers.push({
            message:
              `#${pr.number} still targets \u{201c}${pr.baseBranch}\u{201d}, not ` +
              `\u{201c}${trunk}\u{201d} — publish the stack to retarget it first`,
            wait: false,
          });
        if (flags.has("unmergeable"))
          blockers.push({
            message:
              `#${pr.number} has merge conflicts with \u{201c}${trunk}\u{201d} — ` +
              `rebase onto the fetched trunk, publish, and land again`,
            wait: false,
          });
        if (pr.review === "changesRequested")
          blockers.push({
            message: `changes were requested on #${pr.number}`,
            wait: false,
          });
        if (pr.checks === "failing")
          blockers.push({
            message: `#${pr.number}'s checks are failing`,
            wait: false,
          });
        if (!blockers.length) {
          const waits = [];
          if (pr.checks === "pending")
            waits.push(`#${pr.number}'s checks are still running`);
          if (pr.review === "reviewRequired")
            waits.push(`#${pr.number} still needs an approving review`);
          if (params.get("lqueue")) {
            actions.push({
              kind: "enqueuePr",
              number: pr.number,
              bookmark: bm.name,
            });
            warnings.push(
              `\u{201c}${trunk}\u{201d} is protected by a merge queue; GitHub ` +
                `lands #${pr.number} from here — run Land again afterwards to reconcile`,
            );
          } else if (waits.length) {
            if (params.get("lnoauto"))
              blockers.push(
                ...waits.map((wait) => ({
                  message:
                    `${wait} — this repository has auto-merge disabled, so ` +
                    `land again when it settles`,
                  wait: true,
                })),
              );
            else {
              actions.push({
                kind: "enableAutoMerge",
                number: pr.number,
                bookmark: bm.name,
                method: "squash",
              });
              warnings.push(
                `${waits.join("; ")} — auto-merge hands the wait to GitHub`,
              );
            }
          } else {
            actions.push({
              kind: "mergePr",
              number: pr.number,
              bookmark: bm.name,
              method: "squash",
              expectedHead: pr.headCommit,
            });
            reconcile([index], "squash");
          }
        }
      }
      segments[index].status = blockers.length
        ? { kind: "waiting" }
        : { kind: "landing" };
    }
    return {
      headBookmark: head,
      remote: repo?.remote ?? "origin",
      baseBranch: trunk,
      segments,
      actions,
      blockers,
      warnings,
    };
  };

  // Execute a land plan's actions against the live stub world — the
  // world change behind both the manual Land flow and the auto-land
  // job twin: the merged PR drops out of the open set, the fetch
  // fabricates the squash commit arriving on trunk, rebases reparent
  // live rows, and cleanup removes the landed bookmark and changes.
  const executeLandTwin = (snap, plan) => {
    let landedSeq = 0;
    return plan.actions.map((action) => {
            const done = (detail) => ({ action, status: "done", detail });
            switch (action.kind) {
              case "mergePr": {
                forge.landedBranches.add(action.bookmark);
                return done(
                  `Merged #${action.number} (${action.bookmark}) into ${plan.baseBranch}`,
                );
              }
              case "enableAutoMerge":
                return done(
                  `Auto-merge enabled on #${action.number}; GitHub merges it ` +
                    `once its requirements are met — run Land again afterwards ` +
                    `to reconcile`,
                );
              case "enqueuePr":
                return done(
                  `#${action.number} added to the merge queue — run Land ` +
                    `again once it lands`,
                );
              case "fetchRemote": {
                // The squash commit arrives on trunk: a fresh immutable
                // node on the old trunk target, the trunk bookmark (and
                // its chip) moving to it.
                const trunkBm = snap.bookmarks.find((b) => b.isTrunk);
                const landing = plan.segments.find(
                  (segment) =>
                    segment.status.kind === "landing" ||
                    segment.status.kind === "merged",
                );
                if (trunkBm) {
                  landedSeq += 1;
                  const old = requireNode(snap, trunkBm.target);
                  const id = `zl${landedSeq}andsq`;
                  const position = snap.nodes.findIndex(
                    (n) => n.id === trunkBm.target,
                  );
                  const number =
                    landing?.status.kind === "merged"
                      ? landing.status.number
                      : (landing?.pr?.number ?? 0);
                  snap.nodes.splice(Math.max(position, 0), 0, {
                    id,
                    changeId: id,
                    commitId: `e5${landedSeq}a4d`,
                    description: `${humanize(landing?.bookmark ?? "stack")} (#${number})`,
                    author: old?.author ?? "GitHub",
                    timestamp: old?.timestamp ?? "2026-07-01T12:00:00Z",
                    kind: "immutable",
                    parents: trunkBm.target ? [trunkBm.target] : [],
                    elidedParents: [],
                    bookmarks: [trunkBm.name],
                    isEmpty: false,
                    hasConflict: false,
                    isDivergent: false,
                  });
                  if (old)
                    old.bookmarks = old.bookmarks.filter(
                      (name) => name !== trunkBm.name,
                    );
                  trunkBm.target = id;
                }
                return done(
                  `Fetched from ${action.remote} — ${trunkBm?.name ?? "trunk"} ` +
                    `moved to the merged commit`,
                );
              }
              case "rebaseOntoTrunk": {
                const trunkBm = snap.bookmarks.find((b) => b.isTrunk);
                const root = requireNode(snap, action.rootChange);
                if (root && trunkBm) root.parents = [trunkBm.target];
                return done(`Rebased ${action.rootChange} onto the new trunk`);
              }
              case "pushStack": {
                action.bookmarks.forEach((name) => {
                  const bm = snap.bookmarks.find((b) => b.name === name);
                  if (bm) {
                    bm.sync = "synced";
                    bm.remote = plan.remote;
                  }
                });
                return done(
                  `Pushed ${action.bookmarks.join(", ")} to ${plan.remote}`,
                );
              }
              case "retargetPr": {
                forge.prOverrides.set(action.number, {
                  ...(forge.prOverrides.get(action.number) || {}),
                  baseBranch: action.toBase,
                });
                return done(`Retargeted #${action.number} onto ${action.toBase}`);
              }
              case "cleanupBookmark": {
                snap.bookmarks = snap.bookmarks.filter(
                  (b) => b.name !== action.bookmark,
                );
                snap.nodes.forEach((n) => {
                  n.bookmarks = n.bookmarks.filter(
                    (name) => name !== action.bookmark,
                  );
                });
                forge.landedBranches.add(action.bookmark);
                return done(
                  `Deleted \u{201c}${action.bookmark}\u{201d} here and on ${plan.remote}`,
                );
              }
              case "abandonLanded": {
                action.changeIds.forEach((id) => {
                  const node = requireNode(snap, id);
                  if (node) removeNode(snap, id, node.parents);
                });
                return done(`Abandoned ${action.changeIds.join(", ")}`);
              }
              default:
                return done("done");
            }
          });
  };


  window.__TAURI_INTERNALS__ = {
    transformCallback: () => 0,
    invoke: (cmd, args) => {
      const snap = data.snapshot;
      if (
        snap &&
        (cmd.endsWith("_change") || cmd.endsWith("_bookmark") || cmd === "squash_into")
      ) {
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
          // A clone, like the real IPC boundary: every snapshot the app
          // receives is a fresh value. Handing out the live stub object
          // lets later stubbed mutations mutate it under Svelte's keyed
          // each mid-flush (caught as each_key_duplicate when the upstream
          // check's fetch op landed while a section switch rendered).
          return snap
            ? Promise.resolve(structuredClone(snap))
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
        case "squash_into": {
          const node = target(args?.changeId);
          if (node instanceof Promise) return node;
          const dest = target(args?.destinationId);
          if (dest instanceof Promise) return dest;
          if (dest.id === node.id)
            return reject(
              "mutation_failed",
              `cannot move changes from ${node.id} into itself`,
            );
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
          const full = whole.length === all.length;
          // Partition the source diff and hand the carved files to the
          // destination's — same hunk matching as split_change.
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
            const destDiff = (data.diffs || {})[dest.id];
            const carvedPaths = carved.map((f) => f.path);
            data.diffs[dest.id] = {
              id: dest.id,
              from: null,
              files: [
                ...(destDiff?.files || []).filter(
                  (f) => !carvedPaths.includes(f.path),
                ),
                ...carved,
              ],
              truncated: false,
            };
            data.diffs[node.id] = { ...diff, files: rest };
          }
          dest.isEmpty = false;
          if (full) {
            // The emptied source is abandoned: its description folds into
            // the destination's, bookmarks land on its parent, an emptied
            // working copy respawns — the real backend's full squash.
            const destText = dest.description.trim();
            const srcText = node.description.trim();
            dest.description =
              destText === ""
                ? srcText
                : srcText === ""
                  ? destText
                  : `${destText}\n\n${srcText}`;
            const parentId = node.parents[0] ?? null;
            const effects = snap.bookmarks
              .filter((b) => b.target === node.id)
              .map((b) => ({ kind: "bookmark", label: `${b.name} moved` }));
            snap.bookmarks.forEach((b) => {
              if (b.target === node.id && parentId) b.target = parentId;
            });
            if (parentId) {
              const parent = snap.nodes.find((n) => n.id === parentId);
              if (parent) parent.bookmarks.push(...node.bookmarks);
            }
            removeNode(snap, node.id, node.parents);
            if (snap.workingCopy === node.id && parentId) {
              spawnWorkingCopy(snap, parentId);
              effects.push(wcMoved);
            }
            const opId = pushOp(
              snap,
              `squash commits into ${dest.commitId}`,
              effects,
            );
            return Promise.resolve({
              operationId: opId,
              summary: `Moved everything in ${node.id} into ${dest.id}; the emptied change was abandoned`,
              targetChange: dest.id,
            });
          }
          const moved =
            partialSelected.length === 0
              ? `${whole.length} file${whole.length === 1 ? "" : "s"}`
              : whole.length === 0
                ? `parts of ${partialSelected.length} file${
                    partialSelected.length === 1 ? "" : "s"
                  }`
                : `${whole.length} file${
                    whole.length === 1 ? "" : "s"
                  } and parts of ${partialSelected.length} more`;
          const opId = pushOp(snap, `squash commits into ${dest.commitId}`, []);
          return Promise.resolve({
            operationId: opId,
            summary: `Moved ${moved} from ${node.id} into ${dest.id}`,
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
        case "git_fetch": {
          // The upstream check (runs once on open, then on the cadence and
          // the chip). Default answers the fetched-nothing no-op; `fetch=`
          // drives the chip's other states: `fail` rejects like an
          // unreachable remote, `wait` never answers (the checking state),
          // `moved` records a fetch operation like a remote that moved.
          const mode = params.get("fetch");
          if (mode === "wait") return new Promise(() => {});
          if (mode === "fail")
            return reject(
              "mutation_failed",
              "could not fetch from origin: connection timed out",
            );
          const remote = (snap?.gitRemotes || [])[0]?.name || "origin";
          if (mode === "moved") {
            const opId = pushOp(
              snap,
              `fetch from git remote(s) ${remote}`,
              [{ kind: "remoteBookmark", label: `${snap.trunkBookmark}@${remote} updated` }],
            );
            return Promise.resolve({
              operationId: opId,
              summary: `Fetched 1 bookmark update from ${remote}`,
              targetChange: null,
            });
          }
          return Promise.resolve({
            operationId: null,
            summary: `Nothing new on ${remote}`,
            targetChange: null,
          });
        }
        // Fetch a PR's head for review: like the Rust mock, the fetched
        // head arrives as one fresh change directly on trunk wearing the
        // new bookmark (a documented approximation — the real backend
        // fetches whatever ancestry the PR carries).
        case "fetch_pr": {
          const name = (args?.bookmark ?? "").trim();
          if (!name)
            return reject("mutation_failed", "bookmark name cannot be empty");
          if (snap.bookmarks.some((b) => b.isLocal && b.name === name))
            return reject(
              "mutation_failed",
              `bookmark “${name}” already exists`,
            );
          const number = args?.number;
          const trunk = snap.bookmarks.find((b) => b.isTrunk);
          const id = `pv${String(mutationIndex).padStart(2, "0")}rwqk`;
          spawnedIds.add(id);
          snap.nodes.unshift({
            id,
            changeId: id,
            commitId: `9c${String(mutationIndex).padStart(2, "0")}7de2`,
            description: `PR #${number} head (fetched for review)`,
            author: "them",
            timestamp: "2026-06-10T13:01:00Z",
            kind: "mutable",
            parents: [trunk?.target].filter(Boolean),
            elidedParents: [],
            bookmarks: [name],
            isEmpty: false,
            hasConflict: false,
            isDivergent: false,
          });
          snap.bookmarks.push({
            name,
            target: id,
            remote: null,
            sync: "localOnly",
            isTrunk: false,
            isLocal: true,
          });
          snap.workstreams.push({
            id: `ws-${id}`,
            title: `PR #${number} head (fetched for review)`,
            nodeIds: [id],
            bookmark: name,
            isActive: false,
            behindTrunk: 0,
          });
          const opId = pushOp(
            snap,
            `fetch pull request #${number} into bookmark ${name}`,
            [{ kind: "bookmark", label: `${name} created` }],
          );
          return Promise.resolve({
            operationId: opId,
            summary: `Fetched PR #${number} into ${name}`,
            targetChange: id,
          });
        }
        // The forge connection (Publish section): repo detection mirrors
        // the backend's GitHub-URL parse over the captured gitRemotes;
        // auth state lives in `forge`, preset via &forge= and mutated by
        // the stubbed login/logout like the real keychain.
        case "forge_status":
        case "forge_verify": {
          if (cmd === "forge_verify" && params.get("fwait"))
            return new Promise(() => {});
          // With fwait, status answers no login so the UI proceeds into
          // the hanging verify; otherwise the login arrives pre-verified.
          const login =
            cmd === "forge_status" && params.get("fwait") ? null : forge.login;
          return Promise.resolve(forgeStatusOf(snap, login));
        }
        case "forge_login": {
          const token = (args?.token || "").trim();
          if (!token) return reject("auth_failed", "no token was entered");
          if (token === "bad")
            return reject(
              "auth_failed",
              "GitHub rejected the token: Bad credentials",
            );
          forge.source = "keychain";
          return Promise.resolve(forgeStatusOf(snap, forge.login));
        }
        case "forge_logout": {
          const after = params.get("fafter");
          forge.source =
            after === "env" ? "environment" : after === "gh" ? "ghCli" : null;
          return Promise.resolve(forgeStatusOf(snap, forge.login));
        }
        // Open-PR state for workbench badges, fabricated from &prs=. The
        // app only asks once the connection is verified, so scenarios pair
        // this with &forge=; the error answers mirror the backend's.
        case "forge_prs": {
          if (params.get("prswait")) return new Promise(() => {});
          if (!forgeRepoOf(snap))
            return reject(
              "no_github_remote",
              "no GitHub remote detected on this repository",
            );
          if (!forge.source)
            return reject("no_token", "no GitHub token is available");
          return Promise.resolve(forgePrsOf(snap));
        }
        // The review flow's by-number lookup: answered from the fabricated
        // open set; unknown numbers read like GitHub's 404.
        case "forge_pr": {
          const pr = forgePrsOf(snap).report.prs.find(
            (candidate) => candidate.number === args?.number,
          );
          return pr
            ? Promise.resolve(pr)
            : reject("not_found", "Not found on GitHub: Not Found");
        }
        // Re-run failed CI: a failing fabricated PR answers one re-run
        // workflow; anything else answers the honest empty report (the
        // failing check lives outside Actions' reach).
        case "rerun_failed_ci": {
          const pr = forgePrsOf(snap).report.prs.find(
            (candidate) => candidate.number === args?.number,
          );
          if (!pr) return reject("not_found", "Not found on GitHub: Not Found");
          return Promise.resolve(
            pr.checks === "failing"
              ? { rerun: ["ci"], refused: [] }
              : { rerun: [], refused: [] },
          );
        }
        // The publish-stack workflow: the plan derives from the captured
        // snapshot + fabricated PR state via a JS twin of plan_submit;
        // executing marks pushed bookmarks synced in the live snapshot and
        // remembers created PRs so the refreshed count/badges follow.
        case "submit_plan": {
          if (params.get("planwait")) return new Promise(() => {});
          const plan = submitPlanOf(snap, args?.headBookmark);
          if (!plan)
            return reject(
              "plan_failed",
              `there is no local bookmark named \u{201c}${args?.headBookmark}\u{201d}`,
            );
          return Promise.resolve(plan);
        }
        case "submit_stack": {
          const plan = submitPlanOf(snap, args?.headBookmark);
          if (!plan) return reject("plan_failed", "the bookmark is gone");
          const pushed = plan.actions.filter((a) => a.kind === "push");
          const pushDetail = `Pushed ${
            pushed.length === 1 ? pushed[0].bookmark : `${pushed.length} bookmarks`
          } to ${plan.remote}`;
          for (const action of pushed) {
            const bm = snap.bookmarks.find((b) => b.name === action.bookmark);
            if (bm) {
              bm.sync = "synced";
              bm.remote = plan.remote;
            }
          }
          const repo = forgeRepoOf(snap);
          const createdByBookmark = new Map();
          const steps = plan.actions.map((action) => {
            if (action.kind === "push")
              return { action, status: "done", detail: pushDetail, pr: null };
            if (action.kind === "createPr") {
              const number = 201 + forge.createdPrs.length;
              const pr = {
                number,
                title: action.title,
                url: `https://github.com/${repo ? `${repo.owner}/${repo.name}` : "o/r"}/pull/${number}`,
                state: "open",
                isDraft: false,
                headBranch: action.bookmark,
                headCommit: "ad".repeat(20),
                headOwner: repo?.owner ?? null,
                baseBranch: action.base,
                body: action.body,
                review: "none",
                checks: "none",
              };
              forge.createdPrs.push(pr);
              createdByBookmark.set(action.bookmark, pr);
              return {
                action,
                status: "done",
                detail: `Opened #${number} for ${action.bookmark}`,
                pr,
              };
            }
            if (action.kind === "updatePrText") {
              forge.prOverrides.set(action.number, {
                body: action.body,
                ...(action.title != null ? { title: action.title } : {}),
              });
              const detail = action.seed
                ? `Recorded Jiji's description fingerprints on #${action.number}`
                : action.title != null
                  ? `Updated #${action.number}'s title and description from ${action.bookmark}`
                  : `Updated #${action.number}'s description from ${action.bookmark}`;
              return { action, status: "done", detail, pr: null };
            }
            if (action.kind === "syncStackComment") {
              const number =
                action.number ?? createdByBookmark.get(action.bookmark)?.number;
              const entries = plan.segments
                .map((segment) => {
                  const pr =
                    segment.pr ?? createdByBookmark.get(segment.bookmark) ?? null;
                  return pr
                    ? { bookmark: segment.bookmark, number: pr.number, url: pr.url }
                    : null;
                })
                .filter(Boolean);
              const existed = forge.comments.has(number);
              forge.comments.set(
                number,
                renderStackComment(entries, action.bookmark),
              );
              return {
                action,
                status: "done",
                detail: existed
                  ? `Updated the stack comment on #${number}`
                  : `Posted the stack comment on #${number}`,
                pr: null,
              };
            }
            return {
              action,
              status: "done",
              detail: `Retargeted #${action.number} (${action.bookmark}) from ${action.fromBase} to ${action.toBase}`,
              pr: null,
            };
          });
          return Promise.resolve({ steps, failed: false });
        }
        // The land-stack workflow: the plan derives via a JS twin of
        // plan_land; executing applies the world change to the live stub —
        // the merged PR drops out of the open set, the fetch fabricates
        // the squash commit arriving on trunk, rebases reparent live rows,
        // and cleanup removes the landed bookmark and changes — so the
        // refreshed graph, count, and badges follow the executed flow.
        case "land_plan": {
          if (params.get("planwait")) return new Promise(() => {});
          const plan = landPlanOf(snap, args?.headBookmark);
          if (!plan)
            return reject(
              "plan_failed",
              `there is no local bookmark named \u{201c}${args?.headBookmark}\u{201d}`,
            );
          return Promise.resolve(plan);
        }
        case "land_stack": {
          const plan = landPlanOf(snap, args?.headBookmark);
          if (!plan) return reject("plan_failed", "the bookmark is gone");
          return Promise.resolve({
            steps: executeLandTwin(snap, plan),
            failed: false,
          });
        }
        // The auto-land job twin: starting a job derives the same land
        // plan and mirrors the Rust engine's first poll — blockers wait
        // (attention when they need the user), an actionable plan runs one
        // round through the shared land executor, a hand-off or a still-
        // stacked segment keeps watching, a completed stack is done. The
        // stubbed event plumbing never fires, so the answered state is
        // what the shell renders.
        case "autoland_start": {
          const head = args?.headBookmark;
          const plan = landPlanOf(snap, head);
          if (!plan)
            return reject(
              "plan_failed",
              `there is no local bookmark named \u{201c}${head}\u{201d}`,
            );
          const job = {
            headBookmark: head,
            phase: { kind: "waiting", attention: false, reasons: [] },
            rounds: 0,
            merged: plan.segments.flatMap((segment) =>
              segment.status.kind === "merged"
                ? [
                    {
                      number: segment.status.number,
                      url: segment.status.url,
                      bookmark: segment.bookmark,
                    },
                  ]
                : [],
            ),
            segments: plan.segments,
            lastOutcome: null,
          };
          if (plan.blockers.length) {
            job.phase = {
              kind: "waiting",
              attention: plan.blockers.some((b) => !b.wait),
              reasons: plan.blockers.map((b) => b.message),
            };
          } else if (!plan.actions.length) {
            job.phase = {
              kind: "waiting",
              attention: false,
              reasons: plan.warnings.length
                ? plan.warnings
                : [
                    "Nothing to run yet — watching for the stack's conditions to change",
                  ],
            };
          } else {
            const steps = executeLandTwin(snap, plan);
            job.rounds = 1;
            job.lastOutcome = { steps, failed: false };
            for (const step of steps)
              if (step.action.kind === "mergePr")
                job.merged.push({
                  number: step.action.number,
                  url:
                    plan.segments.find(
                      (segment) => segment.bookmark === step.action.bookmark,
                    )?.pr?.url ?? "",
                  bookmark: step.action.bookmark,
                });
            const handsOff = plan.actions.some(
              (action) =>
                action.kind === "enableAutoMerge" ||
                action.kind === "enqueuePr",
            );
            const complete =
              !handsOff &&
              plan.segments.every(
                (segment) =>
                  segment.status.kind === "merged" ||
                  segment.status.kind === "landing",
              );
            job.phase = complete
              ? { kind: "done" }
              : {
                  kind: "waiting",
                  attention: false,
                  reasons: [
                    handsOff
                      ? "GitHub is driving the merge now — watching for it to finish"
                      : "Landed a round — waiting for the rebased stack's checks before the next",
                  ],
                };
          }
          forge.autoland = job;
          return Promise.resolve(structuredClone(job));
        }
        case "autoland_stop": {
          if (forge.autoland) forge.autoland.phase = { kind: "stopped" };
          return Promise.resolve(
            forge.autoland ? structuredClone(forge.autoland) : null,
          );
        }
        case "autoland_state":
          return Promise.resolve(
            forge.autoland ? structuredClone(forge.autoland) : null,
          );
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
  // The upstream chip's manual fetch: click it and wait for the recorded
  // operation's breadcrumb (pair with fetch=moved — the default stub
  // answer fetches nothing, which records no operation).
  if (params.get("fetchnow")) {
    steps.push(() => click(".upstream-chip:not(:disabled)"));
    steps.push(() => document.querySelector(".breadcrumb") !== null);
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
  // Forge connection flows (section=publish): type a token, connect,
  // disconnect. The connect wait lands on the connected card, or on the
  // inline error when the token was the refused "bad".
  const forgeToken = params.get("ftoken");
  if (forgeToken) {
    steps.push(() => {
      const input = document.querySelector("[data-forge-token]");
      if (!input || input.disabled) return false;
      input.value = forgeToken;
      input.dispatchEvent(new Event("input", { bubbles: true }));
      return true;
    });
  }
  if (params.get("fconnect")) {
    steps.push(() =>
      click('[data-forge-state="disconnected"] .btn.primary:not(:disabled)'),
    );
    steps.push(() =>
      forgeToken === "bad"
        ? document.querySelector("[data-forge-error]") !== null
        : document.querySelector('[data-forge-state="connected"]') !== null,
    );
  }
  if (params.get("fdisconnect")) {
    steps.push(() =>
      click('[data-forge-state="connected"] .btn.secondary:not(:disabled)'),
    );
    // Either the connect state returns (no fallback token) or the fallback
    // source's managed-outside note replaces the Disconnect button.
    steps.push(
      () =>
        document.querySelector('[data-forge-state="disconnected"]') !== null ||
        document.querySelector(".managed") !== null,
    );
  }
  // Publish-stack flows (section=publish + forge=): click a stack row to
  // derive its plan, then optionally publish and wait for the results.
  const submitStack = params.get("splan");
  if (submitStack) {
    steps.push(() => click(`[data-submit-stack="${submitStack}"]`));
    // With &planwait the answer never comes; settle on the skeleton.
    steps.push(() =>
      params.get("planwait")
        ? document.querySelector('[data-submit-state="planning"]') !== null
        : document.querySelector("[data-submit-plan]") !== null ||
          document.querySelector('[data-submit-state="up-to-date"]') !== null ||
          document.querySelector("[data-submit-error]") !== null,
    );
  }
  if (params.get("sprev")) {
    // Open the plan card's stack-comment preview disclosure.
    steps.push(() => {
      const preview = document.querySelector("[data-submit-comment-preview]");
      if (!preview) return false;
      preview.open = true;
      return true;
    });
  }
  if (params.get("sgo")) {
    steps.push(() => click("[data-submit-go] .btn.primary:not(:disabled)"));
    steps.push(() => document.querySelector("[data-submit-outcome]") !== null);
  }
  // Land-stack flows (section=publish + forge=): click a stack row in the
  // Land group to derive its plan, then optionally land and wait for the
  // executed per-step results.
  const landStackParam = params.get("lplan");
  if (landStackParam) {
    steps.push(() => click(`[data-land-stack="${landStackParam}"]`));
    // With &planwait the answer never comes; settle on the skeleton.
    steps.push(() =>
      params.get("planwait")
        ? document.querySelector('[data-land-state="planning"]') !== null
        : document.querySelector("[data-land-plan]") !== null ||
          document.querySelector("[data-land-error]") !== null,
    );
  }
  if (params.get("lgo")) {
    steps.push(() => click("[data-land-go] .btn.primary:not(:disabled)"));
    steps.push(() => document.querySelector("[data-land-outcome]") !== null);
  }
  // Auto-land flows: queue the derived plan (&lplan first) and wait for
  // the job card; &alstop clicks the running card's Stop.
  if (params.get("alqueue")) {
    steps.push(() =>
      click("[data-autoland-queue] .btn:not(:disabled)"),
    );
    steps.push(() => document.querySelector("[data-autoland-job]") !== null);
  }
  if (params.get("alstop")) {
    steps.push(() => click("[data-autoland-job] .btn:not(:disabled)"));
    steps.push(
      () =>
        document.querySelector('[data-autoland-job="stopped"]') !== null,
    );
  }
  // Review-helper flows (section=publish + forge= + prs=): open the
  // fetch-for-review panel from a PR row (&rfetch) or the by-number
  // lookup (&rlookup), optionally retype the bookmark name (&rname), and
  // confirm (&rgo). &rrun clicks a row's Re-run failed CI and waits for
  // the note; &rrunhdr clicks the workbench header's re-run chip after
  // &click selects the bookmarked change.
  const reviewFetch = params.get("rfetch");
  if (reviewFetch) {
    steps.push(() => click(`[data-review-fetch="${reviewFetch}"]`));
    steps.push(() => document.querySelector("[data-review-panel]") !== null);
  }
  const reviewLookup = params.get("rlookup");
  if (reviewLookup) {
    steps.push(() => {
      const input = document.querySelector("[data-review-lookup]");
      if (!input || input.disabled) return false;
      input.value = reviewLookup;
      input.dispatchEvent(new Event("input", { bubbles: true }));
      return true;
    });
    steps.push(() => click(".lookup-row .btn:not(:disabled)"));
    steps.push(
      () =>
        document.querySelector("[data-review-panel]") !== null ||
        document.querySelector("[data-review-lookup-error]") !== null,
    );
  }
  const reviewName = params.get("rname");
  if (reviewName) {
    steps.push(() => {
      const input = document.querySelector("[data-review-name]");
      if (!input || input.disabled) return false;
      input.value = reviewName;
      input.dispatchEvent(new Event("input", { bubbles: true }));
      return true;
    });
  }
  if (params.get("rgo")) {
    steps.push(() =>
      click(".review-name-row .btn.primary:not(:disabled)"),
    );
    steps.push(
      () =>
        document.querySelector("[data-review-done]") !== null ||
        document.querySelector("[data-review-error]") !== null,
    );
  }
  const reviewRerun = params.get("rrun");
  if (reviewRerun) {
    steps.push(() =>
      click(`[data-review-rerun="${reviewRerun}"]:not(:disabled)`),
    );
    steps.push(
      () => document.querySelector("[data-review-rerun-note]") !== null,
    );
  }
  if (params.get("rrunhdr")) {
    steps.push(() => click('[data-action="rerun-ci"]:not(:disabled)'));
    steps.push(() => {
      const chip = document.querySelector('[data-action="rerun-ci"]');
      return chip !== null && /re-run requested/i.test(chip.textContent);
    });
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
  const splitDest = params.get("sdest");
  if (splitDest) {
    steps.push(() => click(`.dest-row[data-splitdest="${splitDest}"]`));
  }
  const rebaseDest = params.get("dest");
  if (rebaseDest) {
    steps.push(() => click(`.dest-row[data-dest="${rebaseDest}"]`));
  }
  // Hover a destination row without picking it: the graph's rewrite
  // preview scrubs to it live (pointerenter drives the panel's hover
  // state; works for both the rebase and the split-into lists).
  const destHover = params.get("desthover");
  if (destHover) {
    steps.push(() => {
      const row =
        document.querySelector(`.dest-row[data-dest="${destHover}"]`) ||
        document.querySelector(`.dest-row[data-splitdest="${destHover}"]`);
      if (!row) return false;
      row.dispatchEvent(new PointerEvent("pointerenter"));
      return true;
    });
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
