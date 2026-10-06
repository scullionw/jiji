# Changelog

## 0.4.1 - 2026-10-06

A cleaner Publish section, in the same flat style as the workbench.

- A stack's header now matches a change's two bars: the title with a
  Workbench button, then the stack's state, its pull requests and
  changes, the branch it lands into, and whether it is your current
  stack.
- The landing order reads as one list: each row shows its PR number,
  title, branch, checks and review, with "Next to land" or "Merged"
  beside it, and the heading counts how many have merged.
- Land and Update are a segmented control at the top of the action
  column, beside a labelled More menu for shipping straight to trunk.
- Stack list rows lead with the title, then the status and PR count.
  Auto-land jobs are flat rows with a status dot that pulses while the
  job runs.
- Counts read correctly in the singular ("1 step", "1 change").

## 0.4.0 - 2026-09-28

Everything since 0.1.0 in one release: a new look built around the diff,
the `jiji` command line, stacked-PR publishing with landing and auto-land,
conflict resolution, evolution and annotate views, repository tabs, and
jj 0.45 underneath.

- A new look, built around the diff. The window is one full-bleed surface
  with hairline joins: repository tabs share a slim title bar with the
  traffic lights, sections move to a 44px icon rail (⌘B hides it), and the
  change header shrinks to two bars — title and actions, then ids, author,
  bookmarks and the diff controls — so code starts about 130px higher, and
  in a 1440px-wide window the code column is roughly 60% wider. Diffs run
  edge to edge with a pinned header per file and full-contrast context
  lines; graph rows are denser, with flat selection and quiet ref labels.
  The interface uses the platform's own fonts, popovers and menus are
  solid surfaces, glows, gradients and blur are gone, and section, view
  and palette switches are instant. The ten themes were redrawn on the
  same four surface steps, and the welcome screen is a plain start page
  with your recent repositories.
- Built on jj 0.45 (jj-lib 0.45.1, up from 0.39), so Jiji reads and
  writes repositories the way current `jj` does. Fetching follows jj
  0.44's tags: they arrive as `<name>@<remote>`, tracked by local tags,
  and `remotes.<name>.fetch-tags` narrows them. New remote bookmarks
  follow `remotes.<name>.auto-track-bookmarks` now that
  `git.auto-local-bookmark` is gone, and untracked remote tags count as
  immutable, like in the CLI.
- Open a plain Git repository and Jiji offers to set jj up in it, the way
  `jj git init --colocate` does: a `.jj` folder next to `.git`, your
  branches imported as bookmarks, and Git working alongside. Nothing
  changes unless you confirm; opening a folder inside the repository
  offers its root.
- Updates no longer restart Jiji on their own. A new version downloads in
  the background and the status bar offers "Restart to update"; Check for
  Updates… still asks first. A window left open looks again every few
  hours.
- Fixed hiding or unhiding right after another hide occasionally failing
  with "another hide/unhide command owns the archive lock".

- `jiji ready` and `jiji draft` change a stack's review readiness without
  `gh`: mark its draft PRs ready for review, or convert its open PRs back
  to drafts. Nothing is pushed and bases and descriptions are untouched.
  Name an intermediate bookmark to change only the PRs up to it. Both
  support `--dry-run` and `--json` like the other stack commands.
  `jiji publish --draft` opens new PRs as drafts, so reviewers and code
  owners are not asked until the stack is ready.
- History search (⌘⇧F) reopens on the last query, selected, so repeating
  a search is one keystroke and a new one is just typing.
- Tabs for repositories that share a name (`~/work/api` and `~/oss/api`,
  or workspaces in same-named folders) show the enclosing folder that
  tells them apart, in the tab strip and the palette's tab rows.
- Fixed the change header's open panels closing whenever the repository
  refreshed. Saving a file in your editor while describing, splitting, or
  rebasing the working copy used to shut the panel mid-edit (and drop the
  split selection); the header's More actions menu and a failed merge
  tool's error no longer vanish on a refresh either.
- Fixed Jiji rewriting trunk on its own. Opening a repository fetches,
  and while the fetch was still downloading, the repo watcher could
  import the half-updated refs as a second, concurrent operation: a
  merged PR's deleted branch made its commits look abandoned, and jj's
  reconcile of the two operations rebased every trunk commit built on
  them, leaving local `main` diverged from `main@origin`. Jiji's own
  writes to a repository (refreshes, fetches, pushes, every action) now
  take turns; reads still run alongside. If a repo shows "reconcile
  divergent operations" right after a fetch and `main` diverged, restore
  to the fetch's operation (`jj op restore <fetch op>`).
- Fixed the workbench staying blank ("Idle — open a repository") when a
  remote bookmark sits on a commit rewritten since, e.g. after amending a
  pushed branch and switching to the Team view. The hidden commit now
  goes by its commit id, like jj addresses it, instead of colliding with
  the change's visible commit; it also opens its own diff rather than the
  visible commit's. A view that fails to render now says so and offers
  Try again, instead of freezing.
- Refreshes on big repositories are several times faster: whether trunk
  contains each bookmark is answered in one pass instead of one history
  walk per bookmark, and emptiness of merge commits is remembered instead
  of re-merging parent trees every time. On a 105k-commit repo with 1,500
  remote branches a refresh went from about 870ms to 160ms.
- An unsaved description is no longer lost when the editor closes without
  saving (Esc, Cancel, selecting another change, switching tabs). Reopening
  Describe on that change restores the draft, with a Discard link; the
  pencil wears a dot while a draft waits. A draft is dropped once saved,
  and never offered over a description changed elsewhere.
- Move through long histories from the keyboard: after clicking into the
  graph, Page Up/Down moves the selection a screenful and Home/End jump
  to either end (elsewhere they still scroll the diff). `@` jumps back to
  the working copy from anywhere, also as "Go to working copy" in the
  command palette.
- Drop folders from Finder onto the window to open them as repository
  tabs, like dropping them on the Dock icon. A folder inside a repository
  opens that repository.
- Opening a folder inside a repository opens the repository, the way `jj`
  works from any subdirectory. Picking `src/` in the open dialog used to
  refuse with "Not a JJ repository".
- Pushing a bookmark or deleting one from a remote no longer freezes the
  window while the remote responds.
- Recent repositories can be removed from the welcome screen, so a moved
  or deleted repo no longer lingers in the list.
- Added ⌘⇧T to reopen closed repository tabs, newest first, also under
  File ▸ Reopen Closed Tab and in the command palette.
- Status-bar errors show their full text on hover, including jj's hint
  lines, and can be dismissed instead of holding the bar until the next
  refresh.
- Fixed switching tabs while a repository was still opening: the
  repository's saved graph lens could land on the other tab's repository
  and show its history under the wrong tab. A tab closed mid-open now
  also releases the repository it was loading.
- Added `jiji hide`, `unhide`, and `hidden` to put local changes or stacks
  aside and recover them by change ID. Hidden work is removed from ordinary
  jj/Git history, with protected object packs preserving recovery across Git
  garbage collection. Commands support dry runs and JSON output.
- Added the first-class `jiji` CLI: stack discovery/status, publish, one-round
  landing, supervised auto-land with saved resume, direct shipping, PR review,
  and shared GitHub authentication. Commands support repository selection,
  readable output, JSON, dry runs, and landing method/bypass options.
- Moved Publish stack discovery and status classification into shared Rust so
  the CLI and desktop recognize the same jjpr stacks. Existing app-only CLI
  launchers can be upgraded from the command palette; `jiji .` still opens Jiji.
- Rebuilt Publish around selectable stacks with linked PRs in landing
  order, checks and review status, and adjacent landing controls. Stacks
  created with jjpr appear automatically; local scratch work is collapsed
  and the PR browser has its own tab. Publishing reads complete stacks
  regardless of the workbench's graph filter.
- Fixed trunk detection to honor the repository's `trunk()` alias. Repos
  using `develop` no longer plan publishing or landing against `main`.
- Refined the workbench with clearer change titles, readable secondary
  text in every palette, a history search shortcut, and a compact menu for
  less-used change actions. Pane resizing preserves room for the diff and
  supports arrow keys, Home/End, and double-click to reset.
- Reduced initial JavaScript by about 59% by loading syntax languages and
  secondary sections on demand. Large history graphs render only nearby
  rows, and diff scrolling shares one viewport measurement per frame with
  binary searches over cached row offsets.
- Isolated diff caches by repository, preserved proper LRU ordering, and
  prevented stale diff content from supplying file actions during selection
  changes or refreshes. Failed loads now show an error and a Retry action.
- The graph stays readable however wide history gets. Three changes to
  how the workbench lays out the tree: a rail now joins the rail already
  waiting for the same parent as soon as it can (jj's own `├─╯`), so
  forty stacks rebased onto trunk draw two columns instead of forty and
  teammates' branches on one base converge the same way, while trunk's
  spine never bends into another line; history is walked one branch at a
  time down to its base before the next head (git's topo-order, jj's
  grouped log) instead of interleaving parallel lines by date; and the
  gutter has a lane budget measured off the pane — the leftmost lanes
  keep full spacing only as far as every row's text keeps its room, the
  lanes past that compress into a narrow band of hairlines with small
  markers, and anything beyond the band shares its last slot. A row's
  text no longer gives way to the tree: a deep `All history` lens with
  twenty open branches, which used to push every subject off the pane
  behind a horizontal scrollbar, now shows the working copy and every
  commit's id and description at the same left edge. Widening the pane
  spends the room on lanes; narrowing it protects the text. Rewrites and
  pane drags morph the rails to their new positions as before.
- Evolution grew into a full surface: a change's version history now
  takes over the diff pane as a scrubbable timeline instead of a cramped
  header dropdown. Every version the change was rewritten through sits
  on the strip oldest to newest, each step wearing its rewrite's weight
  (+/− lines, dashed when only the description or position moved); the
  selected step's diff renders through the real diff pipeline — syntax
  highlighting, unified or split layout, virtualized cards. Built for
  the `jj edit` workflow: the pane opens on the newest step, so "what
  did I just change" is one click (or the palette's "Show evolution")
  away. ←/→ scrub through time, ⇧-click or ⇧←/⇧→ widen the span to read
  several rewrites as one diff, and description-only rewrites show the
  description's own before/after. Restore moved in too: any earlier
  version restores from the timeline's caption, confirm-first, undoable
  from the op log. Esc returns to the diff.
- Save a revset as a named view: the lens picker's revset input grew a
  "Save as view…" affordance — name what you typed and it becomes a
  preset row in the picker's new Saved group, per repo, surviving
  restarts. A saved view applies with one click like the built-in
  lenses (the lens trigger wears the view's name while it is active),
  and hover offers rename and delete. A saved revset that stops
  resolving (a deleted bookmark, a typo) refuses inline with jj's own
  diagnostic, exactly like a typed one, leaving the graph as it was.
- Git tags show in the graph: a commit a colocated repo tags (a release,
  an rc) now wears a quiet tag chip beside its bookmarks, on whatever row
  the tag's commit is drawn. Read-only markers — Jiji offers no tag
  management, and jj itself treats tagged commits as immutable.
- Annotate any file — `jj file annotate` made visual: right-click a file
  in the diff pane or its file tree and pick "Annotate File" to see, for
  every line of the file as that change's tree carries it, the change
  that introduced the line (its id, author, summary, and age, grouped
  into runs with the whole file's text alongside). Clicking a line's
  origin jumps the graph to that change — revealing it under a transient
  revset when the current lens does not draw it — and hovering any line
  tints everything else that came from the same change. Read-only;
  esc returns to the diff. Binary, oversized, and conflicted files
  refuse with a plain answer instead of pretending markers have blame.
- Resolve divergence without the CLI: the change header's divergence
  callout now offers, per copy, Compare — the two copies' trees against
  each other in the diff pane, through the ordinary pinned comparison —
  and Abandon, with a confirm stating what goes (bookmarks on the copy,
  descendants rebasing) and that the change stops being divergent with
  one copy left. After abandoning, the selection follows the copy that
  was kept. Undoable from the op log like any mutation.
- See how a change evolved: the change header's new Evolution button (and
  the palette's "Show evolution") opens the change's version history —
  `jj evolog` made visual. Every version the change was rewritten through
  lists newest first, each wearing the operation that recorded it and its
  time; a "diff" disclosure on each transition unfolds exactly what that
  rewrite changed, and description-only rewrites say so. Any earlier
  version can be restored: the change's files and description return to
  that version as one new operation (the change keeps its place in the
  graph — parents and author stay), descendants rebase, and the op log
  undoes it like any mutation. Versions from before the op log's horizon
  still list from commit metadata, just without their operation.
- Conflicts render in the diff pane: a conflicted file no longer shows
  jj's raw marker text diffed against the parent — it renders its merge
  structure, read-only. Resolved runs read as context (long ones elide
  like unchanged lines), and each conflict region opens with a
  "Conflict N of M" band followed by every side as its own diff from the
  base, labeled "Side #1 · ours" / "Side #2 · theirs" to match the take
  verbs (plus jj's conflict labels — "rebase destination", a commit's
  description — when the tree carries them). Conflicted files a change
  merely inherits (the parent-relative diff never mentions them) now
  appear in its diff and file list too, so opening a conflicted change
  always shows what is unresolved. Conflicts with more than two sides
  render every side; binary and oversized conflicts keep the old
  marker-text rendering.
- Resolve conflicts without leaving the app: every conflicted file's row
  in the Conflicts view grew "Take ours" / "Take theirs" buttons that keep
  one side of the conflict wholesale (the CLI's built-in `:ours`/`:theirs`
  merge tools — ours is side #1, usually the rebase destination's version;
  theirs side #2, usually the change's own). Instant, no merge-tool window,
  and available even when no external merge tool is configured; the op log
  undoes it like any mutation. Conflicts with more than two sides refuse
  with a pointer to the CLI, matching `jj resolve`.
- Combined diff of a stack selection: ⇧-click a second change in the same
  workstream and the selection extends to the contiguous run between the
  two, every row highlighted, with the diff pane showing the run as one
  combined diff — everything the selected changes add together, measured
  from the run's root parent. The header states the range
  ("3 changes as one diff · root → head · vs base"), the range's base row
  wears the graph's "diff base" chip, and Esc (or the header's ×)
  collapses back to the anchored single change before clearing the
  selection. First slice is contiguous-stack-only; ⇧-clicking outside the
  anchor's lane falls back to a plain select.
- Split single lines: the split panel's hunks now open one level further,
  to a checkbox per changed line. Checking a subset of a hunk's lines
  carves just those line changes into the new (or destination) change,
  with the file and hunk checkboxes showing an indeterminate "–" while
  partially picked. The picked line indices ride the same
  freshness verification as hunk coordinates — a change that moved
  underneath the panel refuses instead of rewriting unseen content.
  Alongside it, "Discard This Line" in the diff context menu reverts just
  the changed line under the cursor.
- Discard a single hunk: right-click a hunk in a mutable change's diff
  (the working copy included) and "Discard This Hunk" reverts just that
  hunk, leaving the file's other hunks in place. The hunk's rendered
  coordinates are re-verified against a fresh diff before anything is
  rewritten — if the change moved underneath the menu, the discard
  refuses with "refresh and try again" instead of touching content you
  never saw. The op log undoes it like any mutation.
- Revert a change (`jj revert`): "Revert Change" in a change's context
  menu and the command palette creates a new change on top of the working
  copy that backs out the target's diff, described like the CLI
  (`Revert "…"` / `This reverts commit …`). Works on immutable changes —
  backing out landed work is the point — and the op log undoes it.
- Fixed: undoing an operation that only added a commit (duplicate, and
  now revert) from the op log answered "already undone" and recorded
  nothing; the added change now disappears as expected.
- File tree: the diff pane grew a directory tree of the changed files,
  docked left of the diff. Single-child directories compact into one row
  (`src/lib/components`), collapsing a directory folds its subtree into a
  file count and aggregate +/− stats, and the row for the file under the
  reading line stays highlighted as the diff scrolls. Click a file to
  jump to it, drag the divider to resize, and toggle the tree from the
  diff header or the command palette; it steps aside on narrow panes and
  single-file diffs.
- Tabs: open several repositories at once, one tab each. ⌘T opens a tab,
  ⌘W closes it, ⌃Tab / ⌘⇧] / ⌘⇧[ cycle, and the command palette offers
  switch rows for the other open tabs. Opening a repo that is already
  open (from the `jiji` CLI, recents, or the picker) switches to its
  existing tab instead of duplicating it; tabs are restored on the next
  launch. Background tabs stay live — each open repo keeps its own
  watcher — and a running auto-land job now survives switching tabs
  (closing its tab interrupts it resumably, like before).

## 0.1.0 - 2026-06-15

Initial alpha release for macOS.

Jiji is a local desktop workbench for Jujutsu repositories. This first release focuses on core repo navigation and history-editing workflows:

- Open a local `jj` repository and inspect its working graph.
- Review change diffs.
- Describe, create, edit, squash, abandon, move, and rebase changes.
- Create, move, rename, and delete bookmarks.
- Use the operation log to restore or revert mistakes.
- Receive future app updates through Tauri's signed updater.

Still intentionally out of scope for this alpha: conflict resolution, publishing/review flows, multi-workspace management, and forge automation.
