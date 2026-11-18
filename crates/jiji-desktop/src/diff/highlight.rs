//! Syntax highlighting for the diff with tree-sitter. A hunk is parsed as
//! two small standalone documents — the old side (context + removed lines)
//! and the new side (context + added lines) — so removed code highlights as
//! it was and added code as it is. Context lines take the new side's
//! reading.
//!
//! Everything here runs on GPUI's background executor; the view paints
//! plain text first and repaints as each file's marks arrive.

use std::cell::RefCell;
use std::ops::Range;
use std::sync::OnceLock;

use gpui::SharedString;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

use crate::diff::model::{Kind, LineId};

/// The app's restrained class set: hues mark scan anchors, everything else
/// stays plain text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Syn {
    Kw,
    Str,
    Const,
    Type,
    Fn,
    Prop,
    Comment,
    Meta,
    Heading,
    Link,
    Em,
    Strong,
}

/// A styled byte range of one line's painted text.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub range: Range<u32>,
    pub class: Syn,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Json,
    Css,
    Html,
    Python,
    Markdown,
    MarkdownInline,
    Yaml,
    Toml,
    Svelte,
}

const LANG_COUNT: usize = 13;

pub fn lang_for_path(path: &str) -> Option<Lang> {
    let name = path.rsplit('/').next()?;
    if name == "Cargo.lock" {
        return Some(Lang::Toml);
    }
    let (_, ext) = name.rsplit_once('.')?;
    Some(match ext.to_ascii_lowercase().as_str() {
        "rs" => Lang::Rust,
        "ts" | "mts" | "cts" => Lang::TypeScript,
        "tsx" => Lang::Tsx,
        "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
        "json" => Lang::Json,
        "css" | "scss" | "less" => Lang::Css,
        "html" | "htm" | "vue" | "xml" | "svg" | "plist" => Lang::Html,
        "py" | "pyi" => Lang::Python,
        "md" | "markdown" => Lang::Markdown,
        "yaml" | "yml" => Lang::Yaml,
        "toml" => Lang::Toml,
        "svelte" => Lang::Svelte,
        _ => return None,
    })
}

/// Languages other grammars inject (a `<script>` in Svelte, a fenced block
/// in Markdown), by the names their injection queries use.
fn lang_for_injection(name: &str) -> Option<Lang> {
    Some(match name.to_ascii_lowercase().as_str() {
        "rust" | "rs" => Lang::Rust,
        "typescript" | "ts" => Lang::TypeScript,
        "tsx" => Lang::Tsx,
        "javascript" | "js" | "jsx" => Lang::JavaScript,
        "json" => Lang::Json,
        "css" | "scss" => Lang::Css,
        "html" => Lang::Html,
        "python" | "py" => Lang::Python,
        "markdown" | "md" => Lang::Markdown,
        "markdown_inline" => Lang::MarkdownInline,
        "yaml" | "yml" => Lang::Yaml,
        "toml" => Lang::Toml,
        "svelte" => Lang::Svelte,
        _ => return None,
    })
}

/// Capture names the highlighter recognizes and the class each paints in.
/// A capture resolves to its longest dotted prefix here (`keyword.control`
/// to `keyword`); anything without one (variables, punctuation, operators)
/// stays plain.
const CAPTURES: &[(&str, Syn)] = &[
    ("attribute", Syn::Prop),
    ("boolean", Syn::Const),
    ("comment", Syn::Comment),
    ("constant", Syn::Const),
    ("constructor", Syn::Type),
    ("escape", Syn::Meta),
    ("function", Syn::Fn),
    ("keyword", Syn::Kw),
    ("label", Syn::Meta),
    ("module", Syn::Type),
    ("namespace", Syn::Type),
    ("number", Syn::Const),
    ("property", Syn::Prop),
    ("string", Syn::Str),
    ("string.escape", Syn::Meta),
    ("string.regexp", Syn::Meta),
    ("string.special", Syn::Meta),
    ("string.special.key", Syn::Prop),
    ("tag", Syn::Type),
    // Svelte's block sigils: `#if` and `/if` read as one keyword.
    ("tag.delimiter", Syn::Kw),
    ("type", Syn::Type),
    ("variable.builtin", Syn::Kw),
    ("variable.member", Syn::Prop),
    ("text.title", Syn::Heading),
    ("text.literal", Syn::Str),
    ("text.uri", Syn::Link),
    ("text.reference", Syn::Link),
    ("text.emphasis", Syn::Em),
    ("text.strong", Syn::Strong),
    ("markup.heading", Syn::Heading),
    ("markup.raw", Syn::Str),
    ("markup.link", Syn::Link),
    ("markup.italic", Syn::Em),
    ("markup.bold", Syn::Strong),
];

/// Index of `attribute` in `CAPTURES`: Rust attributes (`#[derive]`) read
/// as metadata, where HTML and CSS attribute names read as properties.
const ATTRIBUTE: usize = 0;

static CONFIGS: [OnceLock<Option<HighlightConfiguration>>; LANG_COUNT] =
    [const { OnceLock::new() }; LANG_COUNT];

fn config(lang: Lang) -> Option<&'static HighlightConfiguration> {
    CONFIGS[lang as usize].get_or_init(|| build(lang)).as_ref()
}

fn build(lang: Lang) -> Option<HighlightConfiguration> {
    let js = tree_sitter_javascript::HIGHLIGHT_QUERY;
    let jsx = tree_sitter_javascript::JSX_HIGHLIGHT_QUERY;
    let ts = tree_sitter_typescript::HIGHLIGHTS_QUERY;
    // The TypeScript query only covers what TypeScript adds; it layers over
    // JavaScript's (later patterns win). Candidates are tried in order so a
    // base-query pattern the derived grammar rejects costs fidelity, not
    // highlighting.
    let (language, name, highlights, injections): (tree_sitter::Language, _, Vec<String>, &str) =
        match lang {
            Lang::Rust => (
                tree_sitter_rust::LANGUAGE.into(),
                "rust",
                vec![tree_sitter_rust::HIGHLIGHTS_QUERY.into()],
                tree_sitter_rust::INJECTIONS_QUERY,
            ),
            Lang::TypeScript => (
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                "typescript",
                vec![format!("{js}\n{ts}"), ts.into()],
                "",
            ),
            Lang::Tsx => (
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                "tsx",
                vec![
                    format!("{js}\n{jsx}\n{ts}"),
                    format!("{js}\n{ts}"),
                    ts.into(),
                ],
                "",
            ),
            Lang::JavaScript => (
                tree_sitter_javascript::LANGUAGE.into(),
                "javascript",
                vec![format!("{js}\n{jsx}"), js.into()],
                tree_sitter_javascript::INJECTIONS_QUERY,
            ),
            Lang::Json => (
                tree_sitter_json::LANGUAGE.into(),
                "json",
                vec![tree_sitter_json::HIGHLIGHTS_QUERY.into()],
                "",
            ),
            Lang::Css => (
                tree_sitter_css::LANGUAGE.into(),
                "css",
                vec![tree_sitter_css::HIGHLIGHTS_QUERY.into()],
                "",
            ),
            Lang::Html => (
                tree_sitter_html::LANGUAGE.into(),
                "html",
                vec![tree_sitter_html::HIGHLIGHTS_QUERY.into()],
                tree_sitter_html::INJECTIONS_QUERY,
            ),
            Lang::Python => (
                tree_sitter_python::LANGUAGE.into(),
                "python",
                vec![tree_sitter_python::HIGHLIGHTS_QUERY.into()],
                "",
            ),
            Lang::Markdown => (
                tree_sitter_md::LANGUAGE.into(),
                "markdown",
                vec![tree_sitter_md::HIGHLIGHT_QUERY_BLOCK.into()],
                tree_sitter_md::INJECTION_QUERY_BLOCK,
            ),
            Lang::MarkdownInline => (
                tree_sitter_md::INLINE_LANGUAGE.into(),
                "markdown_inline",
                vec![tree_sitter_md::HIGHLIGHT_QUERY_INLINE.into()],
                "",
            ),
            Lang::Yaml => (
                tree_sitter_yaml::LANGUAGE.into(),
                "yaml",
                vec![tree_sitter_yaml::HIGHLIGHTS_QUERY.into()],
                "",
            ),
            Lang::Toml => (
                tree_sitter_toml_ng::LANGUAGE.into(),
                "toml",
                vec![tree_sitter_toml_ng::HIGHLIGHTS_QUERY.into()],
                "",
            ),
            // Svelte's query inherits HTML's the way TypeScript's inherits
            // JavaScript's. The crate's own injection query assumes an
            // editor's inheritance chain, so the three that matter are
            // spelled out: styles, scripts, and every `{expression}`.
            // TypeScript parses plain JavaScript too, which spares reading
            // `lang="ts"` off a script tag a hunk may not even include.
            Lang::Svelte => (
                tree_sitter_svelte_ng::LANGUAGE.into(),
                "svelte",
                vec![
                    format!(
                        "{}\n{}",
                        tree_sitter_html::HIGHLIGHTS_QUERY,
                        tree_sitter_svelte_ng::HIGHLIGHTS_QUERY
                    ),
                    tree_sitter_svelte_ng::HIGHLIGHTS_QUERY.into(),
                ],
                SVELTE_INJECTIONS,
            ),
        };
    let names: Vec<&str> = CAPTURES.iter().map(|(name, _)| *name).collect();
    let mut last_error = None;
    for query in &highlights {
        // An injection query that fails to compile is dropped rather than
        // taking the language's highlighting down with it.
        for injections in [injections, ""] {
            match HighlightConfiguration::new(language.clone(), name, query, injections, "") {
                Ok(mut config) => {
                    config.configure(&names);
                    return Some(config);
                }
                Err(err) => last_error = Some(err),
            }
        }
    }
    if let Some(err) = last_error {
        eprintln!("jiji: no highlighting for {name}: {err}");
    }
    None
}

const SVELTE_INJECTIONS: &str = r#"
((style_element (raw_text) @injection.content) (#set! injection.language "css"))
((script_element (raw_text) @injection.content) (#set! injection.language "typescript"))
((svelte_raw_text) @injection.content (#set! injection.language "typescript"))
"#;

thread_local! {
    static HIGHLIGHTER: RefCell<Highlighter> = RefCell::new(Highlighter::new());
}

/// Past this many bytes a hunk side skips highlighting (a single hunk can
/// be an entire new file).
const MAX_HIGHLIGHT_BYTES: usize = 200_000;

/// Styled ranges of one standalone document, in document order.
fn doc_marks(lang: Lang, doc: &str) -> Option<Vec<(Range<usize>, Syn)>> {
    let config = config(lang)?;
    HIGHLIGHTER.with(|highlighter| {
        let mut highlighter = highlighter.borrow_mut();
        let events = highlighter
            .highlight(config, doc.as_bytes(), None, None, |name| {
                lang_for_injection(name).and_then(self::config)
            })
            .ok()?;
        let mut stack: Vec<Syn> = Vec::new();
        let mut out: Vec<(Range<usize>, Syn)> = Vec::new();
        for event in events {
            match event.ok()? {
                HighlightEvent::HighlightStart(highlight) => {
                    let class = if highlight.0 == ATTRIBUTE && lang == Lang::Rust {
                        Syn::Meta
                    } else {
                        CAPTURES[highlight.0].1
                    };
                    stack.push(class);
                }
                HighlightEvent::HighlightEnd => {
                    stack.pop();
                }
                HighlightEvent::Source { start, end } => {
                    let Some(&class) = stack.last() else { continue };
                    if end <= start {
                        continue;
                    }
                    match out.last_mut() {
                        Some((range, last)) if range.end == start && *last == class => {
                            range.end = end
                        }
                        _ => out.push((start..end, class)),
                    }
                }
            }
        }
        Some(out)
    })
}

/// Parse one side of a hunk and distribute the styled ranges onto its
/// lines as line-relative offsets. A token can span lines (a block
/// comment), so ranges clip per line. `None` when the side is too large or
/// the language has no working grammar.
fn side_marks(lang: Lang, texts: &[&str]) -> Option<Vec<Vec<Mark>>> {
    let mut starts = Vec::with_capacity(texts.len());
    let mut doc = String::new();
    for text in texts {
        starts.push(doc.len());
        doc.push_str(text);
        doc.push('\n');
    }
    if doc.len() > MAX_HIGHLIGHT_BYTES {
        return None;
    }
    let ranges = doc_marks(lang, &doc)?;
    let mut marks: Vec<Vec<Mark>> = texts.iter().map(|_| Vec::new()).collect();
    let mut line = 0;
    for (range, class) in ranges {
        while line + 1 < starts.len() && starts[line + 1] <= range.start {
            line += 1;
        }
        let mut i = line;
        while i < texts.len() && starts[i] < range.end {
            let from = range.start.max(starts[i]) - starts[i];
            let to = range.end.min(starts[i] + texts[i].len()) - starts[i];
            if to > from {
                marks[i].push(Mark {
                    range: from as u32..to as u32,
                    class,
                });
            }
            i += 1;
        }
    }
    Some(marks)
}

/// One file's worth of highlighting work: each hunk's lines with the ids
/// the results are keyed by.
pub struct HighlightJob {
    pub lang: Lang,
    pub hunks: Vec<Vec<(LineId, Kind, SharedString)>>,
}

impl HighlightJob {
    pub fn run(&self) -> Vec<(LineId, Vec<Mark>)> {
        let mut out = Vec::new();
        for hunk in &self.hunks {
            let old: Vec<_> = hunk.iter().filter(|line| line.1 != Kind::Added).collect();
            let new: Vec<_> = hunk.iter().filter(|line| line.1 != Kind::Removed).collect();
            // The old side only matters for removed lines; a pure-add hunk
            // skips it.
            if old.iter().any(|line| line.1 == Kind::Removed) {
                let texts: Vec<&str> = old.iter().map(|line| line.2.as_ref()).collect();
                if let Some(marks) = side_marks(self.lang, &texts) {
                    for (line, marks) in old.iter().zip(marks) {
                        if line.1 == Kind::Removed && !marks.is_empty() {
                            out.push((line.0, marks));
                        }
                    }
                }
            }
            let texts: Vec<&str> = new.iter().map(|line| line.2.as_ref()).collect();
            if let Some(marks) = side_marks(self.lang, &texts) {
                for (line, marks) in new.iter().zip(marks) {
                    if !marks.is_empty() {
                        out.push((line.0, marks));
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(lang: Lang, line: &str) -> Vec<(&str, Syn)> {
        let marks = side_marks(lang, &[line]).expect("grammar loads");
        marks[0]
            .iter()
            .map(|mark| {
                (
                    &line[mark.range.start as usize..mark.range.end as usize],
                    mark.class,
                )
            })
            .collect()
    }

    #[test]
    fn every_language_has_a_working_grammar() {
        for lang in [
            Lang::Rust,
            Lang::TypeScript,
            Lang::Tsx,
            Lang::JavaScript,
            Lang::Json,
            Lang::Css,
            Lang::Html,
            Lang::Python,
            Lang::Markdown,
            Lang::MarkdownInline,
            Lang::Yaml,
            Lang::Toml,
            Lang::Svelte,
        ] {
            assert!(config(lang).is_some(), "{lang:?} failed to load");
        }
    }

    #[test]
    fn rust_tokens_land_in_the_expected_classes() {
        let found = classes(Lang::Rust, "pub fn main() { let x = \"hi\"; } // done");
        assert!(found.contains(&("pub", Syn::Kw)), "{found:?}");
        assert!(found.contains(&("main", Syn::Fn)), "{found:?}");
        assert!(found.contains(&("\"hi\"", Syn::Str)), "{found:?}");
        assert!(found.contains(&("// done", Syn::Comment)), "{found:?}");
    }

    #[test]
    fn typescript_layers_over_the_javascript_query() {
        let found = classes(Lang::TypeScript, "const n: number = 42;");
        assert!(found.contains(&("const", Syn::Kw)), "{found:?}");
        assert!(found.contains(&("number", Syn::Type)), "{found:?}");
        assert!(found.contains(&("42", Syn::Const)), "{found:?}");
    }

    #[test]
    fn svelte_colors_markup_and_the_expressions_inside_it() {
        let line = r#"<span class="dot">{count > 1 ? "many" : label}</span>"#;
        let found = classes(Lang::Svelte, line);
        assert!(found.contains(&("span", Syn::Type)), "{found:?}");
        assert!(found.contains(&("class", Syn::Prop)), "{found:?}");
        assert!(found.contains(&("dot", Syn::Str)), "{found:?}");
        // Injected TypeScript inside the braces.
        assert!(found.contains(&("1", Syn::Const)), "{found:?}");
        assert!(found.contains(&("\"many\"", Syn::Str)), "{found:?}");
        let block = classes(Lang::Svelte, "{#if ready}<p>ok</p>{/if}");
        // The sigil and the word merge into one keyword run.
        assert!(block.contains(&("#if", Syn::Kw)), "{block:?}");
        assert!(block.contains(&("/if", Syn::Kw)), "{block:?}");
    }

    #[test]
    fn a_token_spanning_lines_clips_to_each_line() {
        let marks = side_marks(Lang::Rust, &["/* a", "b */ fn f() {}"]).unwrap();
        assert_eq!(
            marks[0],
            vec![Mark {
                range: 0..4,
                class: Syn::Comment
            }]
        );
        assert_eq!(marks[1][0].range, 0..4);
        assert_eq!(marks[1][0].class, Syn::Comment);
    }

    #[test]
    fn removed_lines_highlight_from_the_old_side() {
        let job = HighlightJob {
            lang: Lang::Rust,
            hunks: vec![vec![
                (0, Kind::Removed, "let a = 1;".into()),
                (1, Kind::Added, "let a = 2;".into()),
            ]],
        };
        let marks = job.run();
        let ids: Vec<LineId> = marks.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![0, 1]);
    }
}
