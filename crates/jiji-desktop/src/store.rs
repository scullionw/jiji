//! Everything Jiji remembers between launches, as small JSON files in the
//! app data directory.
//!
//! `recent-repos.json`, `saved-views.json`, `license.json` and
//! `autoland.json` keep the names and shapes Jiji has written since its
//! first release, so an install from then keeps its recents, saved views,
//! registration and interrupted auto-land jobs. `prefs.json` holds the
//! rest (tabs, theme, pane widths, per-repo view lenses, the window); the
//! Tauri app kept those in its web view, and `upgrade.rs` carries them
//! over once.
//!
//! A file that is there but cannot be read is never written over: what it
//! holds is unknown, not gone. Lists decode entry by entry, so one bad
//! entry costs only itself.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use jiji_core::snapshot::SnapshotView;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub(crate) const APP_ID: &str = "com.lauf.jiji";
const MAX_RECENT: usize = 8;

/// Where the store lives. `JIJI_DATA_DIR` redirects it (tests, scripted
/// runs) so they never touch a real install's files.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("JIJI_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support").join(APP_ID)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join(APP_ID)
    }
}

/// One JSON object on disk, written whole and atomically on every change.
struct JsonFile {
    path: PathBuf,
    map: Map<String, Value>,
    /// The file is there but could not be read or decoded. It may hold
    /// what a later read finds, so it is not written over until a read
    /// succeeds; changes made meanwhile live in memory only.
    unread: bool,
}

impl JsonFile {
    fn load(path: PathBuf) -> Self {
        let mut file = Self {
            path,
            map: Map::new(),
            unread: false,
        };
        file.reload();
        file
    }

    /// Read the file again; false while it is there but still unreadable.
    fn reload(&mut self) -> bool {
        let read = match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|err| err.to_string()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
            Err(err) => Err(err.to_string()),
        };
        match read {
            Ok(map) => {
                self.map = map;
                self.unread = false;
            }
            Err(err) => {
                if !self.unread {
                    log::warn!("could not read {}: {err}", self.path.display());
                }
                self.unread = true;
            }
        }
        !self.unread
    }

    fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.map
            .get(key)
            .and_then(|value| serde_json::from_value(value.clone()).ok())
    }

    /// The entries of a list that decode, in order.
    fn get_each<T: DeserializeOwned>(&self, key: &str) -> Vec<T> {
        match self.map.get(key) {
            Some(Value::Array(entries)) => entries
                .iter()
                .filter_map(|entry| serde_json::from_value(entry.clone()).ok())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Whether a change may go to disk: always, unless the file could not
    /// be read and still cannot.
    fn writable(&mut self) -> bool {
        !self.unread || self.reload()
    }

    fn set<T: Serialize>(&mut self, key: &str, value: &T) {
        let value = match serde_json::to_value(value) {
            Ok(value) => value,
            Err(err) => {
                log::warn!("could not encode {key}: {err}");
                return;
            }
        };
        let writable = self.writable();
        if self.map.get(key) == Some(&value) {
            return;
        }
        self.map.insert(key.to_owned(), value);
        if writable {
            self.save();
        }
    }

    fn remove(&mut self, key: &str) {
        let writable = self.writable();
        if self.map.remove(key).is_some() && writable {
            self.save();
        }
    }

    fn save(&self) {
        if let Err(err) = write_atomic(&self.path, &self.map) {
            log::warn!("could not save {}: {err}", self.path.display());
        }
    }
}

fn write_atomic(path: &Path, map: &Map<String, Value>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let staged = path.with_extension("json.tmp");
    std::fs::write(&staged, serde_json::to_vec_pretty(map)?)?;
    std::fs::rename(&staged, path)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentRepo {
    pub path: String,
    pub name: String,
    pub last_opened_at: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PersistedRepo {
    pub path: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PersistedTabs {
    pub repos: Vec<PersistedRepo>,
    pub active: usize,
}

impl PersistedTabs {
    /// The saved tabs, entry by entry: an entry without a usable path
    /// drops alone, as does a later copy of a path already restored, and
    /// the active tab is found again among what is kept.
    pub(crate) fn decode(value: Option<&Value>) -> Self {
        let tabs = value.and_then(Value::as_object);
        let field = |name| tabs.and_then(|tabs| tabs.get(name));
        let active = field("active")
            .and_then(Value::as_u64)
            .map_or(0, |active| active as usize);
        let entries = field("repos")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        let mut repos: Vec<PersistedRepo> = Vec::new();
        let mut active_at = None;
        for (ix, entry) in entries.iter().enumerate() {
            let path = entry
                .get("path")
                .and_then(Value::as_str)
                .filter(|path| !path.trim().is_empty());
            let at = path.map(|path| {
                repos
                    .iter()
                    .position(|repo| repo.path == path)
                    .unwrap_or_else(|| {
                        repos.push(PersistedRepo {
                            path: path.to_owned(),
                            // The repo names itself once it opens.
                            name: entry
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                        });
                        repos.len() - 1
                    })
            });
            if ix == active {
                // A dropped active entry hands over to the next one kept.
                active_at = Some(at.unwrap_or(repos.len()));
            }
        }
        Self {
            active: active_at.unwrap_or(active),
            repos,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThemePref {
    /// "system", "light" or "dark".
    pub mode: String,
    pub dark: String,
    pub light: String,
}

impl Default for ThemePref {
    fn default() -> Self {
        Self {
            mode: "system".into(),
            dark: crate::theme::DEFAULT_DARK.into(),
            light: crate::theme::DEFAULT_LIGHT.into(),
        }
    }
}

/// A named revset the view picker lists for one repo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedView {
    pub name: String,
    pub revset: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LicenseRecord {
    pub key: String,
    pub activation_id: String,
    pub device_id: String,
    pub status: String,
    /// "solo", "personal" or "unknown".
    pub plan: String,
    pub limit_activations: Option<u64>,
    pub expires_at: Option<String>,
    pub registered_at: u64,
    pub last_validated_at: u64,
}

/// Where the main window was left. The frame is the window's own, from
/// the top-left of the display it was on: where it sits when neither
/// zoomed nor full screen, so a window that comes back zoomed unzooms to
/// it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    /// The display, by its UUID. A state saved before this was kept has
    /// none, and meant the primary display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    #[serde(default)]
    pub zoomed: bool,
    #[serde(default)]
    pub fullscreen: bool,
}

/// The licence file, or the record in it, could not be read: what it holds
/// is unknown, not absent.
#[derive(Debug, PartialEq)]
pub struct Unreadable;

/// Set beside the preferences while a licence record is stored: what a
/// licence file that cannot be read falls back on.
pub(crate) const REGISTERED: &str = "license.registered";

/// The per-repo lenses: one map, a repo path to the lens it was left on.
const VIEWS: &str = "views";

pub struct Store {
    dir: PathBuf,
    prefs: JsonFile,
    recents: JsonFile,
    saved_views: JsonFile,
    license: JsonFile,
}

impl Store {
    pub fn open() -> Self {
        Self::at(data_dir())
    }

    pub fn at(dir: PathBuf) -> Self {
        Self {
            prefs: JsonFile::load(dir.join("prefs.json")),
            recents: JsonFile::load(dir.join("recent-repos.json")),
            saved_views: JsonFile::load(dir.join("saved-views.json")),
            license: JsonFile::load(dir.join("license.json")),
            dir,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    // ── Free-form preferences ──

    pub fn pref<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.prefs.get(key)
    }

    pub fn set_pref<T: Serialize>(&mut self, key: &str, value: &T) {
        self.prefs.set(key, value);
    }

    pub fn clear_pref(&mut self, key: &str) {
        self.prefs.remove(key);
    }

    pub fn flag(&self, key: &str) -> bool {
        self.pref::<bool>(key).unwrap_or(false)
    }

    /// Take in what an earlier install left, in one write. A preference
    /// set here already keeps its value, and so does a repo's lens.
    pub fn adopt(&mut self, prefs: Map<String, Value>, lenses: Map<String, Value>) {
        let file = &mut self.prefs;
        let writable = file.writable();
        let mut changed = false;
        for (key, value) in prefs {
            if !file.map.contains_key(&key) {
                file.map.insert(key, value);
                changed = true;
            }
        }
        if !lenses.is_empty() {
            let views = file
                .map
                .entry(VIEWS)
                .or_insert_with(|| Value::Object(Map::new()));
            if let Value::Object(views) = views {
                for (repo, lens) in lenses {
                    if !views.contains_key(&repo) {
                        views.insert(repo, lens);
                        changed = true;
                    }
                }
            }
        }
        if changed && writable {
            file.save();
        }
    }

    // ── Tabs, theme, window ──

    pub fn tabs(&self) -> PersistedTabs {
        PersistedTabs::decode(self.prefs.map.get("tabs"))
    }

    pub fn set_tabs(&mut self, tabs: &PersistedTabs) {
        self.set_pref("tabs", tabs);
    }

    pub fn theme(&self) -> ThemePref {
        self.pref("theme").unwrap_or_default()
    }

    pub fn set_theme(&mut self, pref: &ThemePref) {
        self.set_pref("theme", pref);
    }

    pub fn window(&self) -> Option<WindowState> {
        self.pref("window")
    }

    pub fn set_window(&mut self, state: &WindowState) {
        self.set_pref("window", state);
    }

    /// The view lens a repo was last left on.
    pub fn repo_view(&self, repo_path: &str) -> SnapshotView {
        self.pref::<HashMap<String, SnapshotView>>(VIEWS)
            .and_then(|mut views| views.remove(repo_path))
            .unwrap_or_default()
    }

    pub fn set_repo_view(&mut self, repo_path: &str, view: &SnapshotView) {
        let mut views = self
            .pref::<HashMap<String, SnapshotView>>(VIEWS)
            .unwrap_or_default();
        if *view == SnapshotView::Workstreams {
            views.remove(repo_path);
        } else {
            views.insert(repo_path.to_owned(), view.clone());
        }
        self.set_pref(VIEWS, &views);
    }

    // ── Recent repositories ──

    pub fn recent_repos(&self) -> Vec<RecentRepo> {
        self.recents.get("repos").unwrap_or_default()
    }

    pub fn remember_repo(&mut self, path: &str, name: &str) -> Vec<RecentRepo> {
        let mut next = vec![RecentRepo {
            path: path.to_owned(),
            name: name.to_owned(),
            last_opened_at: now_ms(),
        }];
        next.extend(
            self.recent_repos()
                .into_iter()
                .filter(|repo| repo.path != path),
        );
        next.truncate(MAX_RECENT);
        self.recents.set("repos", &next);
        next
    }

    pub fn forget_repo(&mut self, path: &str) -> Vec<RecentRepo> {
        let next: Vec<RecentRepo> = self
            .recent_repos()
            .into_iter()
            .filter(|repo| repo.path != path)
            .collect();
        self.recents.set("repos", &next);
        next
    }

    // ── Saved revset views, per repo ──

    /// A repo's saved views, trimmed, without blanks or later copies of a
    /// name.
    pub fn saved_views(&self, repo_path: &str) -> Vec<SavedView> {
        let mut views: Vec<SavedView> = Vec::new();
        for view in self.saved_views.get_each::<SavedView>(repo_path) {
            let (name, revset) = (view.name.trim(), view.revset.trim());
            if !name.is_empty() && !revset.is_empty() && !views.iter().any(|v| v.name == name) {
                views.push(SavedView {
                    name: name.to_owned(),
                    revset: revset.to_owned(),
                });
            }
        }
        views
    }

    pub fn set_saved_views(&mut self, repo_path: &str, views: &[SavedView]) {
        if views.is_empty() {
            self.saved_views.remove(repo_path);
        } else {
            self.saved_views.set(repo_path, &views);
        }
    }

    // ── License ──

    /// The licence record, as the file says it now.
    pub fn license(&mut self) -> Result<Option<LicenseRecord>, Unreadable> {
        if !self.license.reload() {
            return Err(Unreadable);
        }
        let record = match self.license.map.get("state") {
            Some(value) => Some(serde_json::from_value(value.clone()).map_err(|_| Unreadable)?),
            None => None,
        };
        self.prefs.set(REGISTERED, &record.is_some());
        Ok(record)
    }

    /// Whether a licence record was stored when the file was last read.
    pub fn was_registered(&self) -> bool {
        self.flag(REGISTERED)
    }

    pub fn set_license(&mut self, record: &LicenseRecord) {
        self.license.set("state", record);
        self.prefs.set(REGISTERED, &true);
    }

    pub fn clear_license(&mut self) {
        self.license.remove("state");
        self.prefs.set(REGISTERED, &false);
    }

    /// A stable, random per-install id used as the activation device. It
    /// lives in the licence file: while that cannot be read there is none,
    /// rather than a fresh one that would spend another device slot.
    pub fn device_id(&mut self) -> Result<String, Unreadable> {
        if !self.license.writable() {
            return Err(Unreadable);
        }
        if let Some(id) = self.license.get::<String>("deviceId") {
            return Ok(id);
        }
        let id = random_uuid();
        self.license.set("deviceId", &id);
        Ok(id)
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// A version-4 UUID from the system's random source.
fn random_uuid() -> String {
    let mut bytes = [0u8; 16];
    let filled = std::fs::File::open("/dev/urandom")
        .and_then(|mut file| std::io::Read::read_exact(&mut file, &mut bytes))
        .is_ok();
    if !filled {
        // No random device: the clock and the process id still tell
        // installs apart well enough for an activation label.
        let seed = (now_ms() as u128) << 64 | (std::process::id() as u128) << 16;
        bytes = seed.to_le_bytes();
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jiji-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn recents_move_a_reopened_repo_to_the_front_and_cap() {
        let dir = scratch("recents");
        let mut store = Store::at(dir.clone());
        for i in 0..10 {
            store.remember_repo(&format!("/r/{i}"), &format!("r{i}"));
        }
        let listed = store.remember_repo("/r/5", "r5");
        assert_eq!(listed.len(), MAX_RECENT);
        assert_eq!(listed[0].path, "/r/5");
        assert_eq!(listed.iter().filter(|r| r.path == "/r/5").count(), 1);
        // Reloading reads the same list back.
        assert_eq!(Store::at(dir.clone()).recent_repos(), listed);
        assert!(!store.forget_repo("/r/5").iter().any(|r| r.path == "/r/5"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_default_lens_is_not_stored() {
        let dir = scratch("views");
        let mut store = Store::at(dir.clone());
        store.set_repo_view("/r", &SnapshotView::Trunk);
        assert_eq!(store.repo_view("/r"), SnapshotView::Trunk);
        store.set_repo_view("/r", &SnapshotView::Workstreams);
        assert_eq!(store.repo_view("/r"), SnapshotView::Workstreams);
        assert_eq!(
            store.pref::<HashMap<String, SnapshotView>>("views"),
            Some(HashMap::new())
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn device_id_is_stable_and_uuid_shaped() {
        let dir = scratch("device");
        let mut store = Store::at(dir.clone());
        let id = store.device_id().unwrap();
        assert_eq!(id.len(), 36);
        assert_eq!(id.as_bytes()[14], b'4');
        assert_eq!(Store::at(dir.clone()).device_id(), Ok(id));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn view(name: &str, revset: &str) -> SavedView {
        SavedView {
            name: name.into(),
            revset: revset.into(),
        }
    }

    #[test]
    fn a_bad_saved_view_costs_only_itself() {
        let dir = scratch("views-partial");
        std::fs::create_dir_all(&dir).unwrap();
        let saved = serde_json::json!({
            "/r": [
                {"name": "WIP", "revset": "description(wip)"},
                "not an object",
                {"name": "no revset"},
                {"name": 7, "revset": "all()"},
                {"name": "  ", "revset": "all()"},
                {"name": "blank revset", "revset": "  "},
                {"name": " Mine ", "revset": " mine() "},
                {"name": "WIP", "revset": "all()"},
            ],
            "/not-a-list": {"name": "x", "revset": "y"},
        });
        std::fs::write(dir.join("saved-views.json"), saved.to_string()).unwrap();
        let store = Store::at(dir.clone());
        assert_eq!(
            store.saved_views("/r"),
            [view("WIP", "description(wip)"), view("Mine", "mine()")]
        );
        assert_eq!(store.saved_views("/not-a-list"), []);
        assert_eq!(store.saved_views("/missing"), []);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn tabs_from(tabs: serde_json::Value) -> PersistedTabs {
        let dir = scratch("tabs-partial");
        std::fs::create_dir_all(&dir).unwrap();
        let prefs = serde_json::json!({ "tabs": tabs });
        std::fs::write(dir.join("prefs.json"), prefs.to_string()).unwrap();
        let tabs = Store::at(dir.clone()).tabs();
        let _ = std::fs::remove_dir_all(dir);
        tabs
    }

    fn paths(tabs: &PersistedTabs) -> Vec<&str> {
        tabs.repos.iter().map(|repo| repo.path.as_str()).collect()
    }

    #[test]
    fn a_bad_tab_costs_only_itself_and_the_active_one_stays_active() {
        let tabs = tabs_from(serde_json::json!({
            "repos": [
                {"path": "/a", "name": "a"},
                {"name": "no path"},
                {"path": "/b", "name": "b"},
                {"path": "/a", "name": "a again"},
                {"path": "  "},
                {"path": "/c", "name": 5},
            ],
            "active": 2,
        }));
        assert_eq!(paths(&tabs), ["/a", "/b", "/c"]);
        assert_eq!(tabs.active, 1);
        // The name only labels the tab until the repo names itself.
        assert_eq!(tabs.repos[2].name, "");

        // A later copy of a path that was active: its first copy is.
        let copied = tabs_from(serde_json::json!({
            "repos": [{"path": "/a"}, {"path": "/b"}, {"path": "/a"}],
            "active": 2,
        }));
        assert_eq!((paths(&copied), copied.active), (vec!["/a", "/b"], 0));
        // An active entry that is dropped hands over to the next one kept.
        let dropped = tabs_from(serde_json::json!({
            "repos": [{"path": "/a"}, {"path": 7}, {"path": "/b"}],
            "active": 1,
        }));
        assert_eq!((paths(&dropped), dropped.active), (vec!["/a", "/b"], 1));
        // Nothing usable at all.
        assert_eq!(
            tabs_from(serde_json::json!("tabs")),
            PersistedTabs::default()
        );
        assert_eq!(
            tabs_from(serde_json::json!({"repos": {"path": "/a"}, "active": "1"})),
            PersistedTabs::default()
        );
    }

    #[test]
    fn a_file_that_cannot_be_read_is_never_written_over() {
        let dir = scratch("unread");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("prefs.json");
        std::fs::write(
            &file,
            r#"{"theme": {"mode": "dark", "dark": "a", "light": "b"}, "#,
        )
        .unwrap();
        let mut store = Store::at(dir.clone());
        assert_eq!(store.theme(), ThemePref::default());
        // The session goes on in memory; the file keeps what it holds.
        store.set_pref("diff.layout", &"split");
        assert_eq!(
            store.pref::<String>("diff.layout").as_deref(),
            Some("split")
        );
        assert!(std::fs::read_to_string(&file).unwrap().ends_with(", "));

        // Once it reads again, a change lands on what it holds.
        std::fs::write(
            &file,
            r#"{"theme": {"mode": "dark", "dark": "a", "light": "b"}}"#,
        )
        .unwrap();
        store.set_pref("graph.collapsed", &true);
        let reread = Store::at(dir.clone());
        assert_eq!(reread.theme().mode, "dark");
        assert!(reread.flag("graph.collapsed"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
