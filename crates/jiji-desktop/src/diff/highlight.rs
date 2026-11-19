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
    Xml,
}

const LANG_COUNT: usize = 14;

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
        "html" | "htm" | "vue" => Lang::Html,
        "xml" | "svg" | "xsd" | "plist" => Lang::Xml,
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
        "xml" => Lang::Xml,
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
            Lang::Xml => (
                tree_sitter_xml::LANGUAGE_XML.into(),
                "xml",
                vec![format!(
                    "{}\n{XML_FRAGMENT_HIGHLIGHTS}",
                    tree_sitter_xml::XML_HIGHLIGHT_QUERY
                )],
                "",
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

/// A hunk starts and stops anywhere: a closing tag whose opening tag sits
/// above it parses as an error, and still reads as the tag it is.
const XML_FRAGMENT_HIGHLIGHTS: &str = r#"(ERROR "</" . (Name) @tag)"#;

const SVELTE_INJECTIONS: &str = r#"
((style_element (raw_text) @injection.content) (#set! injection.language "css"))
((script_element (raw_text) @injection.content) (#set! injection.language "typescript"))
((svelte_raw_text) @injection.content (#set! injection.language "typescript"))
"#;

thread_local! {
    static HIGHLIGHTER: RefCell<Highlighter> = RefCell::new(Highlighter::new());
}

/// Past this much text a hunk side skips highlighting (a single hunk can
/// be an entire new file). Counted in UTF-16 units, newlines included, as
/// the Tauri app counted it, so a side in Chinese or Greek reaches as far
/// as it did there rather than a third or a half as far.
const MAX_HIGHLIGHT_UNITS: usize = 200_000;

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
                    let class = match (lang, CAPTURES[highlight.0].1) {
                        (Lang::Rust, _) if highlight.0 == ATTRIBUTE => Syn::Meta,
                        // The grammar captures a CDATA section's delimiters
                        // as headings; the whole section reads as a string.
                        (Lang::Xml, Syn::Heading) => Syn::Str,
                        (_, class) => class,
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
        if lang == Lang::Rust {
            token_tree_marks(doc, &mut out);
        }
        Some(out)
    })
}

thread_local! {
    static RUST_PARSER: RefCell<tree_sitter::Parser> = RefCell::new({
        let mut parser = tree_sitter::Parser::new();
        let _ = parser.set_language(&tree_sitter_rust::LANGUAGE.into());
        parser
    });
}

const RUST_KEYWORDS: &[&str] = &[
    "as",
    "async",
    "await",
    "break",
    "const",
    "continue",
    "crate",
    "dyn",
    "else",
    "enum",
    "extern",
    "fn",
    "for",
    "if",
    "impl",
    "in",
    "let",
    "loop",
    "macro_rules",
    "match",
    "mod",
    "move",
    "mut",
    "pub",
    "ref",
    "return",
    "self",
    "Self",
    "static",
    "struct",
    "super",
    "trait",
    "type",
    "union",
    "unsafe",
    "use",
    "where",
    "while",
];

/// A macro's arguments are a token tree to the Rust grammar. Its injection
/// parses them again as Rust, which reads well when they are expressions
/// and badly when they are items — the bodies of `extern_protocol!` or
/// `define_class!`, where a method's name is left uncoloured. So, as the
/// Tauri app did, lex what the tree pass left plain inside each token tree
/// and colour it by its shape: keywords, `Types`, `CONSTANTS`, numbers, a
/// word that is called (`name(`, `name!`) as a function, and a word before
/// a single colon as a field. Marks the tree pass made are kept.
fn token_tree_marks(doc: &str, out: &mut Vec<(Range<usize>, Syn)>) {
    // Token trees that matter are a macro's: no `!`, none to lex.
    if !doc.contains('!') {
        return;
    }
    let Some(tree) = RUST_PARSER.with(|parser| parser.borrow_mut().parse(doc, None)) else {
        return;
    };
    let mut zones = Vec::new();
    let mut cursor = tree.walk();
    'walk: loop {
        let node = cursor.node();
        if node.kind() == "token_tree" {
            zones.push(node.byte_range());
        } else if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                break 'walk;
            }
        }
    }
    if zones.is_empty() {
        return;
    }
    let taken = std::mem::take(out);
    let mut lexed = Vec::new();
    for zone in zones {
        lex_rust(doc, zone, &mut lexed);
    }
    // Interleave the lexed runs with the tree pass's, keeping only what
    // lies in the gaps between those.
    let mut gaps = Vec::new();
    let mut next = 0;
    for (range, class) in lexed {
        while next < taken.len() && taken[next].0.end <= range.start {
            next += 1;
        }
        let mut at = range.start;
        for (mark, _) in &taken[next..] {
            if mark.start >= range.end {
                break;
            }
            if mark.start > at {
                gaps.push((at..mark.start, class));
            }
            at = at.max(mark.end);
        }
        if at < range.end {
            gaps.push((at..range.end, class));
        }
    }
    *out = taken;
    out.extend(gaps);
    out.sort_by_key(|(range, _)| range.start);
}

/// The marks a Rust-shaped lexer gives the text in `zone`. Strings and
/// comments are skipped whole (the tree pass has them), and so are
/// lifetimes and character literals.
fn lex_rust(doc: &str, zone: Range<usize>, out: &mut Vec<(Range<usize>, Syn)>) {
    let bytes = doc.as_bytes();
    let at = |i: usize| bytes.get(i).copied().filter(|_| i < zone.end);
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = zone.start;
    while i < zone.end {
        let c = bytes[i];
        match c {
            b'"' => {
                i += 1;
                while let Some(b) = at(i) {
                    i += if b == b'\\' { 2 } else { 1 };
                    if b == b'"' {
                        break;
                    }
                }
            }
            b'/' if at(i + 1) == Some(b'/') => {
                while at(i).is_some_and(|b| b != b'\n') {
                    i += 1;
                }
            }
            b'/' if at(i + 1) == Some(b'*') => {
                i = doc[i + 2..zone.end]
                    .find("*/")
                    .map_or(zone.end, |end| i + 2 + end + 2);
            }
            b'\'' => {
                let mut end = i + 1;
                if at(end) == Some(b'\\') {
                    end += 2;
                } else {
                    while at(end).is_some_and(word) {
                        end += 1;
                    }
                }
                i = if at(end) == Some(b'\'') {
                    end + 1
                } else {
                    end.max(i + 1)
                };
            }
            b'#' if at(i + 1) == Some(b'[') => {
                // A nested attribute reads best as one run.
                let mut depth = 0;
                let mut end = i + 1;
                while let Some(b) = at(end) {
                    end += 1;
                    match b {
                        b'[' => depth += 1,
                        b']' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                out.push((i..end, Syn::Meta));
                i = end;
            }
            b'0'..=b'9' => {
                let mut end = i + 1;
                while at(end).is_some_and(|b| word(b) || b == b'.') {
                    end += 1;
                }
                out.push((i..end, Syn::Const));
                i = end;
            }
            _ if word(c) => {
                let mut end = i + 1;
                while at(end).is_some_and(word) {
                    end += 1;
                }
                let text = &doc[i..end];
                let upper = text.starts_with(|c: char| c.is_ascii_uppercase());
                let class = if RUST_KEYWORDS.contains(&text) {
                    Some(Syn::Kw)
                } else if upper
                    && text.contains(|c: char| c.is_ascii_lowercase())
                    && !text.contains('_')
                {
                    Some(Syn::Type)
                } else if upper
                    && text.len() > 1
                    && !text.contains(|c: char| c.is_ascii_lowercase())
                {
                    Some(Syn::Const)
                } else if matches!(at(end), Some(b'!' | b'(')) {
                    Some(Syn::Fn)
                } else if at(end) == Some(b':') && at(end + 1) != Some(b':') {
                    Some(Syn::Prop)
                } else {
                    None
                };
                if let Some(class) = class {
                    out.push((i..end, class));
                }
                i = end;
            }
            _ => i += 1,
        }
    }
}

/// Whether a side of an XML hunk starts its file. An XML document has one
/// root element and its prolog comes first, while a hunk from the middle
/// of a file is element content: text, entities and CDATA between tags
/// that open and close elsewhere. Parsed bare, all of that is an error;
/// inside a stand-in root it reads as it does in the file. A side that
/// starts the file parses as the document it is.
fn starts_document(texts: &[&str]) -> bool {
    texts
        .iter()
        .map(|text| text.trim_start_matches('\u{feff}').trim_start())
        .find(|text| !text.is_empty())
        .is_some_and(|text| text.starts_with("<?xml") || text.starts_with("<!DOCTYPE"))
}

/// What closes a side of a hunk in a language of brackets. A hunk stops
/// mid-file, so a block comment or a brace it opens often closes below
/// it; parsed as it stands, everything after the opening is an error to
/// tree-sitter — the name in `fn name(…) {` loses its colour, a comment's
/// prose reads as code — where the Tauri app's parser closed them itself.
/// The closers go after the side's last line, where they colour nothing.
/// Strings and comments are skipped on the way, so their brackets do not
/// count.
fn closers(lang: Lang, body: &str) -> String {
    let line_comments = !matches!(lang, Lang::Css | Lang::Json);
    let bytes = body.as_bytes();
    let mut open = Vec::new();
    let mut in_comment = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if line_comments && bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => match body[i + 2..].find("*/") {
                Some(end) => i += 2 + end + 1,
                None => {
                    in_comment = true;
                    break;
                }
            },
            // A lifetime is no quote in Rust; elsewhere strings end at the
            // line, unless they are template literals.
            quote @ (b'"' | b'\'' | b'`') if !(quote == b'\'' && lang == Lang::Rust) => {
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    if bytes[i] == b'\n' && quote != b'`' && lang != Lang::Rust {
                        break;
                    }
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'(' => open.push(b')'),
            b'[' => open.push(b']'),
            b'{' => open.push(b'}'),
            close @ (b')' | b']' | b'}') if open.last() == Some(&close) => {
                open.pop();
            }
            _ => {}
        }
        i += 1;
    }
    if open.is_empty() && !in_comment {
        return String::new();
    }
    let mut tail = String::from(if in_comment { "\n*/" } else { "\n" });
    tail.extend(open.iter().rev().map(|&b| char::from(b)));
    tail
}

/// Parse one side of a hunk and distribute the styled ranges onto its
/// lines as line-relative offsets. A token can span lines (a block
/// comment), so ranges clip per line. `None` when the side is too large or
/// the language has no working grammar.
fn side_marks(lang: Lang, texts: &[&str]) -> Option<Vec<Vec<Mark>>> {
    let units: usize = texts
        .iter()
        .map(|text| text.encode_utf16().count() + 1)
        .sum();
    if units > MAX_HIGHLIGHT_UNITS {
        return None;
    }
    let root = lang == Lang::Xml && !starts_document(texts);
    let mut starts = Vec::with_capacity(texts.len());
    let mut doc = String::from(if root { "<_>" } else { "" });
    let body = doc.len();
    for text in texts {
        starts.push(doc.len());
        doc.push_str(text);
        doc.push('\n');
    }
    match lang {
        Lang::Xml if root => doc.push_str("</_>"),
        Lang::Rust | Lang::TypeScript | Lang::Tsx | Lang::JavaScript | Lang::Css | Lang::Json => {
            let tail = closers(lang, &doc[body..]);
            doc.push_str(&tail);
        }
        _ => {}
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
            Lang::Xml,
        ] {
            assert!(config(lang).is_some(), "{lang:?} failed to load");
        }
    }

    /// Each line's styled stretches, for a side of several lines.
    fn classes_in<'a>(lang: Lang, lines: &[&'a str]) -> Vec<Vec<(&'a str, Syn)>> {
        let marks = side_marks(lang, lines).expect("grammar loads");
        lines
            .iter()
            .zip(marks)
            .map(|(line, marks)| {
                marks
                    .iter()
                    .map(|mark| {
                        (
                            &line[mark.range.start as usize..mark.range.end as usize],
                            mark.class,
                        )
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn xml_and_its_dialects_take_the_xml_grammar() {
        for path in [
            "a/b.xml",
            "icon.svg",
            "schema.xsd",
            "Schema.XSD",
            "Info.plist",
        ] {
            assert_eq!(lang_for_path(path), Some(Lang::Xml), "{path}");
        }
        for path in ["index.html", "App.vue"] {
            assert_eq!(lang_for_path(path), Some(Lang::Html), "{path}");
        }
        assert_eq!(lang_for_path("notes.unknown"), None);
        assert_eq!(lang_for_path("LICENSE"), None);
    }

    #[test]
    fn a_schema_hunk_colours_its_namespaced_tags() {
        // From the middle of the file: the root opened above the hunk and
        // closes in it.
        let lines = classes_in(
            Lang::Xml,
            &[
                r#"  <xs:element name="note" type="xs:string"/>"#,
                "  <!-- the end -->",
                "</xs:schema>",
            ],
        );
        assert_eq!(
            lines[0],
            [
                ("xs:element", Syn::Type),
                ("name", Syn::Prop),
                (r#""note""#, Syn::Str),
                ("type", Syn::Prop),
                (r#""xs:string""#, Syn::Str),
            ]
        );
        assert_eq!(lines[1], [("<!-- the end -->", Syn::Comment)]);
        assert_eq!(lines[2], [("xs:schema", Syn::Type)]);
    }

    #[test]
    fn a_plist_hunk_reads_as_element_content() {
        let lines = classes_in(
            Lang::Xml,
            &[
                "    <key>CFBundleName</key>",
                "    <string>Jiji &amp; co</string>",
                "    <true/>",
                "  <![CDATA[ a < b ]]>",
                "</dict>",
            ],
        );
        assert_eq!(lines[0], [("key", Syn::Type), ("key", Syn::Type)]);
        assert!(lines[1].contains(&("&amp;", Syn::Const)), "{:?}", lines[1]);
        assert_eq!(lines[2], [("true", Syn::Type)]);
        assert_eq!(lines[3], [("<![CDATA[ a < b ]]>", Syn::Str)]);
        assert_eq!(lines[4], [("dict", Syn::Type)]);
    }

    #[test]
    fn an_svg_that_starts_its_file_keeps_its_declaration() {
        let lines = classes_in(
            Lang::Xml,
            &[
                r#"<?xml version="1.0" encoding="UTF-8"?>"#,
                r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16">"#,
            ],
        );
        assert!(lines[0].contains(&("xml", Syn::Kw)), "{:?}", lines[0]);
        assert!(lines[0].contains(&("version", Syn::Prop)), "{:?}", lines[0]);
        assert_eq!(
            lines[1],
            [
                ("svg", Syn::Type),
                ("xmlns", Syn::Prop),
                (r#""http://www.w3.org/2000/svg""#, Syn::Str),
                ("viewBox", Syn::Prop),
                (r#""0 0 16 16""#, Syn::Str),
            ]
        );
    }

    #[test]
    fn a_macro_body_of_items_still_colours_its_names() {
        // objc2's bindings: a trait, an attribute and a method signature
        // inside a macro's token tree.
        let lines = classes_in(
            Lang::Rust,
            &[
                "extern_protocol!(",
                "    unsafe trait Providing: NSObjectProtocol {",
                "        #[unsafe(method_family = none)]",
                "        fn anchor(&self, controller: &UIWindow);",
                "    }",
                ");",
            ],
        );
        for expected in [
            ("unsafe", Syn::Kw),
            ("trait", Syn::Kw),
            ("Providing", Syn::Type),
            ("NSObjectProtocol", Syn::Type),
        ] {
            assert!(
                lines[1].contains(&expected),
                "{expected:?} in {:?}",
                lines[1]
            );
        }
        assert!(
            lines[2]
                .iter()
                .any(|(text, class)| text.contains("method_family") && *class == Syn::Meta),
            "{:?}",
            lines[2]
        );
        for expected in [
            ("fn", Syn::Kw),
            ("anchor", Syn::Fn),
            ("controller", Syn::Prop),
            ("UIWindow", Syn::Type),
        ] {
            assert!(
                lines[3].contains(&expected),
                "{expected:?} in {:?}",
                lines[3]
            );
        }
    }

    #[test]
    fn the_size_cutoff_counts_utf16_units() {
        // 75,000 CJK characters are 225,000 bytes of UTF-8 but 75,000
        // units: the Tauri app highlighted them, so this one does too.
        let cjk = format!("const s = \"{}\";", "漢".repeat(75_000));
        let found = classes(Lang::TypeScript, &cjk);
        assert!(found.contains(&("const", Syn::Kw)), "{:?}", &found[..1]);
        assert!(found
            .iter()
            .any(|(text, class)| text.starts_with('"') && *class == Syn::Str));
        // Past the cutoff, nothing highlights.
        let huge = format!("let x = \"{}\";", "y".repeat(MAX_HIGHLIGHT_UNITS));
        assert!(side_marks(Lang::TypeScript, &[&huge]).is_none());
        let fits = "y".repeat(MAX_HIGHLIGHT_UNITS - 1);
        assert!(side_marks(Lang::TypeScript, &[&fits]).is_some());
    }

    #[test]
    fn a_token_cut_by_an_intraline_edit_keeps_its_class() {
        // The edit covers `set", "ran`, cutting both strings: the marks
        // follow the tokens, not the edit.
        let line = r#"let x = ["offset", "range"];"#;
        let found = classes(Lang::TypeScript, line);
        assert!(found.contains(&("let", Syn::Kw)), "{found:?}");
        assert!(found.contains(&(r#""offset""#, Syn::Str)), "{found:?}");
        assert!(found.contains(&(r#""range""#, Syn::Str)), "{found:?}");
    }

    #[test]
    fn context_lines_read_from_the_new_side() {
        // The added line opens a block comment that swallows the context
        // line on the new side only.
        let job = HighlightJob {
            lang: Lang::TypeScript,
            hunks: vec![vec![
                (0, Kind::Added, "/*".into()),
                (1, Kind::Context, "let visible = 1;".into()),
            ]],
        };
        let marks = job.run();
        let context = marks
            .iter()
            .find(|(id, _)| *id == 1)
            .expect("context marks");
        assert_eq!(
            context.1,
            [Mark {
                range: 0..16,
                class: Syn::Comment
            }]
        );
    }

    #[test]
    fn removed_lines_read_from_the_old_side() {
        // On the old side the removed line sits inside a comment; its
        // replacement on the new side is live code.
        let job = HighlightJob {
            lang: Lang::TypeScript,
            hunks: vec![vec![
                (0, Kind::Removed, "/*".into()),
                (1, Kind::Removed, "let gone = 1;".into()),
                (2, Kind::Added, "let kept = 2;".into()),
            ]],
        };
        let marks = job.run();
        let class_of = |id: LineId| {
            marks
                .iter()
                .find(|(line, _)| *line == id)
                .map(|(_, m)| m[0].class)
        };
        assert_eq!(class_of(1), Some(Syn::Comment));
        assert_eq!(class_of(2), Some(Syn::Kw));
    }

    #[test]
    fn familiar_languages_colour_their_landmarks() {
        let rust = classes(Lang::Rust, "pub fn snapshot(&self) -> Result<Repo> {");
        for expected in [
            ("fn", Syn::Kw),
            ("snapshot", Syn::Fn),
            ("Result", Syn::Type),
        ] {
            assert!(rust.contains(&expected), "{expected:?} in {rust:?}");
        }
        let ts = classes(Lang::TypeScript, r#"const greeting = "hello";"#);
        assert!(ts.contains(&("const", Syn::Kw)) && ts.contains(&(r#""hello""#, Syn::Str)));
        let markdown = classes(Lang::Markdown, "# Release notes");
        assert!(
            markdown
                .iter()
                .any(|(text, class)| text.contains("Release notes") && *class == Syn::Heading),
            "{markdown:?}"
        );
        let svelte = classes_in(
            Lang::Svelte,
            &[
                "<script>",
                "  let count = 0;",
                "</script>",
                r#"<button class="pill">{count}</button>"#,
            ],
        );
        assert!(svelte[1].contains(&("let", Syn::Kw)), "{:?}", svelte[1]);
        assert!(
            svelte[3].contains(&("button", Syn::Type)),
            "{:?}",
            svelte[3]
        );
        assert!(svelte[3].contains(&("class", Syn::Prop)), "{:?}", svelte[3]);
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
        let three = side_marks(Lang::Rust, &["/* first", "   second", "   third */"]).unwrap();
        for (marks, len) in three.iter().zip([8, 9, 11]) {
            assert_eq!(
                *marks,
                [Mark {
                    range: 0..len,
                    class: Syn::Comment
                }]
            );
        }
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
