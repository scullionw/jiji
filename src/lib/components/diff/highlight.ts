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

import { css } from "@codemirror/lang-css";
import { html } from "@codemirror/lang-html";
import { javascript } from "@codemirror/lang-javascript";
import { json } from "@codemirror/lang-json";
import { markdown } from "@codemirror/lang-markdown";
import { python } from "@codemirror/lang-python";
import { rust } from "@codemirror/lang-rust";
import { xml } from "@codemirror/lang-xml";
import { yaml } from "@codemirror/lang-yaml";
import { highlightTree, tagHighlighter, tags as t } from "@lezer/highlight";
import type { Parser } from "@lezer/common";
import { svelte } from "@replit/codemirror-lang-svelte";
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

const FACTORIES: Record<string, () => Parser> = {
  js: () => javascript().language.parser,
  jsx: () => javascript({ jsx: true }).language.parser,
  ts: () => javascript({ typescript: true }).language.parser,
  tsx: () => javascript({ typescript: true, jsx: true }).language.parser,
  json: () => json().language.parser,
  md: () => markdown().language.parser,
  py: () => python().language.parser,
  rs: () => rust().language.parser,
  xml: () => xml().language.parser,
  yaml: () => yaml().language.parser,
  css: () => css().language.parser,
  html: () => html().language.parser,
  svelte: () => svelte().language.parser,
};

const parsers = new Map<string, Parser>();

function parserFor(path: string): Parser | null {
  const dot = path.lastIndexOf(".");
  if (dot < 0) return null;
  const lang = LANG_BY_EXT[path.slice(dot + 1).toLowerCase()];
  if (!lang) return null;
  let parser = parsers.get(lang);
  if (!parser) {
    parser = FACTORIES[lang]();
    parsers.set(lang, parser);
  }
  return parser;
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
function sideMarks(parser: Parser, lines: DiffSegment[][]): Mark[][] | null {
  const texts = lines.map(lineText);
  const starts: number[] = [];
  let offset = 0;
  for (const text of texts) {
    starts.push(offset);
    offset += text.length + 1;
  }
  if (offset > MAX_HIGHLIGHT_CHARS) return null;
  const marks: Mark[][] = texts.map(() => []);
  let li = 0;
  try {
    highlightTree(parser.parse(texts.join("\n")), CLASSES, (from, to, cls) => {
      while (li + 1 < starts.length && starts[li + 1] <= from) li++;
      for (let i = li; i < texts.length && starts[i] < to; i++) {
        const f = Math.max(from, starts[i]) - starts[i];
        const t2 = Math.min(to, starts[i] + texts[i].length) - starts[i];
        if (t2 > f) marks[i].push({ from: f, to: t2, cls });
      }
    });
  } catch {
    return null;
  }
  return marks;
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

function plainSpans(segments: DiffSegment[]): HighlightSpan[] {
  return segments
    .filter((segment) => segment.text.length > 0)
    .map((segment) => ({ text: segment.text, changed: segment.changed, cls: null }));
}

function highlightHunk(
  parser: Parser,
  hunk: DiffHunk,
  cache: WeakMap<DiffSegment[], HighlightSpan[]>,
): void {
  const oldLines = hunk.lines.filter((l) => l.kind !== "added");
  const newLines = hunk.lines.filter((l) => l.kind !== "removed");
  // The old side only matters for removed lines; a pure-add hunk skips it.
  const oldMarks = oldLines.some((l) => l.kind === "removed")
    ? sideMarks(parser, oldLines.map((l) => l.segments))
    : null;
  oldLines.forEach((line, i) => {
    if (line.kind === "removed") {
      cache.set(line.segments, mergeSpans(line.segments, oldMarks?.[i] ?? []));
    }
  });
  const newMarks = sideMarks(parser, newLines.map((l) => l.segments));
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
export function fileHighlighter(
  path: string,
  hunks: DiffHunk[],
): (segments: DiffSegment[]) => HighlightSpan[] {
  const cache = new WeakMap<DiffSegment[], HighlightSpan[]>();
  const parser = parserFor(path);
  const hunkOf = new WeakMap<DiffSegment[], DiffHunk>();
  const parsed = new WeakSet<DiffHunk>();
  if (parser) {
    for (const hunk of hunks) {
      for (const line of hunk.lines) hunkOf.set(line.segments, hunk);
    }
  }
  return (segments) => {
    let spans = cache.get(segments);
    if (spans) return spans;
    if (parser) {
      const hunk = hunkOf.get(segments);
      if (hunk && !parsed.has(hunk)) {
        parsed.add(hunk);
        highlightHunk(parser, hunk, cache);
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
