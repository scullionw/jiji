//! Desktop integrations: which editor CLIs exist on this machine,
//! launching one on a file (optionally at a line), and opening a directory
//! in the user's terminal.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub struct AppInfo {
    pub id: &'static str,
    pub label: &'static str,
}

/// (id, binary on PATH, label). Order is the default preference order.
const EDITORS: &[(&str, &str, &str)] = &[
    ("zed", "zed", "Zed"),
    ("vscode", "code", "VS Code"),
    ("cursor", "cursor", "Cursor"),
    ("sublime", "subl", "Sublime Text"),
];

// GUI-launched apps see launchd's minimal PATH, not the login shell's;
// editor shims live in Homebrew's bin, /usr/local/bin, or ~/.local/bin, so
// those are searched explicitly.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    for extra in ["/opt/homebrew/bin", "/usr/local/bin"] {
        let dir = PathBuf::from(extra);
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join(".local/bin"));
    }
    dirs
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    path.metadata()
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn find_binary(name: &str) -> Option<PathBuf> {
    search_dirs()
        .into_iter()
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

/// Editor CLIs present on this machine, in preference order.
pub fn detect_editors() -> Vec<AppInfo> {
    EDITORS
        .iter()
        .filter(|(_, binary, _)| find_binary(binary).is_some())
        .map(|&(id, _, label)| AppInfo { id, label })
        .collect()
}

/// Open `path` (a file or directory) in the given editor, jumping to
/// `line` when the editor CLI supports it.
pub fn open_in_editor(editor: &str, path: &str, line: Option<u32>) -> Result<(), String> {
    let (id, binary, label) = EDITORS
        .iter()
        .find(|(id, ..)| *id == editor)
        .ok_or_else(|| format!("unknown editor '{editor}'"))?;
    let program =
        find_binary(binary).ok_or_else(|| format!("{label} command-line tool not found"))?;
    let mut cmd = Command::new(program);
    match line {
        // zed and subl take path:line directly; code and cursor need -g.
        Some(line) => {
            if matches!(*id, "vscode" | "cursor") {
                cmd.arg("-g");
            }
            cmd.arg(format!("{path}:{line}"));
        }
        None => {
            cmd.arg(path);
        }
    }
    cmd.spawn()
        .map_err(|err| format!("failed to launch {label}: {err}"))?;
    Ok(())
}

/// (id, app bundle, label). Bundle names are what `open -a` addresses.
const TERMINALS: &[(&str, &str, &str)] = &[
    ("terminal", "Terminal", "Terminal"),
    ("iterm", "iTerm", "iTerm"),
    ("ghostty", "Ghostty", "Ghostty"),
    ("wezterm", "WezTerm", "WezTerm"),
    ("kitty", "kitty", "kitty"),
    ("alacritty", "Alacritty", "Alacritty"),
];

#[cfg(target_os = "macos")]
fn terminal_app_exists(bundle: &str) -> bool {
    let name = format!("{bundle}.app");
    let mut dirs = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/Applications/Utilities"),
        PathBuf::from("/System/Applications/Utilities"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join("Applications"));
    }
    dirs.iter().any(|dir| dir.join(&name).exists())
}

/// Terminal apps present on this machine, in preference order.
pub fn detect_terminals() -> Vec<AppInfo> {
    #[cfg(target_os = "macos")]
    {
        TERMINALS
            .iter()
            .filter(|(_, bundle, _)| terminal_app_exists(bundle))
            .map(|&(id, _, label)| AppInfo { id, label })
            .collect()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

/// Open a directory in the given terminal app (None: the platform's own).
pub fn open_in_terminal(path: &str, terminal: Option<&str>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let id = terminal.unwrap_or("terminal");
        let (id, bundle, label) = TERMINALS
            .iter()
            .find(|(tid, ..)| *tid == id)
            .ok_or_else(|| format!("unknown terminal '{id}'"))?;
        let mut cmd = Command::new("open");
        match *id {
            // Ghostty, WezTerm, kitty, and Alacritty are not document-based:
            // a dropped folder does nothing, but each takes a directory
            // flag. -n forces a fresh instance so --args reaches it while
            // the app is already running.
            "ghostty" => {
                cmd.args(["-na", bundle, "--args"])
                    .arg(format!("--working-directory={path}"));
            }
            "wezterm" => {
                cmd.args(["-na", bundle, "--args", "start", "--cwd"])
                    .arg(path);
            }
            "kitty" => {
                cmd.args(["-na", bundle, "--args", "--directory"]).arg(path);
            }
            "alacritty" => {
                cmd.args(["-na", bundle, "--args", "--working-directory"])
                    .arg(path);
            }
            // Terminal and iTerm open a folder like a document.
            _ => {
                cmd.args(["-a", bundle]).arg(path);
            }
        }
        cmd.spawn()
            .map_err(|err| format!("failed to open {label}: {err}"))?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = terminal;
        Command::new("x-terminal-emulator")
            .current_dir(path)
            .spawn()
            .map_err(|err| format!("failed to open terminal: {err}"))?;
        Ok(())
    }
}
