//! The enclosing symbol for a diff hunk, in the spirit of git's
//! `xfuncname`: scan upward from the hunk's first line for the nearest
//! line that opens a named declaration, and label the hunk with it. The
//! diff surface shows the label on the "N unchanged lines" band above the
//! hunk, so a reader landing mid-file knows what they are inside without
//! scrolling — the anatomy Sublime Merge's hunk headers provide.
//!
//! Matching is a per-family token scan, not a parse: like git's default
//! patterns it trades precision for robustness, and a miss just means an
//! unlabeled band. Languages whose line grammar makes "the enclosing
//! declaration" meaningless from one line (markup, data files) opt out by
//! having no family.

/// How declaration lines look for one language family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FuncFamily {
    Rust,
    JsTs,
    Python,
    Go,
    /// Git's default heuristic: an unindented line starting a word.
    CLike,
}

impl FuncFamily {
    /// The family for a repo-relative path, by extension. None for
    /// languages where a single line can't name the enclosing scope.
    pub fn of(path: &str) -> Option<FuncFamily> {
        let ext = path.rsplit('.').next()?;
        match ext {
            "rs" => Some(FuncFamily::Rust),
            "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "svelte" | "vue" => {
                Some(FuncFamily::JsTs)
            }
            "py" | "pyi" => Some(FuncFamily::Python),
            "go" => Some(FuncFamily::Go),
            "c" | "h" | "cc" | "cpp" | "hpp" | "m" | "mm" | "swift" | "java" | "kt" | "cs" => {
                Some(FuncFamily::CLike)
            }
            _ => None,
        }
    }
}

/// The nearest declaration line strictly above `hunk_start` (0-based line
/// index), trimmed for display. None when nothing above matches.
pub fn enclosing_symbol(
    lines: &[&[u8]],
    hunk_start: usize,
    family: FuncFamily,
) -> Option<String> {
    let upto = hunk_start.min(lines.len());
    for raw in lines[..upto].iter().rev() {
        let line = String::from_utf8_lossy(raw);
        if declares(line.trim_start(), &line, family) {
            let label: String = line.trim().chars().take(120).collect();
            return Some(label);
        }
    }
    None
}

fn declares(trimmed: &str, full: &str, family: FuncFamily) -> bool {
    match family {
        FuncFamily::Rust => first_word_after(
            trimmed,
            &["pub", "async", "const", "unsafe", "extern", "default"],
        )
        .is_some_and(|w| {
            matches!(w, "fn" | "struct" | "enum" | "union" | "trait" | "impl" | "mod")
                || w.starts_with("macro_rules")
        }),
        FuncFamily::JsTs => first_word_after(
            trimmed,
            &[
                "export", "default", "declare", "abstract", "async", "public", "private",
                "protected", "static", "readonly",
            ],
        )
        .is_some_and(|w| {
            matches!(w, "function" | "class" | "interface" | "enum" | "namespace")
                || w.starts_with("function*")
        }),
        FuncFamily::Python => first_word_after(trimmed, &["async"])
            .is_some_and(|w| matches!(w, "def" | "class")),
        FuncFamily::Go => {
            let w = trimmed.split_whitespace().next().unwrap_or("");
            matches!(w, "func" | "type")
        }
        FuncFamily::CLike => full
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_'),
    }
}

/// The first whitespace-separated word that is not one of `modifiers`
/// (matched by prefix, so `pub(crate)` and `extern "C"`'s ABI string are
/// stepped over).
fn first_word_after<'a>(line: &'a str, modifiers: &[&str]) -> Option<&'a str> {
    line.split_whitespace().find(|word| {
        !modifiers.iter().any(|m| word.starts_with(m)) && !word.starts_with('"')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(lines: &[&str], start: usize, family: FuncFamily) -> Option<String> {
        let bytes: Vec<&[u8]> = lines.iter().map(|l| l.as_bytes()).collect();
        enclosing_symbol(&bytes, start, family)
    }

    #[test]
    fn rust_declarations() {
        let lines = [
            "use std::sync::Arc;",
            "",
            "pub(crate) unsafe fn present_sign_in(window: &Window) {",
            "    let delegate = SignInDelegate::new();",
            "    delegate.run();",
        ];
        assert_eq!(
            sym(&lines, 4, FuncFamily::Rust).as_deref(),
            Some("pub(crate) unsafe fn present_sign_in(window: &Window) {")
        );
        // Nothing declares above the first line.
        assert_eq!(sym(&lines, 1, FuncFamily::Rust), None);
    }

    #[test]
    fn rust_impl_and_macro() {
        let lines = [
            "impl SignInDelegate {",
            "    fn did_fail(&self) {}",
            "}",
            "macro_rules! declare {",
            "    () => {};",
        ];
        assert_eq!(
            sym(&lines, 2, FuncFamily::Rust).as_deref(),
            Some("fn did_fail(&self) {}")
        );
        assert_eq!(
            sym(&lines, 4, FuncFamily::Rust).as_deref(),
            Some("macro_rules! declare {")
        );
    }

    #[test]
    fn ts_and_python_and_go() {
        let ts = ["export default async function submit() {", "  await go();"];
        assert_eq!(
            sym(&ts, 1, FuncFamily::JsTs).as_deref(),
            Some("export default async function submit() {")
        );
        let py = ["class Robot:", "    async def move(self):", "        pass"];
        assert_eq!(
            sym(&py, 2, FuncFamily::Python).as_deref(),
            Some("async def move(self):")
        );
        let go = ["func (s *Server) Serve() error {", "\treturn nil"];
        assert_eq!(
            sym(&go, 1, FuncFamily::Go).as_deref(),
            Some("func (s *Server) Serve() error {")
        );
    }

    #[test]
    fn clike_top_level_only() {
        let lines = ["static int visible = 0;", "  indented = 1;", "  more;"];
        assert_eq!(
            sym(&lines, 2, FuncFamily::CLike).as_deref(),
            Some("static int visible = 0;")
        );
    }

    #[test]
    fn families_by_extension() {
        assert_eq!(FuncFamily::of("src/apple_signin.rs"), Some(FuncFamily::Rust));
        assert_eq!(FuncFamily::of("src/App.svelte"), Some(FuncFamily::JsTs));
        assert_eq!(FuncFamily::of("Info.plist"), None);
        assert_eq!(FuncFamily::of("styles/tokens.css"), None);
        assert_eq!(FuncFamily::of("README"), None);
    }
}
