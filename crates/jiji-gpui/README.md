# jiji-gpui

Jiji's desktop app as a Rust-only program: the same `jiji-core`,
`jiji-forge` and `jiji-cli` crates the Tauri app uses, with the interface
drawn by [GPUI](https://www.gpui.rs) instead of a webview. No IPC, no
serialization boundary, no JavaScript — views call the backend directly and
paint from the snapshot it returns.

It is its own Cargo workspace with its own lockfile and `target/`, so the
Tauri app's build, lockfile and CI never see GPUI. The binary is
`jiji-desktop`, like the Tauri one, and it reads and writes the same data
directory (`~/Library/Application Support/com.lauf.jiji`), so recents, saved
views, the licence and auto-land records carry over.

## Run

```sh
cd crates/jiji-gpui
cargo run                         # the app; debug builds count as registered
cargo run --release
scripts/bundle.sh                 # target/release/bundle/Jiji GPUI.app
JIJI_BUNDLE_RELEASE=1 scripts/bundle.sh   # Jiji.app + the updater archive
```

With arguments the binary is the `jiji` CLI (`jiji-desktop publish …`), so
the app's own switches are environment variables:

| Variable | |
|---|---|
| `JIJI_OPEN=<path>` | open this repository at launch |
| `JIJI_DATA_DIR=<dir>` | keep preferences, recents and licence here instead of the shared directory — set it for any test run |
| `JIJI_MOCK_BACKEND=1` | serve the mock snapshot (needs a directory containing `.jj`) |
| `JIJI_DEV_LICENSE=unregistered\|real` | debug builds only: exercise the unregistered state, or read the real licence |
| `JIJI_BACKGROUND=1` | float the window without taking focus (for scripted runs) |
| `JIJI_SCRIPT=<steps>` | drive the window: see `src/script.rs` |

## Scripted runs

`JIJI_SCRIPT` takes `;`-separated steps and delivers them to the app's own
window as native events, in-process — the pointer, the keyboard focus and
(unless a step copies) the clipboard stay the user's:

```sh
JIJI_DATA_DIR=/tmp/jiji-test JIJI_BACKGROUND=1 JIJI_OPEN=~/Dev/some-repo \
JIJI_SCRIPT="wait=1500;rclick=200,150;key=down;shot=/tmp/menu.png;quit" \
  target/debug/jiji-desktop
```

`click`, `dclick`, `rclick`, `move`, `drag`, `press`/`hold`/`release` (a drag
in parts), `scroll`, `key`, `type`, `wait`, `shot`, `resize`, `quit`;
prefixes `shift-`, `alt-`, `cmd-`. Coordinates are window points;
screenshots are at the display's scale.

## Layout

| | |
|---|---|
| `host/` | The backend the views call: repo sessions and their watchers, the forge workflows (publish, land, ship, auto-land), the CLI launcher, editor and terminal hand-offs, licensing, updates. Blocking; views run it on the background executor. |
| `app/` | The app model — one entity holding tabs, each tab's selection and breadcrumb, the forge connection, upstream checks, auto-land jobs, preferences, and the context menu. Views read it and call its methods. |
| `logic/` | What to draw and what each affordance means, with no UI types: graph layout, stack and conflict grouping, the split checklist, menu and palette contents, drag-and-drop policy, timelines. Unit-tested. |
| `diff/` | The diff surface: a flat row model, tree-sitter highlighting, and one custom-painted element that windows, selects, finds and scrolls. |
| `ui/` | Buttons, inputs (with IME), popovers and the layer that hangs them off a trigger, tooltips, scrollbars, icons, and the text and list elements GPUI lacks: one line that ellipsizes, a paragraph that wraps, a group heading that pins. |
| `shell/` | The window frame: tabs, top bar, section rail, status bar, menus, key bindings. |
| `views/` | The sections — workbench (graph, change header and its plan panels, diff pane, annotate, evolution, drag-and-drop), conflicts, publish, bookmarks, operations — and the overlays (command palette, history search, context menu). |
| `theme.rs`, `store.rs` | The ten palettes; the persisted preferences. |

## Differences from the Tauri app

- The change header's meta bar never wraps: facts leave by pane width, as
  they do there, and whatever still overruns clips. The header's verbs
  keep their labels below the Tauri breakpoint whenever the title leaves
  room for them.
- A diff file's sideways scroll thumb shows only while the pointer is over
  that file, so it never covers another file's last line.
- The annotate pane pans sideways by swipe; it draws no horizontal thumb.
- Bookmark rows always reserve room for their hover actions.
- No layout or panel transitions.
- The in-app updater downloads and verifies releases like the Tauri one, but
  no release pipeline builds this bundle yet.

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
