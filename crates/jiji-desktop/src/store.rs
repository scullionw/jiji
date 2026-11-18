//! Everything Jiji remembers between launches, as small JSON files in the
//! app data directory.
//!
//! `recent-repos.json`, `saved-views.json`, `license.json` and
//! `autoland.json` keep the names and shapes Jiji has written since its
//! first release, so an install from then keeps its recents, saved views,
//! registration and interrupted auto-land jobs. `prefs.json` holds the
//! rest (tabs, theme, pane widths, per-repo view lenses).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use jiji_core::snapshot::SnapshotView;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

const APP_ID: &str = "com.lauf.jiji";
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
}

impl JsonFile {
    fn load(path: PathBuf) -> Self {
        let map = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Map<String, Value>>(&bytes).ok())
            .unwrap_or_default();
        Self { path, map }
    }

    fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.map
            .get(key)
            .and_then(|value| serde_json::from_value(value.clone()).ok())
    }

    fn set<T: Serialize>(&mut self, key: &str, value: &T) {
        match serde_json::to_value(value) {
            Ok(value) => {
                if self.map.get(key) == Some(&value) {
                    return;
                }
                self.map.insert(key.to_owned(), value);
            }
            Err(err) => {
                log::warn!("could not encode {key}: {err}");
                return;
            }
        }
        self.save();
    }

    fn remove(&mut self, key: &str) {
        if self.map.remove(key).is_some() {
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedRepo {
    pub path: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PersistedTabs {
    #[serde(default)]
    pub repos: Vec<PersistedRepo>,
    #[serde(default)]
    pub active: usize,
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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    #[serde(default)]
    pub fullscreen: bool,
}

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

    // ── Tabs, theme, window ──

    pub fn tabs(&self) -> PersistedTabs {
        self.pref("tabs").unwrap_or_default()
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

    pub fn window(&self) -> Option<WindowGeometry> {
        self.pref("window")
    }

    pub fn set_window(&mut self, geometry: &WindowGeometry) {
        self.set_pref("window", geometry);
    }

    /// The view lens a repo was last left on.
    pub fn repo_view(&self, repo_path: &str) -> SnapshotView {
        self.pref::<HashMap<String, SnapshotView>>("views")
            .and_then(|mut views| views.remove(repo_path))
            .unwrap_or_default()
    }

    pub fn set_repo_view(&mut self, repo_path: &str, view: &SnapshotView) {
        let mut views = self
            .pref::<HashMap<String, SnapshotView>>("views")
            .unwrap_or_default();
        if *view == SnapshotView::Workstreams {
            views.remove(repo_path);
        } else {
            views.insert(repo_path.to_owned(), view.clone());
        }
        self.set_pref("views", &views);
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

    pub fn saved_views(&self, repo_path: &str) -> Vec<SavedView> {
        self.saved_views.get(repo_path).unwrap_or_default()
    }

    pub fn set_saved_views(&mut self, repo_path: &str, views: &[SavedView]) {
        if views.is_empty() {
            self.saved_views.remove(repo_path);
        } else {
            self.saved_views.set(repo_path, &views);
        }
    }

    // ── License ──

    pub fn license(&self) -> Option<LicenseRecord> {
        self.license.get("state")
    }

    pub fn set_license(&mut self, record: &LicenseRecord) {
        self.license.set("state", record);
    }

    pub fn clear_license(&mut self) {
        self.license.remove("state");
    }

    /// A stable, random per-install id used as the activation device.
    pub fn device_id(&mut self) -> String {
        if let Some(id) = self.license.get::<String>("deviceId") {
            return id;
        }
        let id = random_uuid();
        self.license.set("deviceId", &id);
        id
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
        let id = store.device_id();
        assert_eq!(id.len(), 36);
        assert_eq!(id.as_bytes()[14], b'4');
        assert_eq!(Store::at(dir.clone()).device_id(), id);
        let _ = std::fs::remove_dir_all(dir);
    }
}
