import { describe, expect, it } from "vitest";
import type { DiffHunk } from "$lib/bindings/DiffHunk";
import type { DiffLine } from "$lib/bindings/DiffLine";
import type { DiffLineKind } from "$lib/bindings/DiffLineKind";
import {
  fileHighlighter,
  mergeSpans,
  MAX_HIGHLIGHT_CHARS,
  type HighlightSpan,
} from "./highlight";

function line(kind: DiffLineKind, text: string): DiffLine {
  return { kind, segments: [{ text, changed: kind !== "context" }], pair: null };
}

function hunk(lines: DiffLine[]): DiffHunk {
  return { oldStart: 1, newStart: 1, context: null, lines };
}

/// The class covering `text` within `spans`, or null when unstyled.
function clsOf(spans: HighlightSpan[], text: string): string | null {
  const span = spans.find((s) => s.text.includes(text));
  expect(span, `no span containing ${JSON.stringify(text)}`).toBeDefined();
  return span!.cls;
}

function joined(spans: HighlightSpan[]): string {
  return spans.map((s) => s.text).join("");
}

describe("fileHighlighter", () => {
  it("classifies tokens for a known language", async () => {
    const added = line("added", 'const greeting = "hello";');
    const hl = await fileHighlighter("src/a.ts", [hunk([added])]);
    const spans = hl(added.segments);
    expect(joined(spans)).toBe('const greeting = "hello";');
    expect(clsOf(spans, "const")).toBe("syn-kw");
    expect(clsOf(spans, '"hello"')).toBe("syn-str");
    expect(spans.every((s) => s.changed)).toBe(true);
  });

  it("highlights inside Rust macro token trees and attributes", async () => {
    // lezer-rust leaves macro arguments and attribute bodies as raw token
    // trees; the fallback lexer must still classify their contents.
    const lines = [
      line("added", "extern_protocol!("),
      line("added", "    unsafe trait Providing: NSObjectProtocol {"),
      line("added", "        #[unsafe(method_family = none)]"),
      line("added", "        fn anchor(&self, controller: &UIWindow);"),
      line("added", "    }"),
      line("added", ");"),
    ];
    const hl = await fileHighlighter("src/a.rs", [hunk(lines)]);
    const trait_ = hl(lines[1].segments);
    expect(clsOf(trait_, "unsafe")).toBe("syn-kw");
    expect(clsOf(trait_, "trait")).toBe("syn-kw");
    expect(clsOf(trait_, "Providing")).toBe("syn-type");
    expect(clsOf(trait_, "NSObjectProtocol")).toBe("syn-type");
    expect(joined(trait_)).toBe("    unsafe trait Providing: NSObjectProtocol {");
    expect(clsOf(hl(lines[2].segments), "method_family")).toBe("syn-meta");
    const fn_ = hl(lines[3].segments);
    expect(clsOf(fn_, "fn")).toBe("syn-kw");
    expect(clsOf(fn_, "anchor")).toBe("syn-fn");
    expect(clsOf(fn_, "controller")).toBe("syn-prop");
    expect(clsOf(fn_, "UIWindow")).toBe("syn-type");
  });

  it("returns unstyled spans of the same shape for unknown extensions", async () => {
    const added = line("added", "fn main() {}");
    const hl = await fileHighlighter("Cargo.lock", [hunk([added])]);
    expect(hl(added.segments)).toEqual([
      { text: "fn main() {}", changed: true, cls: null },
    ]);
  });

  it("splits at both intraline and token boundaries", async () => {
    // The intraline edit covers `set", ran` — cutting both string tokens.
    const edited: DiffLine = {
      kind: "added",
      segments: [
        { text: 'let x = ["off', changed: false },
        { text: 'set", "ran', changed: true },
        { text: 'ge"];', changed: false },
      ],
      pair: null,
    };
    const hl = await fileHighlighter("src/a.ts", [hunk([edited])]);
    const spans = hl(edited.segments);
    expect(joined(spans)).toBe('let x = ["offset", "range"];');
    // The first string is one token split by the segment boundary: same
    // class on both sides, changed flag differing.
    expect(spans).toContainEqual({ text: '"off', changed: false, cls: "syn-str" });
    expect(spans).toContainEqual({ text: 'set"', changed: true, cls: "syn-str" });
    expect(spans).toContainEqual({ text: '"ran', changed: true, cls: "syn-str" });
    expect(spans).toContainEqual({ text: 'ge"', changed: false, cls: "syn-str" });
    expect(clsOf(spans, "let")).toBe("syn-kw");
  });

  it("reads context lines from the new side of the hunk", async () => {
    // The added line opens a block comment that swallows the context line
    // on the new side only; the old side (context alone) is plain code.
    const opened = line("added", "/*");
    const context = line("context", "let visible = 1;");
    const hl = await fileHighlighter("src/a.ts", [hunk([opened, context])]);
    expect(clsOf(hl(context.segments), "let visible")).toBe("syn-comment");
  });

  it("reads removed lines from the old side of the hunk", async () => {
    // On the old side the removed line sits inside a comment; the added
    // replacement on the new side is live code.
    const openRemoved = line("removed", "/*");
    const bodyRemoved = line("removed", "let gone = 1;");
    const replacement = line("added", "let kept = 2;");
    const hl = await fileHighlighter("src/a.ts", [
      hunk([openRemoved, bodyRemoved, replacement]),
    ]);
    expect(clsOf(hl(bodyRemoved.segments), "let gone")).toBe("syn-comment");
    expect(clsOf(hl(replacement.segments), "let")).toBe("syn-kw");
  });

  it("carries a multi-line token across every covered line", async () => {
    const lines = [
      line("added", "/* first"),
      line("added", "   second"),
      line("added", "   third */"),
    ];
    const hl = await fileHighlighter("src/a.rs", [hunk(lines)]);
    for (const l of lines) {
      expect(hl(l.segments)).toEqual([
        { text: joined(hl(l.segments)), changed: true, cls: "syn-comment" },
      ]);
    }
  });

  it("returns the cached spans array on repeated lookups", async () => {
    const added = line("added", "let x = 1;");
    const hl = await fileHighlighter("src/a.ts", [hunk([added])]);
    expect(hl(added.segments)).toBe(hl(added.segments));
  });

  it("skips highlighting when a hunk side exceeds the size cap", async () => {
    const huge = line("added", `let x = "${"y".repeat(MAX_HIGHLIGHT_CHARS)}";`);
    const hl = await fileHighlighter("src/a.ts", [hunk([huge])]);
    expect(hl(huge.segments).every((s) => s.cls === null)).toBe(true);
  });

  it("highlights rust", async () => {
    const added = line("added", "pub fn snapshot(&self) -> Result<Repo> {");
    const hl = await fileHighlighter("crates/jiji-core/src/jj.rs", [hunk([added])]);
    const spans = hl(added.segments);
    expect(clsOf(spans, "fn")).toBe("syn-kw");
    expect(clsOf(spans, "snapshot")).toBe("syn-fn");
    expect(clsOf(spans, "Result")).toBe("syn-type");
  });

  it("highlights svelte markup, script, and style regions", async () => {
    const lines = [
      line("added", "<script>"),
      line("added", "  let count = 0;"),
      line("added", "</script>"),
      line("added", '<button class="pill">{count}</button>'),
    ];
    const hl = await fileHighlighter("src/App.svelte", [hunk(lines)]);
    expect(clsOf(hl(lines[1].segments), "let")).toBe("syn-kw");
    expect(clsOf(hl(lines[3].segments), "button")).toBe("syn-type");
    expect(clsOf(hl(lines[3].segments), "class")).toBe("syn-prop");
  });

  it("highlights markdown structure", async () => {
    const heading = line("added", "# Release notes");
    const hl = await fileHighlighter("README.md", [hunk([heading])]);
    expect(clsOf(hl(heading.segments), "Release notes")).toContain(
      "syn-heading",
    );
  });

  it("only affects the requested file's own hunks", async () => {
    const known = line("added", "let x = 1;");
    const foreign = line("added", "let y = 2;");
    const hl = await fileHighlighter("src/a.ts", [hunk([known])]);
    // Segments from some other file's diff: unstyled, not an error.
    expect(hl(foreign.segments)).toEqual([
      { text: "let y = 2;", changed: true, cls: null },
    ]);
  });
});

describe("mergeSpans", () => {
  it("keeps empty segments from emitting empty spans", async () => {
    expect(mergeSpans([{ text: "", changed: true }], [])).toEqual([]);
  });

  it("emits unstyled spans between marks", async () => {
    const spans = mergeSpans(
      [{ text: "abcdef", changed: false }],
      [
        { from: 1, to: 3, cls: "syn-kw" },
        { from: 4, to: 5, cls: "syn-str" },
      ],
    );
    expect(spans).toEqual([
      { text: "a", changed: false, cls: null },
      { text: "bc", changed: false, cls: "syn-kw" },
      { text: "d", changed: false, cls: null },
      { text: "e", changed: false, cls: "syn-str" },
      { text: "f", changed: false, cls: null },
    ]);
  });
});
