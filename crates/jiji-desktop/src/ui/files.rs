//! A file's icon: its kind as a glyph, its language as a hue.

use gpui::{App, Svg};

use super::icon::{icon, IconName};
use crate::theme::{theme, FileHue};

/// The glyph and hue for a file, by its name.
pub fn file_glyph(name: &str) -> (IconName, FileHue) {
    let name = name.rsplit('/').next().unwrap_or(name);
    let lower = name.to_ascii_lowercase();
    // Whole names first: the files a repository is known by.
    match lower.as_str() {
        "cargo.lock" | "bun.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml"
        | "bun.lockb" | "flake.lock" | "go.sum" => return (IconName::FileLock, FileHue::Muted),
        "makefile" | "justfile" | "dockerfile" => return (IconName::FileTerminal, FileHue::Shell),
        "license" | "licence" | "copying" => return (IconName::FileText, FileHue::Muted),
        _ => {}
    }
    let ext = lower.rsplit_once('.').map_or("", |(_, ext)| ext);
    match ext {
        "rs" => (IconName::FileCode, FileHue::Rust),
        "ts" | "tsx" | "mts" | "cts" => (IconName::FileCode, FileHue::TypeScript),
        "js" | "jsx" | "mjs" | "cjs" => (IconName::FileCode, FileHue::JavaScript),
        "svelte" | "vue" | "astro" => (IconName::FileCode, FileHue::Svelte),
        "html" | "htm" | "xml" | "svg" => (IconName::FileCode, FileHue::Markup),
        "css" | "scss" | "sass" | "less" => (IconName::FileCode, FileHue::Style),
        "json" | "jsonc" | "json5" => (IconName::FileJson, FileHue::Data),
        "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "env" | "editorconfig" => {
            (IconName::FileSliders, FileHue::Config)
        }
        "md" | "mdx" | "markdown" | "txt" | "rst" | "adoc" => (IconName::FileText, FileHue::Docs),
        "py" | "pyi" => (IconName::FileCode, FileHue::Python),
        "go" => (IconName::FileCode, FileHue::Go),
        "sh" | "bash" | "zsh" | "fish" | "nu" => (IconName::FileTerminal, FileHue::Shell),
        "c" | "h" | "cc" | "cpp" | "hpp" | "cxx" | "m" | "mm" | "swift" | "zig" | "java" | "kt"
        | "kts" | "rb" | "ex" | "exs" | "hs" | "ml" | "lua" => {
            (IconName::FileCode, FileHue::Native)
        }
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" | "icns" | "bmp" | "tif" | "tiff"
        | "avif" | "heic" | "heif" => (IconName::FileImage, FileHue::Image),
        "lock" => (IconName::FileLock, FileHue::Muted),
        _ => (IconName::File, FileHue::Muted),
    }
}

/// A file's icon at `size`, in its language's hue.
pub fn file_icon(name: &str, size: f32, cx: &App) -> Svg {
    let (glyph, hue) = file_glyph(name);
    icon(glyph, size, theme(cx).file_hue(hue))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_known_by_name_then_extension() {
        assert_eq!(
            file_glyph("src/main.rs"),
            (IconName::FileCode, FileHue::Rust)
        );
        assert_eq!(
            file_glyph("Cargo.lock"),
            (IconName::FileLock, FileHue::Muted)
        );
        assert_eq!(file_glyph("README.md").1, FileHue::Docs);
        assert_eq!(file_glyph("tokens.CSS").1, FileHue::Style);
        assert_eq!(file_glyph("Makefile").1, FileHue::Shell);
        assert_eq!(file_glyph("notes"), (IconName::File, FileHue::Muted));
    }
}
