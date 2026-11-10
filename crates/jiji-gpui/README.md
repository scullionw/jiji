# jiji-gpui

A spike: Jiji's diff view rendered with [GPUI](https://www.gpui.rs) instead of
the webview, to judge whether a Rust-only frontend is worth pursuing. It calls
`jiji-core` directly (no IPC, no serialization) and draws the diff as one
custom-painted element.

It is its own Cargo workspace with its own lockfile and `target/`, so the
shipping app's build, lockfile and CI never see GPUI. Deleting this directory
removes the spike.

## Run

```sh
cd crates/jiji-gpui
cargo run --release -- ~/Dev/some-jj-repo                  # the working copy's diff
cargo run --release -- . --change 87f92aea9                # one change (change-id prefix or commit id)
cargo run --release -- . --from 4cef44d39 --change c015172af --split
```

| Key | |
|---|---|
| `s` | unified ↔ split |
| `]` / `[` | next / previous file |
| `space` / `shift-space` | page down / up |
| `cmd-up` / `cmd-down` | top / bottom |
| `cmd-a`, `cmd-c` | select all, copy |
| `b` | run the scroll benchmark |
| `f` | toggle the frame stats in the toolbar |

Drag selects text (one side at a time in the split layout), double-click a
word, triple-click a line, shift-click extends. Dragging past the top or bottom
edge scrolls. Sideways scrolling pans the file under the pointer.

## What is here

- Windowing over a flat row model (`model.rs`, a port of `diff.ts`): fixed row
  heights, a prefix sum, a binary search. Nothing off screen is shaped or
  painted.
- Unified and split layouts, the split aligned on the backend's similarity
  pairs, with intraline word emphasis.
- Syntax highlighting with tree-sitter (`highlight.rs`), per hunk side like the
  webview's lezer pass, on background threads: Rust, TypeScript/TSX,
  JavaScript, JSON, CSS, HTML, Python, Markdown, YAML, TOML, Svelte.
- Selection and copy, pinned file headers, per-file horizontal pan, overlay
  scroll thumbs, the Midnight palette from `tokens.css`.

## What is not

Collapsing files, the file tree, find-in-diff, the context menu, image diffs,
the split (jj split) panel, themes other than Midnight, and everything outside
the diff view. Conflicted files render as plain full-width sections.

## Scripted runs

For checking the view without the pointer or the keyboard:

```sh
jiji-gpui . --change 87f92aea9 --background \
  --script "key=s;scroll=5200;drag=900,93,1000,450;print;shot=/tmp/split.png;quit"
jiji-gpui . --change 4cef44d39 --bench --exit --background
```

`--script` steps call the same handlers real input reaches, in window
coordinates; `print` writes what `cmd-c` would copy to stdout and `shot=`
writes the window to a PNG from inside the process (`screencapture` needs
Screen Recording permission; an app reading its own window does not).
`--background` does not take focus; the window floats instead, because GPUI
stops painting a window that is fully covered. `--bench` sweeps the diff at
9000 and 18000 px/s and prints frame intervals, paint time and whole-process
CPU per frame. `--repeat N` multiplies the diff to stress the windowing.

## Build notes

- `gpui` is built with `runtime_shaders`, so no Xcode Metal Toolchain component
  is needed; shaders compile at launch.
- macOS only as written (`capture.rs` is AppKit FFI for the scripted-run
  helpers; the rest is portable GPUI).
