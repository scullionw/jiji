# Tauri parity tracker

Everything the GPUI app still owes the Tauri app it replaced, in one list,
with a status per item. It merges two audits of the GPUI build at
`d74874059c18`: the first is written up in full in
[tauri-parity.md](tauri-parity.md) (items 1–28 keep its numbers), the
second added items 29–34, T4 and E7.

## Progress

| Group | Done | Total |
| --- | --- | --- |
| Release blockers (1–6) | 0 | 6 |
| Other gaps (7–34) | 0 | 28 |
| Decisions (D1–D4) | 0 | 4 |
| Tooling and tests (T1–T4) | 0 | 4 |
| Sign-off checks (E1–E7) | 0 | 7 |

An item is done when its status is `verified` or `accepted`.

## Keeping this current

Change an item's status in the same change that does the work, and update
the Progress table with it.

| Status | Meaning |
| --- | --- |
| `open` | Nothing done yet. |
| `in progress` | Someone is on it; say who or which change. |
| `fixed` | The code landed. Add the change id. The "Done when" check has not been run. |
| `verified` | The "Done when" check was run and passed. Say how: a test name, or who checked by hand. |
| `accepted` | Decided not to restore. Put the reason in the row. |
| `undecided` | Waiting on a decision (the Decisions table, and T3). |
| `not run` | A sign-off check nobody has run yet. |

The Basis column says how sure the finding is:

| Basis | Meaning |
| --- | --- |
| `test` | Reproduced with a throwaway test during the first audit. |
| `code` | Confirmed by reading both the Tauri and the GPUI source. |
| `reported` | From the first audit; not checked a second time. |
| `inferred` | Deduced from the Tauri source; needs a look at the old app. |

Paths in the Where column are under `crates/jiji-desktop/src/` unless they
start elsewhere. "Old:" paths are in the last Tauri tree, under
`src/lib/components/` unless they start with `src`; read one with
`git show 5c8dfb9e8655:<path>`. The shipped baseline is `app-v0.4.1`
(`3d3b2f3ab085`).

## Release blockers

| # | Status | What is lost | Done when | Where | Basis |
| --- | --- | --- | --- | --- | --- |
| 1 | open | **A destructive action can act on stale content.** While another change loads, or after a refresh fails, the menu on the diff still on screen offers file, hunk and line actions from the old diff. Discard Changes to File was enabled with mismatched operation ids. | With the response held while switching from change A to B, and with a refresh failing after a new operation, the dimmed diff's menu offers no file, hunk, line or resolve action until repository, operation and comparison all match. A test pins both cases. | `views/workbench/mod.rs` `open_diff_menu`, `app/menu.rs`. Old: `diff/DiffView.test.ts` | test, code |
| 2 | open | **An upgrade resets the session and preferences.** Tauri kept them in the webview's local storage; GPUI reads `prefs.json` and imports nothing. Auto-fetch set to Off comes back as 15 minutes, so an upgraded app can fetch when a repository opens. | A populated 0.4.1 profile (two repositories, auto-fetch Off) upgrades with every value in the inventory below intact, before the first fetch and again after a restart. A partly filled GPUI profile is not overwritten. | `store.rs`, `app/forge.rs` (fetch default). Old: `src/lib/state/tabs.svelte.ts`, `src/lib/state/upstream.svelte.ts`, `graph/views.ts` | code |
| 3 | open | **The updater can install bytes it did not verify.** The download is verified, written to disk, and reopened by path at Restart with no second check. Tauri held the verified bytes in memory until install. | Changing the staged archive between download and Restart leaves the installed app intact and tells the user why. An untouched signed archive still installs. | `host/update.rs` `download`, `install_and_relaunch` | code |
| 4 | open | **Keyboard-only use of ordinary controls.** Buttons, segmented controls and checkboxes answer the pointer only. There is no Tab or Shift-Tab path through them, no focus ring, and no Space or Return activation. | With Full Keyboard Access on and the pointer parked: open Settings and change a setting, enter the file tree, follow an annotate origin, and confirm a split. Order in both directions, visible focus, activation, Escape and where focus returns are all right. | `ui/button.rs`. Old: `ui/Button.svelte`, `diff/FileTree.svelte`, `diff/AnnotatePane.svelte` | code |
| 5 | open | **Screen readers.** The old interface named its dialogs, tabs, trees and list options, exposed selected and expanded states, and announced search results and errors. The GPUI views set no accessibility roles, so nothing is exposed. | VoiceOver names both repository tabs and says which is selected, announces a directory expanding, reads search results, and speaks a failed action. Name, role and state are right at each step. | `shell/topbar.rs` (tabs), `views/search.rs`. Old: `shell/TabBar.svelte`, `search/SearchOverlay.svelte` | code |
| 6 | open | **Intel Macs on macOS 10.13–10.15.** The 0.4.1 bundle declares 10.13; the GPUI bundle declares 11.0 for everyone. Through the shared update feed, an Intel user on an older system can be offered an app the system refuses to launch. | The floor is decided and written down. Either the bundle and both slices run on the old floor, or a client below the new floor is never offered the update and keeps a working app. Checked on the oldest supported Intel system through the real feed. | `crates/jiji-desktop/resources/Info.plist`, `.github/workflows/release-macos.yml` (manifest lists both architectures) | code |

### What item 2 must carry over

| Choice | Tauri storage key |
| --- | --- |
| Open tabs, their order, the active one | `jiji.tabs` |
| Per-repository lens or custom revset | `jiji.workbench.lens:<repo>` |
| Graph or Focus, compact or classic rows | `jiji.workbench.view`, `jiji.workbench.density` |
| Rail hidden, graph hidden, graph width | `jiji.rail.hidden`, `jiji.graph.collapsed`, `jiji.pane.graph` |
| Split or unified diff, file tree shown, its width | `jiji.diff.layout`, `jiji.diff.tree`, `jiji.pane.filetree` |
| Theme mode and the chosen dark and light palettes | `jiji-theme` |
| Preferred editor, last editor, preferred terminal | `jiji.editor.preferred`, `jiji.editor.last`, `jiji.terminal.preferred` |
| Fetch interval (including Off), dismissed CLI offer | `jiji.upstream.interval`, `jiji.cli.offer-dismissed` |

Recent repositories, named saved views, the licence record and device id,
GitHub credentials in the Keychain, and auto-land records already carry
over: their files and keys are unchanged.

## Other gaps

### Saved data and licence

| # | Status | What is lost | Done when | Where | Basis |
| --- | --- | --- | --- | --- | --- |
| 7 | open | **Window restoration.** Tauri's `.window-state.json` is ignored on upgrade. Afterwards one rectangle and a fullscreen flag are saved, so the zoomed state and the rectangle from before zooming are lost. | An offset, zoomed window survives the upgrade and a relaunch, and unzooms to its original rectangle. The same holds after its monitor is disconnected. | `store.rs` `WindowGeometry`, `main.rs` (window creation) | code |
| 8 | open | **One bad entry discards the whole list.** Tauri kept the valid saved views and tabs beside a malformed one, trimmed names and revsets, and dropped blanks and duplicates. GPUI decodes each list all at once. | A list holding a valid item, a malformed one and another valid one loads both valid items in order. Duplicate and whitespace-only saved views are dropped as before. | `store.rs` `JsonFile::get`, `saved_views`, `tabs`. Old: `graph/views.ts`, `src/lib/state/tabs.svelte.ts` | test, code |
| 9 | open | **Re-entering a licence key during a network hiccup.** If validating the existing activation fails, GPUI goes on to request a new activation, which can use up a device slot or show a device-limit error. Tauri stopped. | With a stubbed licence service that fails validation of a known key once: no activation call is made, the device id and entitlement are unchanged, and a retry works. | `host/license.rs` `activate_license`. Old: `src/lib/license/state.svelte.ts` | code |
| 10 | open | **Registration when the licence file cannot be read.** Tauri fell back to a "was registered" flag. GPUI treats an unreadable or undecodable file as no licence, which locks the paid themes for a registered user. | A registered profile whose licence file is briefly unreadable keeps its themes, and returns to normal when the file can be read. | `store.rs` `JsonFile::load`, `app/prefs.rs` `LicenseUi::load`. Old: `src/lib/license/persist.ts` | code |

### File tree and diff

| # | Status | What is lost | Done when | Where | Basis |
| --- | --- | --- | --- | --- | --- |
| 11 | open | **Right-click on a file in the tree.** It used to offer the same actions as the diff card: open in editor, Open With, Finder, annotate, copy path and patch, move, discard, merge tool. Tree rows now take only the left button. | Right-clicking a modified, a removed, a conflicted and an immutable file in the tree offers the same actions, with the same availability and paths, as the fresh diff card. | `views/workbench/diffpane.rs` (tree rows), `logic/menu.rs`. Old: `diff/FileTree.svelte` | code |
| 12 | open | **The active tree row stays on screen.** Scrolling the diff moves the highlight but never scrolls the tree to it, and keyboard moves ask a scroll handle that cannot place a row. | On a tree taller than the window: scroll the diff to the last file, press End and Home in the tree, then collapse the active file's directory and repeat. The highlighted or focused row is visible each time. | `views/workbench/diffpane.rs` `tree_scroll` | reported |
| 13 | open | **File details in the tree and the jump list.** A rename's old path no longer shows on hover in either place, and the diff card loses its full-path hover when the header clips. Per-file counts moved into tooltips, and modified files lost their M (that one on purpose: see Design in the crate README). | Both names of a rename can be found in the tree and in the jump list, and a clipped card header shows its full path on hover. The inline counts and the M are each restored or marked `accepted` here. | `views/workbench/diffpane.rs`, `views/workbench/header/bars.rs` (jump list), `diff/view.rs` (file header) | code, in part |
| 14 | open | **Right-click in an Evolution diff.** Tauri offered the read-only actions there: copy a hunk or file as a patch, copy paths. The evolution pane's diff emits the event and nothing listens. | In an evolution span whose patch differs from today's, copying the historical hunk, file patch and path puts the right text on the clipboard, and no changing action is offered. | `views/workbench/evolution.rs`, `views/workbench/mod.rs` (subscriptions) | code |
| 19 | open | **ICO, AVIF and HEIC previews.** The backend sends all three; the webview decoded them. GPUI drops them before decoding and can call a small image "Too large to preview". | Small before and after fixtures of each format preview wherever the old build showed them. An unsupported format, a failed decode and the real size limit read differently. | `diff/model.rs` `image_side`, `crates/jiji-core/src/jj.rs` `image_mime` | test, code |
| 20 | open | **Animated and transparent images.** The webview animated GIF and WebP and showed transparency over a checkerboard. GPUI paints the first frame on a solid background. | A two-frame GIF animates and a partly transparent PNG shows its transparency, in each theme. | `diff/view.rs` (image painting) | code |
| 21 | open | **Syntax highlighting.** `.xsd` is no longer highlighted, and XML, SVG and plist use the HTML grammar. A method name in the old Rust macro fixture loses its function colour. The size cutoff counts bytes instead of UTF-16 units, so non-ASCII hunks stop highlighting sooner. | XML namespace, XSD and plist fixtures, the old `extern_protocol!` case, and a hunk of 75,000 CJK characters highlight as before, in unified and split layouts. | `diff/highlight.rs` `lang_for_path` and the cutoff. Old: `diff/highlight.ts`, `diff/highlight.test.ts` | test, code |
| 22 | open | **Find ignores some case pairs.** Searching `σ` no longer matches final `ς`, in filenames and lines. | With `ς` in a filename and in a hunk, searching `σ` finds both; counts, stepping and highlight positions are right. | `diff/view.rs` `find_in_doc`. Old: `diff/find.ts` | test |
| 27 | open | **Edit ▸ Copy and Select All on a diff.** ⌘C and ⌘A work, but the menu items send the text-field actions, which a focused diff does not handle. | With a diff focused, choosing Edit ▸ Copy with the mouse copies the selection and Edit ▸ Select All selects, in the main diff and the evolution diff. | `shell/mod.rs` `menus`, `diff/view.rs` (actions) | test, code |

### Editing and reading

| # | Status | What is lost | Done when | Where | Basis |
| --- | --- | --- | --- | --- | --- |
| 15 | open | **Resizing the graph pane from the keyboard.** The divider took focus, moved 16 px per arrow press and jumped to its limits with Home and End. Dragging and double-click reset remain. | The divider can be focused; both arrows and both endpoints work, and the width is still there after a relaunch. | `views/workbench/mod.rs` `drag_divider`, `divider_click`. Old: `shell/SplitPane.svelte` | code |
| 16 | open | **Selecting text outside the diff.** The change title, the change and commit ids, the description body and the squash preview could be selected and copied in part. They are static labels now. Copy id and copying from the diff still work. | Part of an immutable change's description, and part of the squash preview, can be selected and copied without opening an editor. The title and ids too. | `views/workbench/header/bars.rs`, `views/workbench/header/trays.rs`. Old: `diff/ChangeHeader.svelte` (`.selectable`) | code |
| 17 | open | **Resizing the description editors by hand.** The Describe and Split description fields had a vertical resize handle. The new fields grow to a fixed number of rows. | Both editors can be made taller and shorter by hand without changing the draft. | `ui/input.rs` `multiline`, `views/workbench/header/mod.rs` | code |
| 18 | open | **A horizontal scrollbar in Annotate.** Long lines can be panned by swipe or shift-wheel only; no thumb is drawn. | With an ordinary mouse, the last column of a long line can be reached by dragging a horizontal thumb, and its origin followed. | `views/workbench/annotate.rs` | code |
| 29 | open | **Spell checking in the description editor.** It was the one text field the old app did not switch spell checking off for. | First confirm in the 0.4.1 app that a misspelling is underlined there. If it is, the new editor underlines it too. | `ui/input.rs`. Old: `diff/ChangeHeader.svelte` (the describe `<textarea>`) | inferred |

### Robustness and speed

| # | Status | What is lost | Done when | Where | Basis |
| --- | --- | --- | --- | --- | --- |
| 23 | open | **Fast navigation and scrolling.** Tauri shared one backend request between a prefetch and a selection of the same diff, and a warm cache hit rendered at once. GPUI can read twice and rebuilds the document on a cache hit. The Tauri graph also cached row offsets and widened its overscan with fling speed. | Measure first; the size of any slowdown is unknown. A prefetch followed at once by selection makes one backend read, and warm revisits and fast graph flings are no slower than Tauri on the same repository and window size. | `app/diffs.rs` `fetch_diff`, `prefetch_diff`; `views/workbench/graph.rs`. Old: `diff/diffcache.ts`, `graph/GraphView.svelte` | reported |
| 24 | open | **Prerelease update ordering.** The version comparison drops the prerelease suffix, so `1.2.3-rc.1` never sees `rc.2` or the final `1.2.3`. It also accepts a fourth numeric part Tauri rejected. | Prerelease to prerelease, prerelease to final, final to an older prerelease, the same version with build metadata, and a malformed manifest all order as semantic versioning does. | `host/update.rs` `parse_version`, `is_newer` | test, code |
| 25 | open | **A log file.** Tauri wrote `~/Library/Logs/com.lauf.jiji/Jiji.log`. GPUI prints to stderr, which a Finder launch discards. There is no panic hook either, so a crash leaves nothing behind. | After a session launched from Finder with a controlled error, the record is in a Jiji log file. A panic is recorded too. | `main.rs` `Logger`. Old: `src-tauri/src/lib.rs` | code |
| 26 | open | **Recovering from a view that fails to render.** Tauri showed "This view hit an error" with the message and Try again, and the shell and other tabs kept working. In GPUI a panic in a view takes the app down. | A failure injected into one section leaves the tabs and the other sections usable, shows the error, and offers a retry. | `views/mod.rs` (section construction). Old: `shell/AppShell.svelte` | code |
| 28 | open | **Retry after Publish fails to read the stacks.** Retry refreshes pull requests, and the stacks are only re-read when the snapshot or the PR data changes. If neither does, the error stays on screen after the local problem is gone. | After one failed stack read with the PR data unchanged, Retry reads the stacks again, clears the error and shows the list without leaving the section. | `views/publish/mod.rs` `sync` and Retry, `app/forge.rs` | test |

### Look and native polish

| # | Status | What is lost | Done when | Where | Basis |
| --- | --- | --- | --- | --- | --- |
| 30 | open | **Motion.** Nothing animates. Gone are (a) the signs that something is running: the fetch spinner, the pulsing busy and auto-land dots, the loading shimmer; (b) graph rows and rails moving to their new places after a rewrite, and panels unfolding; (c) the crossfade between themes. The old app turned all of it off under Reduce Motion. The crate README used to list "no layout or panel transitions" as a known difference from Tauri. | Each of (a), (b) and (c) is restored or marked `accepted` here. Whatever is restored is instant under Reduce Motion. | `shell/topbar.rs` (fetch chip), `shell/statusbar.rs`, `views/workbench/graph.rs`, `views/workbench/header/`. Old: `src/lib/motion.ts`, `src/lib/state/theme.svelte.ts` | code |
| 31 | open | **Three tooltips.** "tracked" in Bookmarks said "jj tracks this remote branch"; "merged" said "Already ancestry of trunk — deleting is cleanup"; the tab close button said "Close tab (⌘W)". | Hovering each of the three shows its text. | `views/bookmarks.rs`, `shell/topbar.rs`. Old: `bookmarks/BookmarkRow.svelte`, `shell/TabBar.svelte` | code |
| 32 | open | **The standard About panel.** About Jiji shows an alert with the version instead of the system panel with the icon. | About Jiji opens the standard panel with the icon and version. | `shell/mod.rs` (the `About` action). Old: `src-tauri/src/menu.rs` | code |
| 33 | open | **SF Mono for code.** The webview's `ui-monospace` was always SF Mono. GPUI uses SF Mono only when it is installed as a font, and Menlo otherwise. | On a Mac without SF Mono in Font Book, code renders in SF Mono. | `main.rs` `mono_font` | code; needs such a Mac |
| 34 | open | **"Exit Full Screen".** The View menu item reads "Enter Full Screen" while in full screen. | In full screen the item reads "Exit Full Screen". | `shell/mod.rs` `menus` | code |

## Decisions

Changed on purpose during the redesign. Each needs a yes or no if the
release promises nothing regresses. Set the status to `accepted` with the
reason, or to `open` and move the row into the gaps.

| # | Status | What changed | Where |
| --- | --- | --- | --- |
| D1 | undecided | **Comparisons are read-only.** Tauri offered file changes and the merge tool inside a fresh pinned or range comparison. GPUI offers reading actions only there, so acting means going back to the parent diff. | `views/workbench/mod.rs` `open_diff_menu`, `views/workbench/diffpane.rs` |
| D2 | undecided | **Title bar and tabs.** The one-click history search button, the theme menu and the registered-licence button are gone from the bar: the command field opens the palette, history search is ⌘⇧F, and appearance and licence live in Settings. A tab's close button shows on hover instead of always on the active tab. | `shell/topbar.rs` |
| D3 | undecided | **Status bar.** The idle line skips working-copy snapshot operations and names the backend only for mock data. Tauri showed the newest operation of any kind and always named the backend. | `shell/statusbar.rs` |
| D4 | undecided | **Visual cues.** Graph node symbols, lane widths and colours, selection styling and "+N" for refs that do not fit. Header metadata clips in a narrow pane. Bookmark rows reserve room for their actions. The diff's horizontal thumb shows on hover. | `views/workbench/graph.rs`, `views/workbench/header/bars.rs`, `diff/view.rs` |

## Tooling and tests

| # | Status | What is lost | Done when |
| --- | --- | --- | --- |
| T1 | open | **Offline interface fixtures.** `scripts/visual-harness` replayed captured repositories and diffs, seeded pull requests and landing plans, and chosen error states. `dump_diff` printed a real change's refined diff. GPUI has headless screenshots, scripted input and a mock repository, but its tests see no forge and nothing replays captured GitHub data. | The outcomes the harness exercised have GPUI tests or scripted fixtures: stack selection, plan options, late responses, CI, auto-land. |
| T2 | open | **Local release preflight.** `scripts/release-preflight` checked the feed, repository visibility, the names of the signing secrets and a local signing identity before a tag was pushed. | A release mistake of those kinds is caught before the workflow runs, or the check is marked `accepted` as covered by the workflow. |
| T3 | undecided | **Source compatibility.** Removed with the web frontend: `bun run bindings` and the generated TypeScript types, the `ts-rs` derives, and `jiji_forge::reconcile::TextPlan::has_write()` (it had no caller). The Windows terminal fallback is gone, and `host/cli.rs` imports Unix file permissions unconditionally. Releases were macOS-only. | Marked `accepted`, or the pieces someone relies on are named and restored. |
| T4 | open | **Test coverage.** 409 frontend tests and 7 shell tests became 127 tests in the app crate. By area: graph, selection and drag 78 → 18; diff, find, split, tree and evolution 120 → 26; Publish 61 → 12. | The behaviours the old suites pinned are pinned again, starting with those three areas. The old test files are the checklist. |

## Sign-off checks

These need a real environment and have not been run. Status is `not run`
until someone records the result and the date.

| # | Status | What to exercise |
| --- | --- | --- |
| E1 | not run | **Installed upgrade and system floor.** Upgrade a copy of a real 0.4.1 profile to a signed GPUI version, and from that to the next. Automatic and manual checks, Later, restart, a failed download and relaunch, and an install that needs administrator approval. Apple Silicon, current Intel, and the Intel systems in item 6. |
| E2 | not run | **Desktop integration.** Cold and warm opens from Finder, the Dock, the CLI and a file drop; nested paths; a moved or missing repository; duplicate repository names; restoring several tabs. Native menus, dragging by the title bar, zoom, full screen, several displays, editors opening at a line, terminals, installing and removing the CLI. |
| E3 | not run | **Workbench and repository writes.** Graph and Focus, custom and saved revsets, range and pinned comparisons, unified and split diffs, drag to rebase and squash, drafts across tabs, split and discard by file, hunk and line, undo. Check the resulting jj tree and operation ids. Repeat during outside writes, watcher refreshes, tab switches, stale-workspace recovery and a cancelled merge tool. |
| E4 | not run | **Publish, review and auto-land.** Against a GitHub repository you control: sign-in, stack discovery and refresh, no token, network failure, PR lookup and fetch for review, re-running failed CI, publish, ship, land with its options and bypass, stale plans, stopping a queued job, resuming after an upgrade. Check local refs and the remote. Tauri waited for the PR refresh before replanning; GPUI starts both together, so try a changing remote and late responses. |
| E5 | not run | **Input, appearance and performance.** VoiceOver, Full Keyboard Access, input-method composition, the clipboard, several keyboard layouts. All ten palettes, system theme changes, the minimum window width, Retina scaling, a 1,000-node graph, a large file tree, long lines, image-heavy diffs, fast scrolling. Record frame times and screenshots against Tauri. |
| E6 | not run | **Registration continuity.** With test entitlements: an upgrade, an offline launch, replacing a key, an expired or revoked licence, an unreadable licence file. The device registration, cached entitlement and paid theme survive. |
| E7 | not run | **First run of the release workflow.** It has never run. Developer ID signing, notarization, the Intel half of the universal build, the DMG (now a plain image with no window layout), the updater archive's signature, and a 0.4.1 install updating through the real feed. |
