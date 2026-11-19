//! What the Tauri app (Jiji 0.4 and earlier) left, carried into the store
//! the first time this app starts after it.
//!
//! Its session and preferences lived in its web view's local storage: a
//! SQLite file WebKit keeps for the page's origin, each value UTF-16 text
//! in the format the page wrote it in. Its window was the window-state
//! plugin's file in the app data directory. Both are read once, and only
//! read; a marker in `prefs.json` says it is done. A preference already
//! set here keeps its value. A store that is there but cannot be read is
//! tried again at the next launch, and a value that makes no sense costs
//! only its key. A run with `JIJI_DATA_DIR` set keeps its data elsewhere
//! and never looks at the real profile.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::{point, px, size, App, Bounds, Pixels};
use jiji_core::snapshot::SnapshotView;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::app::UPSTREAM_INTERVALS;
use crate::shell::placement::{DEFAULT_SIZE, MIN_SIZE};
use crate::store::{PersistedTabs, Store, ThemePref, WindowState, APP_ID, REGISTERED};
use crate::theme::{self, Scheme};

/// Set in `prefs.json` once the Tauri profile has been read.
const CARRIED_OVER: &str = "tauri.imported";

/// The window-state plugin's file, in the app data directory.
const WINDOW_FILE: &str = ".window-state.json";

/// A repo's lens was kept under this and the repo's path.
const LENS: &str = "jiji.workbench.lens:";

/// How the page wrote a value.
#[derive(Clone, Copy)]
enum Format {
    /// The open tabs as JSON, in the shape this store keeps them in.
    Tabs,
    /// One of a few words.
    Word(&'static [&'static str]),
    /// "1" or "0".
    Bit,
    /// A pane's width in points.
    Width,
    /// An editor's or a terminal's id.
    Id,
    /// The fetch interval in minutes, 0 for Off.
    Interval,
    /// The theme as JSON, or a bare mode from before named palettes.
    Theme,
}

/// Each key the page kept, the preference it is here, and how it was
/// written. The lenses, one key per repo, come apart.
const KEYS: &[(&str, &str, Format)] = &[
    ("jiji.tabs", "tabs", Format::Tabs),
    (
        "jiji.workbench.view",
        "workbench.view",
        Format::Word(&["graph", "focus"]),
    ),
    (
        "jiji.workbench.density",
        "workbench.density",
        Format::Word(&["compact", "classic"]),
    ),
    ("jiji.rail.hidden", "rail.hidden", Format::Bit),
    ("jiji.graph.collapsed", "graph.collapsed", Format::Bit),
    ("jiji.pane.graph", "pane.graph", Format::Width),
    (
        "jiji.diff.layout",
        "diff.layout",
        Format::Word(&["unified", "split"]),
    ),
    ("jiji.diff.tree", "diff.tree", Format::Bit),
    ("jiji.pane.filetree", "pane.filetree", Format::Width),
    ("jiji-theme", "theme", Format::Theme),
    ("jiji.editor.preferred", "editor.preferred", Format::Id),
    ("jiji.editor.last", "editor.last", Format::Id),
    ("jiji.terminal.preferred", "terminal.preferred", Format::Id),
    (
        "jiji.upstream.interval",
        "upstream.interval",
        Format::Interval,
    ),
    (
        "jiji.cli.offer-dismissed",
        "cli.offer-dismissed",
        Format::Bit,
    ),
    // The page read it before first paint to gate the themes; here it is
    // what a licence file that cannot be read falls back on.
    ("jiji-registered", REGISTERED, Format::Bit),
];

/// Carry the Tauri app's profile over, the first time this app starts.
pub fn carry_over(store: &mut Store, cx: &App) {
    let profile = webkit_profile(std::env::var_os("JIJI_DATA_DIR"), std::env::var_os("HOME"));
    if let Some(webkit) = profile {
        carry_over_from(store, &webkit, || arrangement(cx));
    }
}

/// Where WebKit kept the Tauri app's page data. A run that keeps its own
/// data elsewhere has no Tauri past.
fn webkit_profile(data_dir: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    if data_dir.is_some() || !cfg!(target_os = "macos") {
        return None;
    }
    Some(PathBuf::from(home?).join("Library/WebKit").join(APP_ID))
}

/// Read the profile WebKit kept under `webkit`, and the window-state file
/// beside the store, into the store: once. `screens` is asked for only
/// when there is a window to place.
fn carry_over_from(store: &mut Store, webkit: &Path, screens: impl FnOnce() -> Vec<GlobalScreen>) {
    if store.flag(CARRIED_OVER) {
        return;
    }
    let stored = local_storage(webkit);
    let (mut prefs, lenses) = match &stored {
        Ok(items) => {
            if !items.is_empty() {
                log::info!(
                    "carrying over what the Tauri app kept: {} values",
                    items.len()
                );
            }
            preferences(items)
        }
        Err(err) => {
            log::warn!(
                "could not read the Tauri app's preferences; trying again next launch: {err}"
            );
            (Map::new(), Map::new())
        }
    };
    let window = std::fs::read(store.dir().join(WINDOW_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<TauriWindows>(&bytes).ok())
        .and_then(|windows| window_state(&windows.main, &screens()))
        .and_then(|state| serde_json::to_value(state).ok());
    if let Some(window) = window {
        prefs.insert("window".into(), window);
    }
    if stored.is_ok() {
        prefs.insert(CARRIED_OVER.into(), Value::Bool(true));
    }
    store.adopt(prefs, lenses);
}

// ── The page's local storage ──

/// Every key the page kept and its text. No store at all is an empty
/// list; one that is there and cannot be read is the reason why.
fn local_storage(webkit: &Path) -> Result<Vec<(String, String)>, String> {
    let Some(db) = find_storage(webkit) else {
        return Ok(Vec::new());
    };
    // WebKit may still have the file open, with its latest writes in the
    // journal beside it. The query reads a copy of both, so nothing in
    // WebKit's directory is ever opened for writing.
    static COPIES: AtomicUsize = AtomicUsize::new(0);
    let copy = std::env::temp_dir().join(format!(
        "jiji-tauri-storage-{}-{}",
        std::process::id(),
        COPIES.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&copy);
    let read = stage(&db, &copy)
        .map_err(|err| err.to_string())
        .and_then(|staged| query(&staged));
    let _ = std::fs::remove_dir_all(&copy);
    read
}

/// WebKit keeps each origin's storage under hashed directories, with a
/// file beside it naming the origin. The Tauri app's page was
/// `tauri://localhost`.
fn find_storage(webkit: &Path) -> Option<PathBuf> {
    let origin = tauri_origin();
    std::fs::read_dir(webkit.join("WebsiteData/Default"))
        .ok()?
        .flatten()
        .filter_map(|top| std::fs::read_dir(top.path()).ok())
        .flat_map(|frames| frames.flatten())
        .map(|frame| frame.path())
        .find(|frame| std::fs::read(frame.join("origin")).is_ok_and(|bytes| bytes == origin))
        .map(|frame| frame.join("LocalStorage/localstorage.sqlite3"))
        .filter(|db| db.is_file())
}

/// How WebKit writes the origin of a page that is its own top frame: the
/// top origin, then the frame's, each a scheme and a host (a 32-bit
/// little-endian length, 1 for 8-bit text, the text) and 0 for no port.
fn tauri_origin() -> Vec<u8> {
    let mut origin = Vec::new();
    for _ in 0..2 {
        for part in ["tauri", "localhost"] {
            origin.extend((part.len() as u32).to_le_bytes());
            origin.push(1);
            origin.extend(part.as_bytes());
        }
        origin.push(0);
    }
    origin
}

/// Copy the store, and its journal if there is one, into `copy`.
fn stage(db: &Path, copy: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(copy)?;
    let staged = copy.join("localstorage.sqlite3");
    std::fs::copy(db, &staged)?;
    let journal = db.with_file_name("localstorage.sqlite3-wal");
    if journal.is_file() {
        std::fs::copy(&journal, copy.join("localstorage.sqlite3-wal"))?;
    }
    Ok(staged)
}

fn query(db: &Path) -> Result<Vec<(String, String)>, String> {
    let output = Command::new("/usr/bin/sqlite3")
        // Not the user's ~/.sqliterc, and one known shape out: each row's
        // key and value in hex, which no path or text can break.
        .args(["-init", "/dev/null", "-batch", "-bail", "-noheader"])
        .args(["-list", "-separator", "|"])
        .arg(db)
        .arg("SELECT hex(key), hex(value) FROM ItemTable")
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    let rows = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('|')?;
            let key = String::from_utf8(unhex(key)?).ok()?;
            // Each value is UTF-16 in the machine's byte order.
            let value = unhex(value)?;
            let (pairs, rest) = value.as_chunks::<2>();
            if !rest.is_empty() {
                return None;
            }
            let units: Vec<u16> = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
            Some((key, String::from_utf16(&units).ok()?))
        })
        .collect();
    Ok(rows)
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}

/// The page's values as this store keeps them: the preferences, and the
/// lenses by repo.
fn preferences(items: &[(String, String)]) -> (Map<String, Value>, Map<String, Value>) {
    let mut prefs = Map::new();
    let mut lenses = Map::new();
    for (key, text) in items {
        if let Some(repo) = key.strip_prefix(LENS) {
            if let Some(lens) = lens(text).filter(|_| !repo.is_empty()) {
                lenses.insert(repo.to_owned(), lens);
            }
        } else if let Some((_, here, format)) = KEYS.iter().find(|(there, ..)| there == key) {
            if let Some(value) = convert(*format, text) {
                prefs.insert((*here).to_owned(), value);
            }
        }
    }
    (prefs, lenses)
}

fn convert(format: Format, text: &str) -> Option<Value> {
    match format {
        Format::Tabs => {
            let tabs = PersistedTabs::decode(serde_json::from_str::<Value>(text).ok().as_ref());
            if tabs.repos.is_empty() {
                return None;
            }
            serde_json::to_value(tabs).ok()
        }
        Format::Word(words) => words.contains(&text).then(|| Value::from(text)),
        Format::Bit => match text {
            "1" => Some(Value::Bool(true)),
            "0" => Some(Value::Bool(false)),
            _ => None,
        },
        Format::Width => text
            .parse::<f32>()
            .ok()
            .filter(|width| width.is_finite() && *width > 0.)
            .map(Value::from),
        Format::Id => (!text.trim().is_empty()).then(|| Value::from(text)),
        Format::Interval => text
            .parse::<u32>()
            .ok()
            .filter(|minutes| UPSTREAM_INTERVALS.iter().any(|(m, _)| m == minutes))
            .map(Value::from),
        Format::Theme => serde_json::to_value(theme_pref(text)?).ok(),
    }
}

/// The theme: its mode and its two palettes, each kept when it names one
/// this app has. Before named palettes it was a bare mode.
fn theme_pref(text: &str) -> Option<ThemePref> {
    const MODES: [&str; 3] = ["system", "light", "dark"];
    let mut pref = ThemePref::default();
    if MODES.contains(&text) {
        pref.mode = text.into();
        return Some(pref);
    }
    let stored: Map<String, Value> = serde_json::from_str(text).ok()?;
    let field = |name: &str| stored.get(name).and_then(Value::as_str);
    let mode = field("mode").filter(|mode| MODES.contains(mode));
    let palette = |name, scheme| field(name).filter(|id| theme::scheme_of(id) == Some(scheme));
    let (dark, light) = (
        palette("dark", Scheme::Dark),
        palette("light", Scheme::Light),
    );
    if mode.is_none() && dark.is_none() && light.is_none() {
        return None;
    }
    if let Some(mode) = mode {
        pref.mode = mode.into();
    }
    if let Some(dark) = dark {
        pref.dark = dark.into();
    }
    if let Some(light) = light {
        pref.light = light.into();
    }
    Some(pref)
}

/// A lens as the page's generated bindings wrote it: the backend's
/// `SnapshotView`. The default lens was never kept, and a custom one needs
/// a revset.
fn lens(text: &str) -> Option<Value> {
    let view: SnapshotView = serde_json::from_str(text).ok()?;
    let kept = match &view {
        SnapshotView::Workstreams => false,
        SnapshotView::Custom { revset } => !revset.trim().is_empty(),
        _ => true,
    };
    kept.then(|| serde_json::to_value(view).ok()).flatten()
}

// ── The window ──

/// The plugin's file: a record per window label. Jiji's one window was
/// `main`.
#[derive(Deserialize)]
struct TauriWindows {
    main: TauriWindow,
}

/// The plugin's record of a window: its top-left and size in physical
/// pixels, the top-left in the desktop's space (from the primary display's
/// top-left), at the scale of the display it was on. While zoomed it
/// stopped following the size, and `prev_x` and `prev_y` held the place
/// from before the last move: together, the frame from before zooming.
#[derive(Deserialize)]
struct TauriWindow {
    width: f32,
    height: f32,
    x: f32,
    y: f32,
    prev_x: f32,
    prev_y: f32,
    maximized: bool,
    fullscreen: bool,
}

/// A display in the desktop's space: where it sits, in points from the
/// primary display's top-left, and at what scale.
#[derive(Clone, Debug)]
struct GlobalScreen {
    uuid: Option<String>,
    frame: Bounds<Pixels>,
    scale: f32,
}

/// The displays, the primary first. GPUI knows each by itself only;
/// CoreGraphics says where it sits and at what scale.
#[cfg(target_os = "macos")]
fn arrangement(cx: &App) -> Vec<GlobalScreen> {
    use core_graphics::display::CGDisplay;
    crate::shell::placement::screens(cx)
        .into_iter()
        .map(|screen| {
            // GPUI's display id on macOS is CoreGraphics' own.
            let display = CGDisplay::new(u64::from(screen.id) as u32);
            let rect = display.bounds();
            let scale = display
                .display_mode()
                .map(|mode| mode.pixel_width() as f32 / mode.width() as f32)
                .filter(|scale| scale.is_finite() && *scale > 0.)
                .unwrap_or(1.);
            GlobalScreen {
                uuid: screen.uuid,
                frame: Bounds::new(
                    point(px(rect.origin.x as f32), px(rect.origin.y as f32)),
                    size(px(rect.size.width as f32), px(rect.size.height as f32)),
                ),
                scale,
            }
        })
        .collect()
}

#[cfg(not(target_os = "macos"))]
fn arrangement(_: &App) -> Vec<GlobalScreen> {
    Vec::new()
}

/// The window as this store keeps it. Each display's scale makes the
/// record a frame: the one that lands on that display, at a size the
/// window could have had, is where it was. One no display shows now opens
/// centered on the primary.
fn window_state(saved: &TauriWindow, screens: &[GlobalScreen]) -> Option<WindowState> {
    if !(saved.width > 0. && saved.height > 0.) {
        // The plugin's blank record: nothing was saved.
        return None;
    }
    let (x, y) = if saved.maximized {
        (saved.prev_x, saved.prev_y)
    } else {
        (saved.x, saved.y)
    };
    let at_scale = |scale: f32| {
        Bounds::new(
            point(px(x / scale), px(y / scale)),
            size(px(saved.width / scale), px(saved.height / scale)),
        )
    };
    let mut best: Option<(f32, &GlobalScreen, Bounds<Pixels>)> = None;
    for screen in screens {
        let frame = at_scale(screen.scale);
        let shown = shown_part(&frame, &screen.frame);
        if shown <= 0. {
            continue;
        }
        let possible = frame.size.width >= MIN_SIZE.width && frame.size.height >= MIN_SIZE.height;
        let score = shown + if possible { 1. } else { 0. };
        if best.as_ref().is_none_or(|(top, ..)| score > *top) {
            best = Some((score, screen, frame));
        }
    }
    let middle = |screen: &GlobalScreen, size| {
        Bounds::centered_at(
            Bounds::new(point(px(0.), px(0.)), screen.frame.size).center(),
            size,
        )
    };
    let (screen, frame) = match best {
        Some((_, screen, frame)) => (screen, frame - screen.frame.origin),
        None => {
            let primary = screens.first()?;
            (primary, middle(primary, at_scale(primary.scale).size))
        }
    };
    // Full screen, the frame was the display's own; the one before it is
    // not known.
    let frame = if saved.fullscreen {
        middle(screen, DEFAULT_SIZE)
    } else {
        frame
    };
    Some(WindowState {
        display: screen.uuid.clone(),
        x: frame.origin.x.into(),
        y: frame.origin.y.into(),
        width: frame.size.width.into(),
        height: frame.size.height.into(),
        zoomed: saved.maximized && !saved.fullscreen,
        fullscreen: saved.fullscreen,
    })
}

/// How much of `frame` is on `screen`, from 0 to 1.
fn shown_part(frame: &Bounds<Pixels>, screen: &Bounds<Pixels>) -> f32 {
    let overlap = frame.intersect(screen);
    let width = f32::from(overlap.size.width).max(0.);
    let height = f32::from(overlap.size.height).max(0.);
    width * height / (f32::from(frame.size.width) * f32::from(frame.size.height))
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, WindowBounds};
    use serde_json::json;

    use super::*;
    use crate::app::UpstreamUi;
    use crate::shell::placement;
    use crate::testing::{install, jj_repo, scratch};

    fn utf16_hex(text: &str) -> String {
        text.encode_utf16()
            .flat_map(u16::to_le_bytes)
            .map(|byte| format!("{byte:02X}"))
            .collect()
    }

    fn sqlite(db: &Path, sql: &str) {
        let ran = Command::new("/usr/bin/sqlite3")
            .args(["-init", "/dev/null"])
            .arg(db)
            .arg(sql)
            .status()
            .expect("sqlite3 runs");
        assert!(ran.success(), "{sql}");
    }

    /// A page's local storage the way WebKit leaves it: under hashed
    /// directories beside a file naming `origin`, each value UTF-16.
    fn page_storage(webkit: &Path, hash: &str, origin: &[u8], items: &[(&str, &str)]) -> PathBuf {
        let frame = webkit.join("WebsiteData/Default").join(hash).join(hash);
        std::fs::create_dir_all(frame.join("LocalStorage")).unwrap();
        std::fs::write(frame.join("origin"), origin).unwrap();
        let db = frame.join("LocalStorage/localstorage.sqlite3");
        sqlite(
            &db,
            "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, \
             value BLOB NOT NULL ON CONFLICT FAIL)",
        );
        for (key, value) in items {
            let key = key.replace('\'', "''");
            sqlite(
                &db,
                &format!(
                    "INSERT INTO ItemTable VALUES ('{key}', X'{}')",
                    utf16_hex(value)
                ),
            );
        }
        db
    }

    /// The Tauri app's storage as 0.4.1 left it.
    fn tauri_storage(webkit: &Path, items: &[(&str, &str)]) -> PathBuf {
        page_storage(webkit, "zlzHKJr4zEpwKGo0", &tauri_origin(), items)
    }

    /// A 5K display at twice its points, alone.
    fn retina() -> GlobalScreen {
        GlobalScreen {
            uuid: Some("retina".into()),
            frame: Bounds::new(point(px(0.), px(0.)), size(px(2560.), px(1440.))),
            scale: 2.,
        }
    }

    /// The window-state record of a window zoomed from 980×885 at (0, 35)
    /// on `retina`, the way a 0.4.1 install on this display left it.
    fn zoomed_record() -> Value {
        json!({"main": {
            "width": 1960, "height": 1770, "x": 0, "y": 60, "prev_x": 0, "prev_y": 70,
            "maximized": true, "visible": true, "decorated": true, "fullscreen": false,
        }})
    }

    fn zoomed_state() -> WindowState {
        WindowState {
            display: Some("retina".into()),
            x: 0.,
            y: 35.,
            width: 980.,
            height: 885.,
            zoomed: true,
            fullscreen: false,
        }
    }

    #[gpui::test]
    fn an_upgrade_keeps_the_tauri_session_and_preferences(cx: &mut TestAppContext) {
        let dir = scratch("upgrade");
        let (alpha, beta) = (jj_repo(&dir, "alpha"), jj_repo(&dir, "beta"));
        // Both fetch from a remote: opening either would fetch at once,
        // were fetching not off.
        let remote = dir.join("remote.git").to_string_lossy().into_owned();
        let git = |args: &[&str]| {
            let ran = Command::new("git").args(args).status().expect("git runs");
            assert!(ran.success(), "git {args:?}");
        };
        git(&["init", "--quiet", "--bare", &remote]);
        for repo in [&alpha, &beta] {
            git(&["-C", repo, "remote", "add", "origin", &remote]);
        }
        let tabs = json!({
            "repos": [{"path": alpha, "name": "alpha"}, {"path": beta, "name": "beta"}],
            "active": 1,
        })
        .to_string();
        let (alpha_lens, beta_lens) = (format!("{LENS}{alpha}"), format!("{LENS}{beta}"));
        let webkit = dir.join("WebKit");
        tauri_storage(
            &webkit,
            &[
                ("jiji.tabs", &tabs),
                (
                    &alpha_lens,
                    r#"{"kind":"custom","revset":"mine() & ~empty()"}"#,
                ),
                (&beta_lens, r#"{"kind":"trunk"}"#),
                ("jiji.workbench.view", "focus"),
                ("jiji.workbench.density", "classic"),
                ("jiji.rail.hidden", "1"),
                ("jiji.graph.collapsed", "1"),
                ("jiji.pane.graph", "512"),
                ("jiji.diff.layout", "split"),
                ("jiji.diff.tree", "0"),
                ("jiji.pane.filetree", "300"),
                (
                    "jiji-theme",
                    r#"{"mode":"dark","dark":"ember","light":"linen"}"#,
                ),
                ("jiji.editor.preferred", "zed"),
                ("jiji.editor.last", "cursor"),
                ("jiji.terminal.preferred", "ghostty"),
                ("jiji.upstream.interval", "0"),
                ("jiji.cli.offer-dismissed", "1"),
                ("jiji-registered", "1"),
            ],
        );
        let data = dir.join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join(WINDOW_FILE), zoomed_record().to_string()).unwrap();
        carry_over_from(&mut Store::at(data.clone()), &webkit, || vec![retina()]);

        // The first launch, on what was carried over and read back.
        let model = install(&dir, cx);
        model.read_with(cx, |model, _| {
            assert_eq!(model.upstream.interval_minutes, 0)
        });
        cx.run_until_parked();
        model.read_with(cx, |model, _| {
            let paths: Vec<_> = model.tabs.iter().map(|tab| tab.path.clone()).collect();
            assert_eq!(paths, [Some(alpha.clone()), Some(beta.clone())]);
            assert_eq!(model.tab().path.as_deref(), Some(beta.as_str()));
            let shown = model.snapshot().map(|snapshot| snapshot.view.clone());
            assert_eq!(shown, Some(SnapshotView::Trunk));
            // Off: the repo on screen has a remote, and nothing fetched.
            let upstream = &model.upstream;
            assert!(
                !upstream.checking && upstream.last_checked.is_none() && upstream.error.is_none()
            );

            assert!(model.sidebar_collapsed && model.graph_collapsed && model.cli.dismissed);
            assert_eq!(
                model.theme_pref(),
                ThemePref {
                    mode: "dark".into(),
                    dark: "ember".into(),
                    light: "linen".into(),
                }
            );
            assert_eq!(model.preferred_editor().as_deref(), Some("zed"));
            assert_eq!(model.preferred_terminal().as_deref(), Some("ghostty"));
            assert!(model.store.was_registered());
        });

        // And so it stays: everything is in the store's files.
        let store = Store::at(data);
        let text = |key| store.pref::<String>(key);
        assert_eq!(text("workbench.view").as_deref(), Some("focus"));
        assert_eq!(text("workbench.density").as_deref(), Some("classic"));
        assert_eq!(text("diff.layout").as_deref(), Some("split"));
        assert_eq!(text("editor.last").as_deref(), Some("cursor"));
        assert_eq!(store.pref::<bool>("diff.tree"), Some(false));
        assert_eq!(store.pref::<f32>("pane.graph"), Some(512.));
        assert_eq!(store.pref::<f32>("pane.filetree"), Some(300.));
        assert_eq!(UpstreamUi::new(&store, false).interval_minutes, 0);
        assert_eq!(
            store.repo_view(&alpha),
            SnapshotView::Custom {
                revset: "mine() & ~empty()".into()
            }
        );
        assert_eq!(store.repo_view(&beta), SnapshotView::Trunk);
        // The zoomed window reopens zoomed, over the frame it unzooms to.
        assert_eq!(store.window(), Some(zoomed_state()));
        let screen = placement::Screen {
            id: gpui::DisplayId::new(1),
            uuid: Some("retina".into()),
            bounds: retina().frame,
            visible: Bounds::new(point(px(0.), px(25.)), size(px(2560.), px(1345.))),
        };
        assert_eq!(
            placement::restore(store.window().as_ref(), &[screen]).bounds,
            WindowBounds::Maximized(Bounds::new(
                point(px(0.), px(35.)),
                size(px(980.), px(885.))
            ))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn what_is_set_here_already_stays_and_the_profile_is_read_once() {
        let dir = scratch("upgrade-partial");
        let data = dir.join("data");
        let mut store = Store::at(data.clone());
        // A profile this app filled in part before it carried anything
        // over.
        store.set_pref("diff.layout", &"unified");
        store.set_pref("upstream.interval", &30u32);
        store.set_repo_view("/r/alpha", &SnapshotView::Mine);
        let here = WindowState {
            display: None,
            x: 248.,
            y: 137.,
            width: 1929.,
            height: 1141.,
            zoomed: false,
            fullscreen: false,
        };
        store.set_window(&here);
        let webkit = dir.join("WebKit");
        tauri_storage(
            &webkit,
            &[
                ("jiji.diff.layout", "split"),
                ("jiji.upstream.interval", "0"),
                ("jiji.workbench.lens:/r/alpha", r#"{"kind":"trunk"}"#),
                ("jiji.workbench.lens:/r/beta", r#"{"kind":"all"}"#),
                ("jiji.workbench.density", "classic"),
                ("jiji.editor.last", "zed"),
            ],
        );
        std::fs::write(data.join(WINDOW_FILE), zoomed_record().to_string()).unwrap();
        carry_over_from(&mut store, &webkit, || vec![retina()]);

        let store = Store::at(data.clone());
        assert_eq!(
            store.pref::<String>("diff.layout").as_deref(),
            Some("unified")
        );
        assert_eq!(store.pref::<u32>("upstream.interval"), Some(30));
        assert_eq!(store.repo_view("/r/alpha"), SnapshotView::Mine);
        assert_eq!(store.window(), Some(here));
        // What this app had not set comes over.
        assert_eq!(store.repo_view("/r/beta"), SnapshotView::All);
        assert_eq!(
            store.pref::<String>("workbench.density").as_deref(),
            Some("classic")
        );
        assert_eq!(store.pref::<String>("editor.last").as_deref(), Some("zed"));

        // Once: a later launch takes nothing again, even where the user
        // has since changed or cleared what came over.
        let mut store = Store::at(data.clone());
        store.set_pref("workbench.density", &"compact");
        store.clear_pref("editor.last");
        carry_over_from(&mut store, &webkit, || vec![retina()]);
        let store = Store::at(data);
        assert_eq!(
            store.pref::<String>("workbench.density").as_deref(),
            Some("compact")
        );
        assert_eq!(store.pref::<String>("editor.last"), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_value_that_makes_no_sense_costs_only_its_key() {
        let dir = scratch("upgrade-values");
        let webkit = dir.join("WebKit");
        let db = tauri_storage(
            &webkit,
            &[
                ("jiji.pane.graph", "wide"),
                ("jiji.pane.filetree", "280"),
                ("jiji.diff.layout", "sideways"),
                ("jiji.diff.tree", "0"),
                ("jiji.rail.hidden", "yes"),
                ("jiji.tabs", "{\"repos\": "),
                ("jiji.upstream.interval", "7"),
                // Neither a mode nor a dark palette; the light one is.
                (
                    "jiji-theme",
                    r#"{"mode":"sepia","dark":"paper","light":"linen"}"#,
                ),
                (
                    "jiji.workbench.lens:/r/blank",
                    r#"{"kind":"custom","revset":"  "}"#,
                ),
                ("jiji.workbench.lens:/r/odd", r#"{"kind":"sideways"}"#),
                ("jiji.workbench.lens:/r/good", r#"{"kind":"recent"}"#),
                ("jiji.workbench.lens:", r#"{"kind":"trunk"}"#),
            ],
        );
        // A value that is not UTF-16: half of a surrogate pair.
        sqlite(
            &db,
            "INSERT INTO ItemTable VALUES ('jiji.editor.last', X'00D8')",
        );
        // The development server's page, `http://localhost:1430`, kept
        // storage of its own beside the app's.
        let mut server = Vec::new();
        for _ in 0..2 {
            for part in ["http", "localhost"] {
                server.extend((part.len() as u32).to_le_bytes());
                server.push(1);
                server.extend(part.as_bytes());
            }
            server.push(1);
            server.extend(1430u16.to_le_bytes());
        }
        page_storage(
            &webkit,
            "devserver",
            &server,
            &[("jiji.workbench.view", "focus")],
        );
        let data = dir.join("data");
        carry_over_from(&mut Store::at(data.clone()), &webkit, Vec::new);

        let store = Store::at(data);
        assert_eq!(store.pref::<f32>("pane.filetree"), Some(280.));
        assert_eq!(store.pref::<bool>("diff.tree"), Some(false));
        assert_eq!(
            store.theme(),
            ThemePref {
                light: "linen".into(),
                ..ThemePref::default()
            }
        );
        assert_eq!(store.repo_view("/r/good"), SnapshotView::Recent);
        for key in [
            "pane.graph",
            "diff.layout",
            "rail.hidden",
            "tabs",
            "upstream.interval",
            "editor.last",
            "workbench.view",
        ] {
            assert_eq!(store.pref::<Value>(key), None, "{key}");
        }
        assert_eq!(
            store.pref::<Value>("views"),
            Some(json!({"/r/good": {"kind": "recent"}}))
        );
        // A theme from before named palettes: a bare mode.
        assert_eq!(
            theme_pref("light"),
            Some(ThemePref {
                mode: "light".into(),
                ..ThemePref::default()
            })
        );
        assert_eq!(theme_pref(r#"{"mode":"sepia"}"#), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_store_that_cannot_be_read_is_skipped_and_tried_again() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = scratch("upgrade-unreadable");
        let data = dir.join("data");
        let webkit = dir.join("WebKit");

        // No Tauri app ever ran here: nothing to carry, and done.
        carry_over_from(&mut Store::at(data.clone()), &webkit, Vec::new);
        let store = Store::at(data.clone());
        assert!(store.flag(CARRIED_OVER));
        assert_eq!(store.pref::<Value>("diff.layout"), None);
        std::fs::remove_dir_all(&data).unwrap();

        let db = tauri_storage(&webkit, &[("jiji.diff.layout", "split")]);
        let good = std::fs::read(&db).unwrap();
        let originals = |db: &Path| {
            let mut names: Vec<_> = std::fs::read_dir(db.parent().unwrap())
                .unwrap()
                .flatten()
                .map(|entry| entry.file_name())
                .collect();
            names.sort();
            names
        };
        let before = originals(&db);
        for how in ["permission", "damaged"] {
            match how {
                "permission" => {
                    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o000)).unwrap()
                }
                _ => std::fs::write(&db, b"SQLite format 3\0 and then nothing").unwrap(),
            }
            carry_over_from(&mut Store::at(data.clone()), &webkit, Vec::new);
            let store = Store::at(data.clone());
            assert!(!store.flag(CARRIED_OVER), "{how}");
            assert_eq!(store.pref::<Value>("diff.layout"), None, "{how}");
            std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        // Readable again: the next launch carries it over. WebKit's
        // directory holds what it held, and nothing more.
        std::fs::write(&db, &good).unwrap();
        carry_over_from(&mut Store::at(data.clone()), &webkit, Vec::new);
        let store = Store::at(data);
        assert!(store.flag(CARRIED_OVER));
        assert_eq!(
            store.pref::<String>("diff.layout").as_deref(),
            Some("split")
        );
        assert_eq!(originals(&db), before);
        assert_eq!(std::fs::read(&db).unwrap(), good);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_run_with_its_own_data_directory_never_looks_at_the_real_profile() {
        let home = || Some(OsString::from("/Users/someone"));
        assert_eq!(webkit_profile(Some("/tmp/jiji-test".into()), home()), None);
        assert_eq!(
            webkit_profile(None, home()),
            Some(PathBuf::from("/Users/someone/Library/WebKit/com.lauf.jiji"))
        );
        assert_eq!(webkit_profile(None, None), None);
    }

    #[test]
    fn the_window_comes_over_at_its_display_s_scale() {
        let record = |value: Value| serde_json::from_value::<TauriWindows>(value).unwrap().main;
        let windowed = |x: f32, y: f32, width: f32, height: f32| {
            record(json!({"main": {
                "width": width, "height": height, "x": x, "y": y, "prev_x": 0, "prev_y": 0,
                "maximized": false, "fullscreen": false,
            }}))
        };
        let state = |display: &str, x, y, width, height| WindowState {
            display: Some(display.into()),
            x,
            y,
            width,
            height,
            zoomed: false,
            fullscreen: false,
        };
        assert_eq!(
            window_state(&record(zoomed_record()), &[retina()]),
            Some(zoomed_state())
        );

        // A laptop at twice its points, and to its right a display at its
        // own, sitting higher. A window there reads as a smaller one on
        // the laptop too, but only there is it a size the window can be.
        let laptop = GlobalScreen {
            uuid: Some("laptop".into()),
            frame: Bounds::new(point(px(0.), px(0.)), size(px(1512.), px(982.))),
            scale: 2.,
        };
        let side = GlobalScreen {
            uuid: Some("side".into()),
            frame: Bounds::new(point(px(1512.), px(-200.)), size(px(2560.), px(1440.))),
            scale: 1.,
        };
        let screens = [laptop.clone(), side];
        assert_eq!(
            window_state(&windowed(1700., 100., 1280., 820.), &screens),
            Some(state("side", 188., 300., 1280., 820.))
        );
        assert_eq!(
            window_state(&windowed(400., 200., 2800., 1600.), &screens),
            Some(state("laptop", 200., 100., 1400., 800.))
        );
        // On a display that is gone: centered on the primary.
        assert_eq!(
            window_state(&windowed(9000., 100., 2560., 1640.), &screens),
            Some(state("laptop", 116., 81., 1280., 820.))
        );
        // Full screen: the frame was the display's.
        let full = record(json!({"main": {
            "width": 5120, "height": 2880, "x": 0, "y": 0, "prev_x": 0, "prev_y": 0,
            "maximized": false, "fullscreen": true,
        }}));
        assert_eq!(
            window_state(&full, &[retina()]),
            Some(WindowState {
                fullscreen: true,
                ..state("retina", 640., 310., 1280., 820.)
            })
        );
        // The plugin's blank record, and no display to measure against.
        assert_eq!(window_state(&windowed(0., 0., 0., 0.), &screens), None);
        assert_eq!(window_state(&windowed(0., 0., 1280., 820.), &[]), None);
    }
}
