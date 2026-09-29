// Syntax highlighting for the continuous diff: lezer parsers from the
// CodeMirror language packages, applied per hunk and merged with the
// backend's intraline segments. No Svelte.
//
// A hunk is parsed as two small standalone documents — the old side
// (context + removed lines) and the new side (context + added lines) —
// so removed code highlights as it was and added code as it is. Context
// lines take the new side's reading. Constructs that span elided gaps
// (a block comment opened above the hunk) misparse at the hunk edge;
// that is the standard diff-viewer tradeoff and lezer degrades gracefully.
//
// Parsing is lazy per hunk: the windowed renderer (FileDiffCard) asks for
// spans only when a block mounts, so off-screen hunks are never parsed.
// Results are cached by the identity of a line's `segments` array, which
// flows by reference from `hunk.lines` into both layouts' rows — unified
// and split rows of the same line share one computation.

import { highlightTree, tagHighlighter, tags as t } from "@lezer/highlight";
import type { Parser, Tree } from "@lezer/common";
import type { DiffHunk } from "$lib/bindings/DiffHunk";
import type { DiffSegment } from "$lib/bindings/DiffSegment";

/// One renderable run of a line: the intraline `changed` flag from the
/// diff, plus the syntax class covering it (space-separated when a token
/// carries several, null for unstyled text). Splitting happens at both
/// boundaries, so a keyword half-covered by an intraline edit becomes two
/// spans with the same class.
export interface HighlightSpan {
  text: string;
  changed: boolean;
  cls: string | null;
}

// A restrained class set (styled in FileDiffCard from the --syn-* tokens):
// most code stays plain text; color marks the scan anchors. More specific
// tags win, so `function(propertyName)` reads as a call, not a property.
const CLASSES = tagHighlighter([
  { tag: [t.keyword, t.self], class: "syn-kw" },
  { tag: [t.string, t.special(t.string), t.character, t.attributeValue], class: "syn-str" },
  { tag: [t.number, t.bool, t.atom, t.null, t.unit, t.color], class: "syn-const" },
  { tag: [t.typeName, t.className, t.namespace, t.tagName], class: "syn-type" },
  { tag: [t.function(t.variableName), t.function(t.propertyName), t.macroName], class: "syn-fn" },
  { tag: [t.propertyName, t.attributeName], class: "syn-prop" },
  { tag: t.comment, class: "syn-comment" },
  { tag: [t.meta, t.annotation, t.processingInstruction, t.regexp, t.escape], class: "syn-meta" },
  { tag: t.heading, class: "syn-heading" },
  { tag: [t.link, t.url], class: "syn-link" },
  { tag: t.emphasis, class: "syn-em" },
  { tag: t.strong, class: "syn-strong" },
]);

// Everything here ships in the dependency baseline — approximate homes
// (scss under the css grammar, vue under html) beat no highlighting, since
// lezer recovers around constructs it does not know. Notably absent until
// a grammar is worth adding: toml, shell, go, sql.
const LANG_BY_EXT: Record<string, string> = {
  js: "js", mjs: "js", cjs: "js",
  jsx: "jsx",
  ts: "ts", mts: "ts", cts: "ts",
  tsx: "tsx",
  json: "json",
  md: "md", markdown: "md",
  py: "py", pyi: "py",
  rs: "rs",
  xml: "xml", svg: "xml", xsd: "xml", plist: "xml",
  yaml: "yaml", yml: "yaml",
  css: "css", scss: "css", less: "css",
  html: "html", htm: "html", vue: "html",
  svelte: "svelte",
};

const FACTORIES: Record<string, () => Promise<Parser>> = {
  js: async () => (await import("@codemirror/lang-javascript")).javascript().language.parser,
  jsx: async () => (await import("@codemirror/lang-javascript")).javascript({ jsx: true }).language.parser,
  ts: async () => (await import("@codemirror/lang-javascript")).javascript({ typescript: true }).language.parser,
  tsx: async () => (await import("@codemirror/lang-javascript")).javascript({ typescript: true, jsx: true }).language.parser,
  json: async () => (await import("@codemirror/lang-json")).json().language.parser,
  md: async () => (await import("@codemirror/lang-markdown")).markdown().language.parser,
  py: async () => (await import("@codemirror/lang-python")).python().language.parser,
  rs: async () => (await import("@codemirror/lang-rust")).rust().language.parser,
  xml: async () => (await import("@codemirror/lang-xml")).xml().language.parser,
  yaml: async () => (await import("@codemirror/lang-yaml")).yaml().language.parser,
  css: async () => (await import("@codemirror/lang-css")).css().language.parser,
  html: async () => (await import("@codemirror/lang-html")).html().language.parser,
  svelte: async () => (await import("@replit/codemirror-lang-svelte")).svelte().language.parser,
};

// Share both the download and parser construction across visible files.
// A rejected import is retryable; plain diff text stays available throughout.
const parsers = new Map<string, Promise<Parser>>();

async function parserFor(path: string): Promise<{ parser: Parser; lang: string } | null> {
  const extension = path.split("/").pop()?.split(".");
  if (!extension || extension.length < 2) return null;
  const lang = LANG_BY_EXT[extension[extension.length - 1].toLowerCase()];
  if (!lang) return null;
  let pending = parsers.get(lang);
  if (!pending) {
    pending = FACTORIES[lang]();
    parsers.set(lang, pending);
  }
  try {
    return { parser: await pending, lang };
  } catch {
    parsers.delete(lang);
    return null;
  }
}

/// Past this many characters a hunk side skips highlighting (a single
/// hunk can be an entire new file); parsing stays a small, bounded cost
/// on the block-mount path.
export const MAX_HIGHLIGHT_CHARS = 200_000;

function lineText(segments: DiffSegment[]): string {
  let text = "";
  for (const segment of segments) text += segment.text;
  return text;
}

interface Mark {
  from: number;
  to: number;
  cls: string;
}

// Parse one side of a hunk and distribute the styled ranges onto its
// lines (line-relative offsets). A single token can span lines — a block
// comment — so ranges clip per line. null when the side is too large or
// the parser fails; the caller falls back to unstyled spans.
function sideMarks(
  parser: Parser,
  lines: DiffSegment[][],
  lang: string | null,
): Mark[][] | null {
  const texts = lines.map(lineText);
  const starts: number[] = [];
  let offset = 0;
  for (const text of texts) {
    starts.push(offset);
    offset += text.length + 1;
  }
  if (offset > MAX_HIGHLIGHT_CHARS) return null;
  const doc = texts.join("\n");
  const ranges: Mark[] = [];
  try {
    const tree = parser.parse(doc);
    highlightTree(tree, CLASSES, (from, to, cls) =>
      ranges.push({ from, to, cls }),
    );
    if (lang === "rs") rustRawTokenMarks(tree, doc, ranges);
  } catch {
    return null;
  }
  ranges.sort((a, b) => a.from - b.from);
  const marks: Mark[][] = texts.map(() => []);
  let li = 0;
  for (const { from, to, cls } of ranges) {
    while (li + 1 < starts.length && starts[li + 1] <= from) li++;
    for (let i = li; i < texts.length && starts[i] < to; i++) {
      const f = Math.max(from, starts[i]) - starts[i];
      const t2 = Math.min(to, starts[i] + texts[i].length) - starts[i];
      if (t2 > f) marks[i].push({ from: f, to: t2, cls });
    }
  }
  return marks;
}

// lezer-rust parses macro arguments and attribute bodies as raw token
// trees: only strings, comments, and numbers tokenize on their own in
// there, everything else is untagged — so idiomatic macro-heavy Rust
// (objc2 bindings, declarative macros) renders as a wall of plain text.
// Approximate what a full parse would say: lex those interiors with a
// small Rust-shaped tokenizer and add marks in the gaps the tree pass
// left.

const RUST_RAW_NODES = new Set([
  "ParenthesizedTokens",
  "BracketedTokens",
  "BracedTokens",
  "AttributeItem",
]);

const RUST_KEYWORDS = new Set([
  "as", "async", "await", "break", "const", "continue", "crate", "dyn",
  "else", "enum", "extern", "fn", "for", "if", "impl", "in", "let", "loop",
  "macro_rules", "match", "mod", "move", "mut", "pub", "ref", "return",
  "self", "Self", "static", "struct", "super", "trait", "type", "union",
  "unsafe", "use", "where", "while",
]);

function rustRawTokenMarks(tree: Tree, doc: string, ranges: Mark[]): void {
  const zones: { from: number; to: number }[] = [];
  tree.iterate({
    enter(node) {
      if (!RUST_RAW_NODES.has(node.name)) return;
      zones.push({ from: node.from, to: node.to });
      return false; // the outermost zone covers any nested trees
    },
  });
  if (zones.length === 0) return;
  // The tree pass's marks, in document order; the lexer emits only into
  // the stretches between them, so marks stay non-overlapping.
  const taken = [...ranges].sort((a, b) => a.from - b.from);
  let ti = 0;
  const emit = (from: number, to: number, cls: string) => {
    while (ti < taken.length && taken[ti].to <= from) ti++;
    let cur = from;
    for (let j = ti; cur < to; j++) {
      const mark = j < taken.length ? taken[j] : null;
      if (!mark || mark.from >= to) {
        ranges.push({ from: cur, to, cls });
        break;
      }
      if (mark.from > cur) ranges.push({ from: cur, to: mark.from, cls });
      cur = Math.max(cur, mark.to);
    }
  };
  for (const zone of zones) lexRustRange(doc, zone.from, zone.to, emit);
}

/// Past the closing quote of a string starting at `i` (handles escapes).
function skipString(doc: string, i: number, to: number): number {
  for (i++; i < to; i++) {
    if (doc[i] === "\\") i++;
    else if (doc[i] === '"') return i + 1;
  }
  return to;
}

/// Past the `]` matching the `[` at `open` (skips strings; balanced by
/// construction inside a token tree).
function matchBracket(doc: string, open: number, to: number): number {
  let depth = 0;
  for (let i = open; i < to; i++) {
    const c = doc[i];
    if (c === '"') i = skipString(doc, i, to) - 1;
    else if (c === "[") depth++;
    else if (c === "]" && --depth === 0) return i + 1;
  }
  return to;
}

const WORD_CHAR = /[A-Za-z0-9_]/;
const TYPE_NAME = /^[A-Z][A-Za-z0-9]*$/;
const CONST_NAME = /^[A-Z][A-Z0-9_]+$/;

function lexRustRange(
  doc: string,
  from: number,
  to: number,
  emit: (from: number, to: number, cls: string) => void,
): void {
  let i = from;
  while (i < to) {
    const c = doc[i];
    // Strings and comments are already tagged by the tree pass; skip them
    // whole so their contents don't lex as code.
    if (c === '"') {
      i = skipString(doc, i, to);
    } else if (c === "/" && doc[i + 1] === "/") {
      while (i < to && doc[i] !== "\n") i++;
    } else if (c === "/" && doc[i + 1] === "*") {
      const end = doc.indexOf("*/", i + 2);
      i = end === -1 || end + 2 > to ? to : end + 2;
    } else if (c === "'") {
      // Lifetime ('a) or char literal ('x', already tagged): skip either.
      let e = i + 1;
      if (doc[e] === "\\") e += 2;
      else if (WORD_CHAR.test(doc[e] ?? "")) {
        e++;
        while (e < to && WORD_CHAR.test(doc[e])) e++;
      }
      i = doc[e] === "'" ? e + 1 : e;
    } else if (c === "#" && doc[i + 1] === "[") {
      // A nested attribute reads best as one flat run.
      const end = matchBracket(doc, i + 1, to);
      emit(i, end, "syn-meta");
      i = end;
    } else if (c >= "0" && c <= "9") {
      let e = i + 1;
      while (e < to && (WORD_CHAR.test(doc[e]) || doc[e] === ".")) e++;
      emit(i, e, "syn-const");
      i = e;
    } else if (WORD_CHAR.test(c) && !(c >= "0" && c <= "9")) {
      let e = i + 1;
      while (e < to && WORD_CHAR.test(doc[e])) e++;
      const word = doc.slice(i, e);
      let cls: string | null = null;
      if (RUST_KEYWORDS.has(word)) cls = "syn-kw";
      else if (TYPE_NAME.test(word) && /[a-z]/.test(word)) cls = "syn-type";
      else if (CONST_NAME.test(word)) cls = "syn-const";
      else if (doc[e] === "!" || doc[e] === "(") cls = "syn-fn";
      else if (doc[e] === ":" && doc[e + 1] !== ":") cls = "syn-prop";
      if (cls) emit(i, e, cls);
      i = e;
    } else {
      i++;
    }
  }
}

/// Split one line's diff segments at the syntax-token boundaries (marks
/// are line-relative, sorted, non-overlapping). Span texts concatenate
/// back to the line; `changed` follows the segments, `cls` the marks.
export function mergeSpans(
  segments: DiffSegment[],
  marks: Mark[],
): HighlightSpan[] {
  const spans: HighlightSpan[] = [];
  let pos = 0;
  let mi = 0;
  for (const segment of segments) {
    const end = pos + segment.text.length;
    let cur = pos;
    while (cur < end) {
      while (mi < marks.length && marks[mi].to <= cur) mi++;
      const mark = mi < marks.length ? marks[mi] : null;
      let stop: number;
      let cls: string | null;
      if (!mark || mark.from >= end) {
        stop = end;
        cls = null;
      } else if (mark.from > cur) {
        stop = mark.from;
        cls = null;
      } else {
        stop = Math.min(mark.to, end);
        cls = mark.cls;
      }
      spans.push({
        text: segment.text.slice(cur - pos, stop - pos),
        changed: segment.changed,
        cls,
      });
      cur = stop;
    }
    pos = end;
  }
  return spans;
}

export function plainSpans(segments: DiffSegment[]): HighlightSpan[] {
  return segments
    .filter((segment) => segment.text.length > 0)
    .map((segment) => ({ text: segment.text, changed: segment.changed, cls: null }));
}

function highlightHunk(
  parser: Parser,
  lang: string,
  hunk: DiffHunk,
  cache: WeakMap<DiffSegment[], HighlightSpan[]>,
): void {
  const oldLines = hunk.lines.filter((l) => l.kind !== "added");
  const newLines = hunk.lines.filter((l) => l.kind !== "removed");
  // The old side only matters for removed lines; a pure-add hunk skips it.
  const oldMarks = oldLines.some((l) => l.kind === "removed")
    ? sideMarks(parser, oldLines.map((l) => l.segments), lang)
    : null;
  oldLines.forEach((line, i) => {
    if (line.kind === "removed") {
      cache.set(line.segments, mergeSpans(line.segments, oldMarks?.[i] ?? []));
    }
  });
  const newMarks = sideMarks(parser, newLines.map((l) => l.segments), lang);
  newLines.forEach((line, i) => {
    cache.set(line.segments, mergeSpans(line.segments, newMarks?.[i] ?? []));
  });
}

/// The per-file lookup the renderer calls with each line's segments.
/// Nothing is parsed until a line is first requested; the whole hunk it
/// belongs to highlights then, so scrolling into a hunk costs one parse.
/// Unknown languages (and lines from another file) fall back to unstyled
/// spans of the same shape. Results are cached by segments-array identity,
/// so repeated renders of a mounted block are lookups.
export async function fileHighlighter(
  path: string,
  hunks: DiffHunk[],
): Promise<(segments: DiffSegment[]) => HighlightSpan[]> {
  const cache = new WeakMap<DiffSegment[], HighlightSpan[]>();
  const lang = await parserFor(path);
  const hunkOf = new WeakMap<DiffSegment[], DiffHunk>();
  const parsed = new WeakSet<DiffHunk>();
  if (lang) {
    for (const hunk of hunks) {
      for (const line of hunk.lines) hunkOf.set(line.segments, hunk);
    }
  }
  return (segments) => {
    let spans = cache.get(segments);
    if (spans) return spans;
    if (lang) {
      const hunk = hunkOf.get(segments);
      if (hunk && !parsed.has(hunk)) {
        parsed.add(hunk);
        highlightHunk(lang.parser, lang.lang, hunk, cache);
        spans = cache.get(segments);
      }
    }
    if (!spans) {
      spans = plainSpans(segments);
      cache.set(segments, spans);
    }
    return spans;
  };
}
