//! The preferences the app and the `jiji` command share.
//!
//! The app keeps what it remembers in `prefs.json`, in its data directory
//! (the desktop crate's `store.rs` owns the file and writes it). One
//! preference changes what a publish does, and a publish has to behave the
//! same from the app and from the command, so the command reads that one
//! from the same file. It only ever reads: the file is the app's.

use std::path::{Path, PathBuf};

/// The app's identifier, which names its data directory.
pub const APP_ID: &str = "com.lauf.jiji";

/// The key of the preference that makes a publish link a new stack as a
/// GitHub stack without being asked
/// ([`crate::submit::SubmitOptions::under_setting`]). Off unless set.
pub const LINK_NEW_STACKS: &str = "publish.linkNewStacks";

/// Where the app's files live. `JIJI_DATA_DIR` redirects it (tests,
/// scripted runs), so they never touch a real install's files.
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

/// Whether the preferences kept in `dir` ask for new stacks to be linked
/// as GitHub stacks. A file that is missing or cannot be read, and a value
/// that is anything but `true`, read as off: that is the default, and a
/// publish that was meant to link can still be told to.
pub fn link_new_stacks(dir: &Path) -> bool {
    std::fs::read(dir.join("prefs.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|prefs| prefs.get(LINK_NEW_STACKS)?.as_bool())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linking_new_stacks_is_off_unless_the_file_says_true() {
        let dir = tempfile::tempdir().unwrap();
        // No file at all: a machine that never ran the app.
        assert!(!link_new_stacks(dir.path()));
        let write = |text: &str| std::fs::write(dir.path().join("prefs.json"), text).unwrap();
        write(r#"{"publish.linkNewStacks": true, "theme": {"mode": "dark"}}"#);
        assert!(link_new_stacks(dir.path()));
        write(r#"{"publish.linkNewStacks": false}"#);
        assert!(!link_new_stacks(dir.path()));
        // Anything else is not a yes: another type, another key, a file
        // cut short.
        for text in [
            r#"{"publish.linkNewStacks": "true"}"#,
            r#"{"publish.linkNewStacks": 1}"#,
            r#"{"theme": {"mode": "dark"}}"#,
            r#"{"publish.linkNewStacks": true, "#,
            "",
        ] {
            write(text);
            assert!(!link_new_stacks(dir.path()), "{text}");
        }
    }
}
