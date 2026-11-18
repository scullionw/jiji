//! What the tests that drive the model or a window share.
//!
//! These run the real model and the real views on GPUI's test context:
//! nothing reaches the screen, and tasks run when a test lets them. Repos
//! are real ones made for the test, with no remotes, and the host finds no
//! forge token under test (`host::forge::resolved_token`), so nothing here
//! reads the user's credentials or reaches the network.

use std::path::{Path, PathBuf};

use gpui::{Entity, TestAppContext};

use crate::app::AppModel;
use crate::store::Store;

/// An empty directory of the test's own, under the system's temporary
/// folder.
pub fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("jiji-gpui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// The app as `main` sets it up — key bindings, actions, the model — with
/// its preferences and recents under `dir`.
pub fn install(dir: &Path, cx: &mut TestAppContext) -> Entity<AppModel> {
    cx.update(|cx| {
        crate::ui::tooltip::init(cx);
        crate::ui::input::bind_keys(cx);
        crate::shell::bind_keys(cx);
        crate::views::workbench::bind_keys(cx);
        crate::views::bind_keys(cx);
        crate::views::palette::bind_keys(cx);
        crate::views::publish::bind_keys(cx);
        crate::views::search::bind_keys(cx);
        crate::diff::view::bind_keys(cx);
        crate::shell::register_app_actions(cx);
        AppModel::install(Store::at(dir.join("data")), "Menlo".into(), cx)
    })
}

/// A plain Git repository at `dir/name`: what the app offers to set jj up
/// in. Returns its path as the app names it.
pub fn git_repo(dir: &Path, name: &str) -> String {
    let root = dir.join(name);
    let made = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .arg(&root)
        .status()
        .expect("git runs");
    assert!(made.success(), "git init {}", root.display());
    root.to_string_lossy().into_owned()
}

/// A jj repository at `dir/name`, colocated with Git and holding nothing
/// but its empty working copy.
pub fn jj_repo(dir: &Path, name: &str) -> String {
    use jiji_core::RepoBackend as _;
    let root = git_repo(dir, name);
    jiji_core::JjBackend::default()
        .init_colocated(Path::new(&root))
        .expect("jj sets up in a fresh Git repository");
    root
}
