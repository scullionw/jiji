# jiji-desktop

Jiji's desktop app: `jiji-core`, `jiji-forge` and `jiji-cli` behind an
interface drawn by [GPUI](https://www.gpui.rs). It is one Rust program, so
views call the backend directly and paint from the snapshot it returns.

The binary is `jiji-desktop`. Run bare it is the app; with arguments it is
the `jiji` CLI (`jiji-desktop publish …`), which is how the `jiji` command
the app installs works. What the app remembers lives in
`~/Library/Application Support/com.lauf.jiji`: recents, saved views,
preferences, the licence and auto-land records. Its log, with any panic and
its backtrace, is `~/Library/Logs/com.lauf.jiji/Jiji.log`, kept to about a
megabyte with the part before it in `Jiji.old.log`.

## GPUI version

`gpui` here is [`gpui-pre`](https://crates.io/crates/gpui-pre), a weekly
crates.io snapshot of Zed's main branch: Zed has not published the `gpui`
crate itself since 0.2.2 (October 2025). It tracks Zed's compiler too, so
it wants a recent stable Rust. `Cargo.lock` holds the snapshot in use;
`cargo update -p gpui-pre -p gpui-pre-platform` moves both to the newest.
A snapshot may break API: fix what the compiler reports, then run the same
scripted tour on the build from before and the build from after and
compare the screenshots.

## Run

```sh
cargo run -p jiji-desktop                         # the app; debug builds count as registered
cargo run -p jiji-desktop --release
cargo build -p jiji-desktop --features headless   # a build scripted runs can drive unseen
cargo test -p jiji-desktop                        # logic, plus the model and views driven in memory
scripts/bundle.sh                                 # target/release/bundle/Jiji.app
just install                                      # the bundle, copied into /Applications
```

A release build running from its bundle updates itself from the release
feed (`src/host/update.rs`). Anything else — `cargo run`, a debug build —
has no bundle to replace and never asks.

Since arguments belong to the CLI, the app's own switches are environment
variables:

| Variable | |
|---|---|
| `JIJI_OPEN=<path>` | open this repository at launch; several, separated like `PATH`, arrive together the way folders opened from Finder do |
| `JIJI_DATA_DIR=<dir>` | keep preferences, recents, licence and the log here instead of the real data and log directories — set it for any test run |
| `JIJI_MOCK_BACKEND=1` | serve the mock repository (needs a directory containing `.jj`) |
| `JIJI_DEV_LICENSE=unregistered\|real` | debug builds only: exercise the unregistered state, or read the real licence |
| `JIJI_SCRIPT=<steps>` | drive the window: see `src/script.rs` |
| `JIJI_HEADLESS=1` | never show the window; a scripted run nobody sees (needs a `headless` build) |

## Scripted runs

`JIJI_SCRIPT` takes `;`-separated steps and hands them to the app's window
through GPUI's own event dispatch, in-process: nothing is synthesized at
the OS level, and the clipboard is only touched by a step that copies.

With `JIJI_HEADLESS=1` the window is never shown. It is the real app — the
real platform, executors, watchers and text — but there is no Dock icon,
nothing takes focus or sits over other work, and neither the pointer nor
the keyboard can reach it. Frames are delivered by the run itself and
screenshots are rendered offscreen (`src/headless.rs`); the app quits when
the steps run out. Runs do not see each other, so several can go at once.

```sh
cargo build -p jiji-desktop --features headless
JIJI_DATA_DIR=/tmp/jiji-test JIJI_HEADLESS=1 JIJI_OPEN=~/Dev/some-repo \
JIJI_SCRIPT="wait=1500;rclick=200,150;key=down;shot=/tmp/menu.png" \
  target/debug/jiji-desktop
```

`click`, `dclick`, `rclick`, `move`, `drag`, `press`/`hold`/`release` (a drag
in parts), `scroll`, `key`, `type`, `wait`, `shot`, `resize`, `quit`;
prefixes `shift-`, `alt-`, `cmd-`. Coordinates are window points;
screenshots are at the display's scale.

The `headless` feature turns on GPUI's `test-support`, which is where its
offscreen renderer lives, so it stays out of ordinary and shipping builds:
without it `JIJI_HEADLESS` and `shot` are refused, and a script drives the
visible window. What a headless run cannot show is what AppKit draws or
does itself: the traffic lights and the window's corners are not in a
screenshot, and the translation of real OS events, the input method,
native menus and sheets, and how frames reach a display are not exercised.

## Tests

`cargo test -p jiji-desktop` covers the pure logic and, through GPUI's test
context, the model and the views themselves: the real `AppModel` and
`Shell` in a window that exists only in memory, with tasks running when
the test lets them (`src/testing.rs`). That is where navigation belongs —
what survives a switch of section or tab, which tab an open lands in —
since those go wrong between views rather than inside a function.

These tests open real repositories, made for the test and without remotes,
or the mock backend's (`testing::install_mock`: conflicts, a merge tool,
every kind of diff, and a backend a test can make fail by hiding `.jj`).
Under test the host finds no forge token, so they read no credentials and
reach no network. Keep it that way: a test that needs a token-bearing path
should fake the forge, not lift the guard in `host::forge::resolved_token`.

## Layout

Under `src/`:

| | |
|---|---|
| `host/` | The backend the views call: repo sessions and their watchers, the forge workflows (publish, land, ship, auto-land), the CLI launcher, editor and terminal hand-offs, licensing, updates. Blocking; views run it on the background executor. |
| `app/` | The app model — one entity holding tabs, each tab's selection and breadcrumb, the forge connection, upstream checks, auto-land jobs, preferences, and the context menu. Views read it and call its methods. |
| `logic/` | What to draw and what each affordance means, with no UI types: graph layout, stack and conflict grouping, the split checklist, menu and palette contents, drag-and-drop policy, timelines. Unit-tested. |
| `diff/` | The diff surface: a flat row model, tree-sitter highlighting, and one custom-painted element that windows, selects, finds and scrolls. |
| `ui/` | Buttons, inputs (with IME), popovers and the layer that hangs them off a trigger, tooltips, scrollbars, icons, ref labels and tags, a page's bar and bands, and the text and list elements GPUI lacks: one line that ellipsizes, a paragraph that wraps, a group heading that pins. |
| `shell/` | The window frame: tabs, top bar, section rail, status bar, menus, key bindings. |
| `views/` | The sections — workbench (graph, change header and its plan panels, diff pane, annotate, evolution, drag-and-drop), conflicts, publish, bookmarks, operations — and the overlays (command palette, history search, context menu). |
| `theme.rs`, `store.rs`, `diagnostics.rs` | The ten palettes; the persisted preferences; the log and the record a panic leaves. |

## Design

One look, stated once so new surfaces keep it. The tokens are in
`theme.rs`, the parts in `ui/`; views add layout, not material.

- **Flat and ruled.** Depth is the four surface steps and hairlines. No
  gradients, glass, glows or pills; the one shadow belongs to surfaces
  that float. Hairlines are strong enough to read as structure.
- **A frame around the work.** The title bar, the rail and the status
  line are one L of bg0, a clear step darker than the work area (bg1)
  they hold. The frame's hairline runs over the work area only, and the
  open repo's tab is cut from bg1 and runs down into it, browser-style:
  flush, never an inset or floating sheet.
- **The graph is a transit map.** Each workstream is a line in its own
  lane hue: three-point rails, S-bends where a line changes track, and
  stations on the line — a ring per change, filled when selected; a
  ring around a dot for the working copy (you are here); history's
  diamond; a red disc with the cross cut from it for a conflict. The
  first stream (the working copy's) owns the accent lane and no other
  line wears it (`lane_slot`). A change id's unique prefix wears its
  line's hue, the way jj colors it in a terminal.
- **Marks travel.** Wherever a change appears outside the graph — the
  header's title, a bookmark's target, a stack's landing order, the
  operations journal, the status line — it carries the graph's mark in
  the graph's hue (`ui::node_mark`, `ui::mark_for`, `ui::line_of`). A
  selection takes its line's hue too: a tint across the row and the
  line's color on the leading bar (`ui::lead_bar_in`).
- **One toolbar line.** Every pane's first bar is `TOOLBAR_H` tall, so a
  single hairline runs the window's width: the graph head, the change
  header, a section's page bar, the repo overview.
- **The title bar is the way in.** The window's center line carries the
  command field (⌘K), the one entry to every command and search; the
  palette and history search open out of it, their search line where the
  field was. Beside it, trunk's state and the fetch age read quietly, and
  one Settings control holds appearance, integrations and the licence.
  An unregistered copy adds a single Register control. Nothing else.
- **Hue is identity.** Every color names something: a line (lanes), a
  section (`Section::hue`, its rail tile and page mark), a repository or
  an author (`Theme::identity`, `ui::monogram`), an operation's family
  (`op_kind`), a file's language (`ui::file_icon`), a status, a diff
  side. Surfaces stay neutral; hue arrives in small, saturated marks.
  What is current without a line of its own — the file tree's row, a
  page's selection — carries the two-pixel accent bar (`ui::lead_bar`)
  on the neutral `selected` fill.
- **Rows run edge to edge.** Graph rows, the file tree, a page's ledger:
  no inset rounded rows on a pane. Rounded rows belong to menus, trays
  and popovers.
- **Labels are outlined.** A ref is a hairline box (`ui::ref_label`),
  trunk the same in the accent (`ui::trunk_label`), a state word the
  same in its tone (`ui::tag`). No filled chips: a row of labels must
  stay lighter than the title beside it.
- **Labels fit or are counted.** A graph row gives its title what it
  needs, up to half the row; refs truncate into the rest, and whatever
  cannot is a closing "+N" that names them on hover. Nothing clips.
- **The graph hugs its rails.** A row's text starts after its own rails
  (`row_gutter_width`), the way `jj log` prints it — a lone stack's titles
  sit beside their rail however wide deep history runs below.
- **Files are known by their kind.** A file wears its language's icon
  and hue in the tree and its diff header. A modified file, the usual
  case, carries no further mark; the files that came, went or moved say
  so in their side's hue. A changed line has a solid edge in its side's
  hue down the gutter. A "+a −r" count closes with the five-block
  diffstat (`ui::diffstat`), the split read before the numbers are.
- **A section is a page bar and a ledger** (`ui::page_bar_marked`,
  `ui::group_head`, `ui::page_notice`, `ui::tray`): the section's mark,
  name and count on the toolbar line with the page's filter and acts,
  pinned group
  bands, then full-width rows in columns. No centered reading column,
  no opening paragraph or lede in the bar — what a group is for is said
  once, in its band or under it.
- **Empty is a state, not an absence.** `ui::empty_state`: the glyph on a
  hairline tile, a title, one sentence, and the jj command that would
  change it, as you would type it. Good news (`ui::all_clear`) carries a
  check in the ok tone.
- **Figures line up.** Counts, ages, times and line numbers use tabular
  figures (`ui::Tabular`) or the mono face, so columns hold still as they
  change. Ids are always mono; a change id's unique prefix carries the
  weight.
- **Text keeps its floors.** text2 at 8:1 or better and text3 at 5.5:1
  on bg0–bg2; hierarchy comes from weight, size and the three steps,
  never from fading. Sentence case, no letter-spaced caps.

## GPUI notes

Things this code works around, so the next change does not rediscover them:

- **Page columns are block flow, not flex columns.** A flex column asked
  for its height (which a scroller always does) sizes each row before
  stretching it to the column's width. Rows whose content wraps are then
  measured unwrapped and squeezed, and groups sit at their content's
  width. `div()` stacks the same rows and measures them at full width.
- **A hover style that sets a text color replaces the element's whole text
  style** while it applies — size, weight and family fall back to the
  parent's. Elements that shape text at paint time (`ui::truncated`,
  `ui::wrapped`) keep the face they were laid out in.
- **A tooltip waiting out its delay is never cancelled by a click**, and a
  visible one is retired only when the pointer next moves. `ui::tooltip`
  guards against both; `ui::popover_layer` puts a layer under an open
  panel so its trigger cannot raise a tooltip over it.
- **A purely sideways swipe scrolls a vertical-only scroller.** Use
  `.scroll_y()` (or `ui::vertical_only` for a `uniform_list`), never
  `.overflow_y_scroll()` directly.
- **Only a trackpad says where a scroll gesture starts.** Smooth-scrolling
  mice send precise deltas with no touch phase, so an axis lock that
  waits for `TouchPhase::Started` holds its first axis for the life of
  the view. A view that scrolls both ways puts its wheel events through
  `ui::AxisLock`, which also starts a gesture after a pause and turns
  one that set off on the wrong axis.
- **A scroller hears the wheel before its ancestors do**, and spends a
  sideways swipe's vertical drift on its rows. An ancestor that pans
  listens in the capture phase (`ui::wheel_first`) and stops the event.
- **A window reads the real pointer when it opens and when its bounds
  change**, hidden or not, so hover would follow wherever the user's own
  pointer rests. A scripted run starts with its pointer off the window and
  puts it back after a `resize` (`script::run`).
- **A window that is not visible is never asked for a frame**, and GPUI
  keeps its dirty flag to itself. A headless run steps frames by hand and
  reads the flag from `App::debug_state` (`headless::pump_frames`), so it
  draws only when the app asked — a missed `cx.notify()` stays visible.
- **A scripted `scroll` is one precise delta with no touch phase** — the
  smooth-mouse case, not the trackpad one. Scroll down before scrolling
  sideways in a script; a sideways scroll fresh from launch only covers a
  gesture's first report.
- **A panic cannot be contained to a view.** Frames and events arrive
  through `extern "C"` AppKit callbacks, which abort on an unwind. Catching
  one sooner leaves GPUI broken: the entity being updated is dropped from
  the entity map while its handles live on (`LeaseInner::drop`), `App`'s
  pending-update count is never wound back, so effects stop flushing, and
  the window's draw stacks are left pushed. So a panic ends the session:
  `diagnostics` records it with its backtrace, and the next launch says so
  in the status line, with the tabs restored.
