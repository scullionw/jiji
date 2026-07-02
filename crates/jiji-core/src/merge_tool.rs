//! External 3-way merge tool selection and invocation.
//!
//! Merge-tool configuration is CLI-owned (`ui.merge-editor`, the
//! `[merge-tools.<name>]` tables); jj-lib knows nothing about it. This
//! module mirrors the CLI's semantics, pinned against jj 0.41's
//! `merge_tools/external.rs`: the same config shapes, the same
//! `$left`/`$base`/`$right`/`$output`/`$marker_length`/`$path` variables,
//! the same exit-code and output-file handling.
//!
//! One curated deviation, because Jiji cannot host jj's builtin TUI merge
//! editor: when `ui.merge-editor` is unset (the CLI would fall back to
//! `:builtin`) Jiji prefers Sublime Merge when it can be found — the
//! product's stated default resolve target — and otherwise reports that no
//! tool is available instead of silently picking something. An explicitly
//! configured `:builtin`/`:ours`/`:theirs` is refused with the same
//! explanation rather than reinterpreted.

use std::path::{Path, PathBuf};

use jj_lib::conflicts::ConflictMarkerStyle;
use jj_lib::config::{ConfigGetResultExt as _, ConfigNamePathBuf};
use jj_lib::settings::UserSettings;

use crate::backend::BackendError;

/// Everything needed to run one configured merge tool. `name` is what the
/// UI shows ("smerge", "meld", …): the `merge-tools` table key, or the
/// program name for inline command forms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MergeTool {
    pub name: String,
    pub program: String,
    pub merge_args: Vec<String>,
    /// The `$output` file starts as the materialized conflict (markers
    /// included) and remaining markers are parsed back after the tool
    /// exits, instead of starting empty and being taken verbatim.
    pub edits_conflict_markers: bool,
    /// Exit codes that mean "finished, but left conflict markers in the
    /// output" rather than "aborted" (mergiraf's contract).
    pub conflict_exit_codes: Vec<i32>,
    /// Overrides `ui.conflict-marker-style` when materializing for this
    /// tool.
    pub marker_style: Option<ConflictMarkerStyle>,
}

/// The `[merge-tools.<name>]` table shape, matching the CLI's
/// `ExternalMergeTool` for the fields the resolve flow uses (unknown fields
/// like `diff-args` are ignored here, not rejected).
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct MergeToolTable {
    program: String,
    merge_args: Vec<String>,
    merge_conflict_exit_codes: Vec<i32>,
    merge_tool_edits_conflict_markers: bool,
    conflict_marker_style: Option<ConflictMarkerStyle>,
}

fn config_err(err: impl std::fmt::Display) -> BackendError {
    BackendError::ConfigInvalid(err.to_string())
}

const NO_TOOL_HELP: &str = "No 3-way merge tool is available. Install Sublime Merge, or set \
     `ui.merge-editor` in your jj config (for example `ui.merge-editor = \"meld\"`)";

/// Resolves the merge tool `Resolve` will run, like the CLI resolves
/// `ui.merge-editor` for `jj resolve`. Errors are plain-language: broken
/// config is `ConfigInvalid`; "nothing usable is configured" explains what
/// to do about it.
pub(crate) fn resolve_merge_tool(settings: &UserSettings) -> Result<MergeTool, BackendError> {
    let value = settings
        .get_value("ui.merge-editor")
        .optional()
        .map_err(config_err)?;
    let Some(value) = value else {
        return default_tool(settings);
    };
    if let Some(text) = value.as_str() {
        if let Some(builtin) = text.strip_prefix(':') {
            return Err(BackendError::MutationFailed(format!(
                "ui.merge-editor is set to \u{201c}:{builtin}\u{201d}, a built-in jj CLI tool \
                 Jiji cannot run. Set it to an external tool (for example \"smerge\" or \
                 \"meld\") to resolve from Jiji"
            )));
        }
        // Like the CLI: the whole string is tried as a table name first, and
        // only then read as a program-plus-arguments command line.
        if let Some(tool) = tool_by_name(settings, text)? {
            return Ok(tool);
        }
        let words: Vec<String> =
            text.split(' ').filter(|w| !w.is_empty()).map(str::to_owned).collect();
        return tool_from_command(&words);
    }
    if let Some(array) = value.as_array() {
        let words: Vec<String> = array
            .iter()
            .map(|item| {
                item.as_str().map(str::to_owned).ok_or_else(|| {
                    config_err("ui.merge-editor: expected a string or an array of strings")
                })
            })
            .collect::<Result<_, _>>()?;
        return tool_from_command(&words);
    }
    Err(config_err(
        "ui.merge-editor: expected a string or an array of strings",
    ))
}

/// The tool name the snapshot advertises for Resolve affordances, or `None`
/// when resolving would fail before launching anything. Never an error: a
/// snapshot must not fail because the merge-tool config is broken — the
/// mutation path reports that properly when Resolve is actually invoked.
pub(crate) fn available_tool_name(settings: &UserSettings) -> Option<String> {
    match resolve_merge_tool(settings) {
        Ok(tool) => Some(tool.name),
        Err(err) => {
            tracing::debug!(%err, "no merge tool available for Resolve");
            None
        }
    }
}

/// Loads `[merge-tools.<name>]`. `Ok(None)` means no such table; a table
/// without `merge-args` is an error naming the missing piece (the CLI's
/// `MergeArgsNotConfigured`), since running it could not work.
fn tool_by_name(settings: &UserSettings, name: &str) -> Result<Option<MergeTool>, BackendError> {
    let path = ConfigNamePathBuf::from_iter(["merge-tools", name]);
    let table: Option<MergeToolTable> = settings.get(&path).optional().map_err(config_err)?;
    let Some(table) = table else {
        return Ok(None);
    };
    if table.merge_args.is_empty() {
        return Err(config_err(format!(
            "the tool \u{201c}{name}\u{201d} cannot be used for conflict resolution: \
             merge-tools.{name}.merge-args is not configured"
        )));
    }
    Ok(Some(MergeTool {
        name: name.to_owned(),
        program: if table.program.is_empty() {
            name.to_owned()
        } else {
            table.program
        },
        merge_args: table.merge_args,
        edits_conflict_markers: table.merge_tool_edits_conflict_markers,
        conflict_exit_codes: table.merge_conflict_exit_codes,
        marker_style: table.conflict_marker_style,
    }))
}

/// An inline command form (`ui.merge-editor = ["prog", "$left", …]` or a
/// multi-word string): the first word is the program, the rest are the
/// merge args. Like the CLI, a bare program with no arguments cannot merge.
fn tool_from_command(words: &[String]) -> Result<MergeTool, BackendError> {
    let [program, args @ ..] = words else {
        return Err(config_err("ui.merge-editor is empty"));
    };
    if args.is_empty() {
        return Err(config_err(format!(
            "the tool \u{201c}{program}\u{201d} cannot be used for conflict resolution: \
             merge-tools.{program}.merge-args is not configured"
        )));
    }
    let name = Path::new(program)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.clone());
    Ok(MergeTool {
        name,
        program: program.clone(),
        merge_args: args.to_vec(),
        edits_conflict_markers: false,
        conflict_exit_codes: vec![],
        marker_style: None,
    })
}

/// The curated default when nothing is configured: Sublime Merge, when it
/// can be found. Its table config always exists (embedded defaults), so
/// availability of the binary is the only question.
fn default_tool(settings: &UserSettings) -> Result<MergeTool, BackendError> {
    if find_program("smerge").is_none() {
        return Err(BackendError::MutationFailed(NO_TOOL_HELP.to_owned()));
    }
    let tool = tool_by_name(settings, "smerge")?;
    tool.ok_or_else(|| BackendError::MutationFailed(NO_TOOL_HELP.to_owned()))
}

/// What one tool run produced: the bytes of the `$output` file plus whether
/// the exit code declared that conflict markers remain in it.
pub(crate) struct MergeToolOutput {
    pub content: Vec<u8>,
    pub exit_implies_conflict: bool,
}

/// The materialized sides handed to the tool, plus everything needed to
/// name the temp files and interpolate the arguments.
pub(crate) struct MergeInput<'a> {
    pub base: &'a [u8],
    pub left: &'a [u8],
    pub right: &'a [u8],
    /// Initial contents of `$output`: the materialized conflict for tools
    /// that edit markers in place, empty otherwise.
    pub initial_output: &'a [u8],
    /// The conflicted file's name, used as the temp-file suffix so the tool
    /// shows something recognizable.
    pub file_name: &'a str,
    /// Repo-relative path for `$path`.
    pub repo_path: &'a str,
    pub marker_len: usize,
}

/// Runs the tool and waits for it to exit — for a GUI merge tool that means
/// however long the user keeps the merge window open. Mirrors the CLI:
/// inputs are read-only temp files, a failure exit aborts (unless declared
/// a conflict-remains code), and an empty or untouched output file means
/// nothing should be recorded.
pub(crate) fn run_merge_tool(
    tool: &MergeTool,
    input: &MergeInput<'_>,
) -> Result<MergeToolOutput, BackendError> {
    let binary = find_program(&tool.program).ok_or_else(|| {
        BackendError::MutationFailed(format!(
            "the merge tool \u{201c}{}\u{201d} is not installed (no \u{201c}{}\u{201d} on PATH)",
            tool.name, tool.program
        ))
    })?;

    let temp_dir = tempfile::Builder::new()
        .prefix("jiji-resolve-")
        .tempdir()
        .map_err(|err| BackendError::MutationFailed(format!("could not set up temp files: {err}")))?;
    let io_err = |err: std::io::Error| {
        BackendError::MutationFailed(format!("could not write merge temp files: {err}"))
    };
    let file_for = |role: &str, contents: &[u8], read_only: bool| -> Result<String, BackendError> {
        let path = temp_dir.path().join(format!("{role}_{}", input.file_name));
        std::fs::write(&path, contents).map_err(io_err)?;
        if read_only {
            let mut perms = std::fs::metadata(&path).map_err(io_err)?.permissions();
            perms.set_readonly(true);
            std::fs::set_permissions(&path, perms).map_err(io_err)?;
        }
        Ok(path.to_string_lossy().into_owned())
    };
    let base = file_for("base", input.base, true)?;
    let left = file_for("left", input.left, true)?;
    let right = file_for("right", input.right, true)?;
    let output = file_for("output", input.initial_output, false)?;

    let marker_len = input.marker_len.to_string();
    let variables: Vec<(&str, &str)> = vec![
        ("base", &base),
        ("left", &left),
        ("right", &right),
        ("output", &output),
        ("marker_length", &marker_len),
        ("path", input.repo_path),
    ];
    let args: Vec<String> = tool
        .merge_args
        .iter()
        .map(|arg| interpolate_variables(arg, &variables))
        .collect();

    let mut cmd = std::process::Command::new(&binary);
    cmd.args(&args).stdin(std::process::Stdio::null());
    tracing::info!(?cmd, "invoking the external merge tool");
    let exit_status = cmd.status().map_err(|err| {
        BackendError::MutationFailed(format!(
            "could not launch \u{201c}{}\u{201d} ({}): {err}",
            tool.name,
            binary.display()
        ))
    })?;
    tracing::info!(%exit_status, "external merge tool exited");

    let exit_implies_conflict = exit_status
        .code()
        .is_some_and(|code| tool.conflict_exit_codes.contains(&code));
    if !exit_status.success() && !exit_implies_conflict {
        return Err(BackendError::MutationFailed(format!(
            "\u{201c}{}\u{201d} was closed without completing the merge ({exit_status}); \
             nothing was recorded",
            tool.name
        )));
    }

    let content = std::fs::read(&output).map_err(|err| {
        BackendError::MutationFailed(format!("could not read the merge result: {err}"))
    })?;
    if content.is_empty() || content == input.initial_output {
        return Err(BackendError::MutationFailed(format!(
            "\u{201c}{}\u{201d} exited without saving a merge result; nothing was recorded",
            tool.name
        )));
    }
    Ok(MergeToolOutput {
        content,
        exit_implies_conflict,
    })
}

/// `$variable` interpolation matching the CLI's rule (`\$([a-z0-9_]+)\b`):
/// a maximal run of `[a-z0-9_]` after `$` is looked up and substituted;
/// unknown variables stay literal.
fn interpolate_variables(arg: &str, variables: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(arg.len());
    let mut rest = arg;
    while let Some(pos) = rest.find('$') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos + 1..];
        let end = rest
            .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
            .unwrap_or(rest.len());
        let name = &rest[..end];
        match variables.iter().find(|(key, _)| *key == name) {
            Some((_, value)) if !name.is_empty() => out.push_str(value),
            _ => {
                out.push('$');
                out.push_str(name);
            }
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// Locates a program like `Command` would (absolute paths and explicit
/// relative paths pass through; bare names search `PATH`), with one extra:
/// Sublime Merge's `smerge` is also looked for in its macOS app bundle,
/// because a GUI-launched app gets a minimal `PATH` that Homebrew- or
/// manually-linked tools may not be on.
pub(crate) fn find_program(program: &str) -> Option<PathBuf> {
    find_program_in(program, std::env::var_os("PATH").as_deref(), &known_locations(program))
}

fn find_program_in(
    program: &str,
    path_var: Option<&std::ffi::OsStr>,
    extra_locations: &[PathBuf],
) -> Option<PathBuf> {
    let as_path = Path::new(program);
    if as_path.components().count() > 1 {
        // Explicit path: trust it if it exists; no PATH search.
        return is_executable(as_path).then(|| as_path.to_owned());
    }
    let on_path = path_var.and_then(|paths| {
        std::env::split_paths(paths)
            .map(|dir| dir.join(program))
            .find(|candidate| is_executable(candidate))
    });
    on_path.or_else(|| {
        extra_locations
            .iter()
            .find(|candidate| is_executable(candidate))
            .cloned()
    })
}

/// Standard install locations checked after `PATH` for known GUI tools.
fn known_locations(program: &str) -> Vec<PathBuf> {
    match program {
        "smerge" => {
            let bundle = "Sublime Merge.app/Contents/SharedSupport/bin/smerge";
            let mut locations = vec![PathBuf::from(format!("/Applications/{bundle}"))];
            if let Some(home) = std::env::home_dir() {
                locations.push(home.join(format!("Applications/{bundle}")));
            }
            locations
        }
        _ => Vec::new(),
    }
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        path.is_file()
            && std::fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(test)]
mod tests {
    use jj_lib::config::{ConfigLayer, ConfigSource, StackedConfig};

    use super::*;

    /// Settings with Jiji's embedded defaults plus one user layer, like
    /// `load_settings` builds them — without reading any real machine state.
    fn settings_with(user_layer: &str) -> UserSettings {
        let mut config = StackedConfig::with_defaults();
        config.add_layer(
            ConfigLayer::parse(ConfigSource::Default, crate::settings::DEFAULT_CONFIG)
                .expect("embedded default config must parse"),
        );
        config.add_layer(
            ConfigLayer::parse(ConfigSource::User, user_layer).expect("test layer must parse"),
        );
        UserSettings::from_config(config).expect("test settings must build")
    }

    #[test]
    fn named_tool_reads_the_merge_tools_table() {
        let settings = settings_with(r#"ui.merge-editor = "meld""#);
        let tool = resolve_merge_tool(&settings).unwrap();
        assert_eq!(tool.name, "meld");
        assert_eq!(tool.program, "meld");
        assert_eq!(tool.merge_args[0], "$left");
        assert!(!tool.edits_conflict_markers);

        // vscode: program differs from the name, edits markers in place.
        let settings = settings_with(r#"ui.merge-editor = "vscode""#);
        let tool = resolve_merge_tool(&settings).unwrap();
        assert_eq!(tool.program, "code");
        assert!(tool.edits_conflict_markers);
        assert_eq!(tool.marker_style, Some(ConflictMarkerStyle::Git));

        // mergiraf: conflict exit codes pass through.
        let settings = settings_with(r#"ui.merge-editor = "mergiraf""#);
        let tool = resolve_merge_tool(&settings).unwrap();
        assert_eq!(tool.conflict_exit_codes, vec![1]);
    }

    #[test]
    fn user_config_overrides_and_extends_tool_tables() {
        // Per-user/per-repo override of an embedded tool definition.
        let settings = settings_with(
            r#"
            ui.merge-editor = "smerge"
            merge-tools.smerge.program = "/opt/sublime/smerge"
            "#,
        );
        let tool = resolve_merge_tool(&settings).unwrap();
        assert_eq!(tool.program, "/opt/sublime/smerge");
        assert_eq!(tool.merge_args[0], "mergetool", "embedded args kept");

        // A tool Jiji has never heard of, fully user-defined.
        let settings = settings_with(
            r#"
            ui.merge-editor = "mytool"
            merge-tools.mytool.merge-args = ["$base", "$left", "$right", "$output"]
            "#,
        );
        let tool = resolve_merge_tool(&settings).unwrap();
        assert_eq!(tool.program, "mytool");
    }

    #[test]
    fn command_forms_and_refusals() {
        // Array form: program plus explicit args.
        let settings =
            settings_with(r#"ui.merge-editor = ["/usr/bin/mymerge", "$left", "$base", "$right", "$output"]"#);
        let tool = resolve_merge_tool(&settings).unwrap();
        assert_eq!(tool.name, "mymerge");
        assert_eq!(tool.program, "/usr/bin/mymerge");
        assert_eq!(tool.merge_args.len(), 4);

        // Multi-word string form splits like the CLI.
        let settings = settings_with(r#"ui.merge-editor = "mymerge $left $base $right $output""#);
        let tool = resolve_merge_tool(&settings).unwrap();
        assert_eq!(tool.program, "mymerge");
        assert_eq!(tool.merge_args.len(), 4);

        // A bare unknown name has no merge-args to run with.
        let settings = settings_with(r#"ui.merge-editor = "sometool""#);
        let err = resolve_merge_tool(&settings).unwrap_err();
        assert!(matches!(err, BackendError::ConfigInvalid(_)), "got {err:?}");
        assert!(err.to_string().contains("merge-args"));

        // Builtin CLI tools cannot run inside Jiji.
        let settings = settings_with(r#"ui.merge-editor = ":builtin""#);
        let err = resolve_merge_tool(&settings).unwrap_err();
        assert!(err.to_string().contains("Jiji cannot run"), "got {err}");

        assert!(available_tool_name(&settings).is_none());
        let settings = settings_with(r#"ui.merge-editor = "meld""#);
        assert_eq!(available_tool_name(&settings).as_deref(), Some("meld"));
    }

    #[test]
    fn interpolation_matches_the_cli_rule() {
        let vars: Vec<(&str, &str)> = vec![("left", "/tmp/L"), ("marker_length", "7")];
        assert_eq!(interpolate_variables("$left", &vars), "/tmp/L");
        assert_eq!(interpolate_variables("--in=$left.", &vars), "--in=/tmp/L.");
        assert_eq!(interpolate_variables("-l$marker_length", &vars), "-l7");
        // Unknown variables and bare dollars stay literal.
        assert_eq!(interpolate_variables("$unknown", &vars), "$unknown");
        assert_eq!(interpolate_variables("$leftmost", &vars), "$leftmost");
        assert_eq!(interpolate_variables("a$", &vars), "a$");
        assert_eq!(interpolate_variables("$LEFT", &vars), "$LEFT");
    }

    #[test]
    fn find_program_searches_path_then_known_locations() {
        let dir = tempfile::tempdir().unwrap();
        let on_path = dir.path().join("bin");
        std::fs::create_dir(&on_path).unwrap();
        let extra = dir.path().join("extra");
        std::fs::create_dir(&extra).unwrap();

        let write_exec = |dir: &Path, name: &str| {
            let path = dir.join(name);
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            path
        };
        let tool_on_path = write_exec(&on_path, "mytool");
        let tool_in_extra = write_exec(&extra, "othertool");

        let path_var = std::env::join_paths([&on_path]).unwrap();
        assert_eq!(
            find_program_in("mytool", Some(path_var.as_os_str()), &[]),
            Some(tool_on_path.clone())
        );
        assert_eq!(find_program_in("missing", Some(path_var.as_os_str()), &[]), None);
        // Known locations are the fallback when PATH misses.
        assert_eq!(
            find_program_in(
                "othertool",
                Some(path_var.as_os_str()),
                &[extra.join("othertool")]
            ),
            Some(tool_in_extra)
        );
        // Explicit paths pass through without a search.
        assert_eq!(
            find_program_in(tool_on_path.to_str().unwrap(), None, &[]),
            Some(tool_on_path)
        );
        #[cfg(unix)]
        {
            // A plain file without the executable bit is not a program.
            let plain = on_path.join("notes.txt");
            std::fs::write(&plain, "hi").unwrap();
            assert_eq!(
                find_program_in("notes.txt", Some(path_var.as_os_str()), &[]),
                None
            );
        }
    }
}
