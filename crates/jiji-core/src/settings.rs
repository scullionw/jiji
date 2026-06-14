//! Real jj user-config loading.
//!
//! The read-only milestones ran on jj-lib's built-in defaults; mutations need
//! the user's actual identity and `immutable_heads()`/`trunk()` aliases. This
//! module stacks config the way the jj CLI does — built-in defaults, an
//! environment base, user config files, then repo config — and exposes the
//! resulting `UserSettings` plus the revset aliases map parsed from it.
//!
//! Policy: broken config is a hard error, matching the CLI. A user whose
//! config does not load cannot run `jj` either, and silently falling back to
//! defaults would make Jiji disagree with the CLI about what is immutable.

use std::path::{Path, PathBuf};

use jj_lib::config::{ConfigLayer, ConfigSource, StackedConfig};
use jj_lib::revset::RevsetAliasesMap;
use jj_lib::settings::UserSettings;
use jj_lib::workspace::{DefaultWorkspaceLoaderFactory, WorkspaceLoaderFactory as _};

use crate::backend::BackendError;

/// Where user-level jj config comes from. Production uses `Discover`; tests
/// pin or disable it so they never absorb the developer's own `~/.config/jj`.
#[derive(Debug, Clone)]
pub enum UserConfigSource {
    /// Mirror the jj CLI: `$JJ_CONFIG` paths when set, otherwise the
    /// platform config directories.
    Discover,
    /// Exactly these files or directories.
    Paths(Vec<PathBuf>),
    /// No user-level config at all.
    None,
}

/// Defaults the jj CLI owns rather than jj-lib, so Jiji has to ship its own
/// copy: the revset aliases from `revsets.toml` and the working-copy snapshot
/// settings from `misc.toml`. One curated deviation: `trunk()` falls back to
/// a local main/master/trunk bookmark before `root()`, matching Jiji's
/// display trunk on repos with no remotes. User layers override any of these.
const DEFAULT_CONFIG: &str = r#"
[revset-aliases]
'trunk()' = '''
latest(
  remote_bookmarks(exact:"main", exact:"origin") |
  remote_bookmarks(exact:"master", exact:"origin") |
  remote_bookmarks(exact:"trunk", exact:"origin") |
  remote_bookmarks(exact:"main", exact:"upstream") |
  remote_bookmarks(exact:"master", exact:"upstream") |
  remote_bookmarks(exact:"trunk", exact:"upstream") |
  remote_bookmarks(exact:"main") |
  remote_bookmarks(exact:"master") |
  remote_bookmarks(exact:"trunk") |
  bookmarks(exact:"main") |
  bookmarks(exact:"master") |
  bookmarks(exact:"trunk") |
  root()
)
'''
'builtin_immutable_heads()' = 'present(trunk()) | tags() | untracked_remote_bookmarks()'
'immutable_heads()' = 'builtin_immutable_heads()'
'immutable()' = '::(immutable_heads() | root())'
'mutable()' = '~immutable()'

# Working-copy snapshot defaults, mirroring the jj CLI's misc.toml (these are
# CLI-owned settings; jj-lib ships no defaults for them). Mutations snapshot
# the working copy first, exactly like every CLI command.
[snapshot]
auto-track = 'all()'
max-new-file-size = '1MiB'
"#;

fn config_err(err: impl std::fmt::Display) -> BackendError {
    BackendError::ConfigInvalid(err.to_string())
}

/// Builds `UserSettings` for the workspace at `workspace_root`, layering
/// like the CLI: jj-lib defaults → Jiji's default aliases → environment base
/// (operation user/host) → user config → repo config → workspace config.
pub(crate) fn load_settings(
    workspace_root: &Path,
    user_config: &UserConfigSource,
) -> Result<UserSettings, BackendError> {
    let mut config = StackedConfig::with_defaults();
    config.add_layer(
        ConfigLayer::parse(ConfigSource::Default, DEFAULT_CONFIG)
            .expect("embedded default config must parse"),
    );
    config.add_layer(env_base_layer());

    for path in user_config_paths(user_config) {
        load_path(&mut config, ConfigSource::User, &path)?;
    }

    // `.jj/repo` may live outside the workspace (non-default workspaces);
    // the loader resolves it without needing settings first.
    let loader = DefaultWorkspaceLoaderFactory
        .create(workspace_root)
        .map_err(|err| BackendError::OpenFailed(err.to_string()))?;
    let scope_roots = scoped_config_roots(user_config);
    if let Some(path) = scoped_config_path(
        loader.repo_path(),
        "config-id",
        "config.toml",
        "repos",
        &scope_roots,
    ) {
        config.load_file(ConfigSource::Repo, &path).map_err(config_err)?;
    }
    if let Some(path) = scoped_config_path(
        &workspace_root.join(".jj"),
        "workspace-config-id",
        "workspace-config.toml",
        "workspaces",
        &scope_roots,
    ) {
        config
            .load_file(ConfigSource::Workspace, &path)
            .map_err(config_err)?;
    }

    UserSettings::from_config(config).map_err(config_err)
}

/// Resolves a repo- or workspace-scoped config file without jj's migration
/// side effects. jj ≥ 0.37 stores these outside the repo ("secure config"):
/// `<scope_dir>/<id_file>` holds an id naming `<user config dir>/<subdir>/
/// <id>/config.toml`. Older repos keep a legacy file inside `<scope_dir>`
/// (left as a symlink to the new location after jj migrates it).
fn scoped_config_path(
    scope_dir: &Path,
    id_file: &str,
    legacy_file: &str,
    subdir: &str,
    roots: &[PathBuf],
) -> Option<PathBuf> {
    if let Ok(id) = std::fs::read_to_string(scope_dir.join(id_file)) {
        let id = id.trim();
        if id.len() == 20 && id.chars().all(|c| c.is_ascii_hexdigit()) {
            for root in roots {
                let path = root.join(subdir).join(id).join("config.toml");
                if path.is_file() {
                    return Some(path);
                }
            }
        } else {
            tracing::warn!(scope = %scope_dir.display(), "invalid jj config id; ignoring");
        }
        return None;
    }
    let legacy = scope_dir.join(legacy_file);
    legacy.is_file().then_some(legacy)
}

/// Directories that may hold the externally-stored repo/workspace configs.
/// Only `Discover` consults the real platform dirs; pinned test
/// configurations must never read the developer's own machine state.
fn scoped_config_roots(source: &UserConfigSource) -> Vec<PathBuf> {
    match source {
        UserConfigSource::Discover => platform_config_dirs(),
        UserConfigSource::Paths(_) | UserConfigSource::None => Vec::new(),
    }
}

/// Values the CLI derives from the environment rather than config files:
/// the `user@host` recorded on operations.
fn env_base_layer() -> ConfigLayer {
    let mut text = String::new();
    let hostname = whoami::fallible::hostname().unwrap_or_default();
    if !hostname.is_empty() {
        text.push_str(&format!("operation.hostname = {:?}\n", hostname));
    }
    let username = whoami::fallible::username().unwrap_or_default();
    if !username.is_empty() {
        text.push_str(&format!("operation.username = {:?}\n", username));
    }
    ConfigLayer::parse(ConfigSource::EnvBase, &text).expect("environment base layer must parse")
}

fn user_config_paths(source: &UserConfigSource) -> Vec<PathBuf> {
    match source {
        UserConfigSource::None => Vec::new(),
        UserConfigSource::Paths(paths) => paths.clone(),
        UserConfigSource::Discover => discover_user_config_paths(),
    }
}

/// The jj CLI's lookup: `$JJ_CONFIG` (a path-separator-delimited list) wins
/// outright; otherwise every existing platform config file loads, lowest
/// precedence first.
fn discover_user_config_paths() -> Vec<PathBuf> {
    if let Some(value) = std::env::var_os("JJ_CONFIG") {
        return std::env::split_paths(&value)
            .filter(|p| !p.as_os_str().is_empty())
            .collect();
    }
    let mut paths = Vec::new();
    for dir in platform_config_dirs() {
        let file = dir.join("config.toml");
        if file.is_file() {
            paths.push(file);
        }
        let conf_d = dir.join("conf.d");
        if conf_d.is_dir() {
            paths.push(conf_d);
        }
    }
    paths
}

/// The platform's candidate `jj` config directories: XDG first, then the
/// macOS-native location, matching where the CLI reads and writes.
fn platform_config_dirs() -> Vec<PathBuf> {
    let home = std::env::home_dir();
    let mut dirs = Vec::new();
    let xdg_base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.as_ref().map(|h| h.join(".config")));
    if let Some(base) = xdg_base {
        dirs.push(base.join("jj"));
    }
    if cfg!(target_os = "macos") {
        if let Some(home) = &home {
            dirs.push(home.join("Library/Application Support/jj"));
        }
    }
    dirs
}

fn load_path(
    config: &mut StackedConfig,
    source: ConfigSource,
    path: &Path,
) -> Result<(), BackendError> {
    if path.is_dir() {
        config.load_dir(source, path).map_err(config_err)
    } else if path.is_file() {
        config.load_file(source, path).map_err(config_err)
    } else {
        // An explicitly-configured path that does not exist is skipped, not
        // fatal: the app should still open repos if a machine lost one of
        // its config files.
        tracing::warn!(path = %path.display(), "configured jj config path does not exist");
        Ok(())
    }
}

/// All `revset-aliases` across layers, later layers overriding earlier ones.
/// Invalid alias *declarations* are skipped with a warning (like the CLI);
/// invalid *definitions* surface when the alias is actually parsed.
pub(crate) fn revset_aliases(settings: &UserSettings) -> RevsetAliasesMap {
    let mut aliases = RevsetAliasesMap::new();
    for layer in settings.config().layers() {
        let table = match layer.look_up_table("revset-aliases") {
            Ok(Some(table)) => table,
            Ok(None) => continue,
            Err(item) => {
                tracing::warn!(?item, "revset-aliases is not a table; ignoring layer");
                continue;
            }
        };
        for (decl, item) in table.iter() {
            let result = item
                .as_str()
                .ok_or_else(|| format!("alias {decl} must be a string"))
                .and_then(|defn| {
                    aliases
                        .insert(decl, defn)
                        .map_err(|err| err.to_string())
                });
            if let Err(reason) = result {
                tracing::warn!(decl, reason, "skipping unloadable revset alias");
            }
        }
    }
    aliases
}
