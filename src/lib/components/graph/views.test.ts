import { describe, expect, it } from "vitest";

import {
  deleteSavedView,
  parseSavedViews,
  parseStoredView,
  renameSavedView,
  sameView,
  upsertSavedView,
  viewLabel,
  VIEW_PRESETS,
} from "./views";

describe("viewLabel", () => {
  it("names every preset and custom", () => {
    for (const preset of VIEW_PRESETS) {
      expect(viewLabel(preset.view)).toBe(preset.label);
    }
    expect(viewLabel({ kind: "custom", revset: "mine()" })).toBe(
      "Custom revset",
    );
  });
});

describe("sameView", () => {
  it("matches presets by kind and customs by revset", () => {
    expect(sameView({ kind: "team" }, { kind: "team" })).toBe(true);
    expect(sameView({ kind: "team" }, { kind: "all" })).toBe(false);
    expect(
      sameView(
        { kind: "custom", revset: "mine()" },
        { kind: "custom", revset: "mine()" },
      ),
    ).toBe(true);
    expect(
      sameView(
        { kind: "custom", revset: "mine()" },
        { kind: "custom", revset: "all()" },
      ),
    ).toBe(false);
  });
});

describe("parseStoredView", () => {
  it("round-trips presets and customs", () => {
    expect(parseStoredView(JSON.stringify({ kind: "trunk" }))).toEqual({
      kind: "trunk",
    });
    expect(
      parseStoredView(JSON.stringify({ kind: "custom", revset: "mine()" })),
    ).toEqual({ kind: "custom", revset: "mine()" });
  });

  it("refuses garbage from older builds or hand edits", () => {
    expect(parseStoredView(null)).toBeNull();
    expect(parseStoredView("not json")).toBeNull();
    expect(parseStoredView(JSON.stringify("team"))).toBeNull();
    expect(parseStoredView(JSON.stringify({ kind: "nope" }))).toBeNull();
    // A custom lens without a usable revset is not worth restoring.
    expect(parseStoredView(JSON.stringify({ kind: "custom" }))).toBeNull();
    expect(
      parseStoredView(JSON.stringify({ kind: "custom", revset: "  " })),
    ).toBeNull();
  });
});

describe("parseSavedViews", () => {
  it("keeps well-formed entries in order", () => {
    expect(
      parseSavedViews([
        { name: "WIP", revset: 'description("wip")' },
        { name: "Big merges", revset: "merges() & mine()" },
      ]),
    ).toEqual([
      { name: "WIP", revset: 'description("wip")' },
      { name: "Big merges", revset: "merges() & mine()" },
    ]);
  });

  it("drops malformed entries and later duplicate names", () => {
    expect(
      parseSavedViews([
        "not an object",
        { name: "ok", revset: "mine()" },
        { name: "no revset" },
        { name: "  ", revset: "all()" },
        { name: "blank revset", revset: "  " },
        { name: 7, revset: "all()" },
        { name: " ok ", revset: "all()" },
      ]),
    ).toEqual([{ name: "ok", revset: "mine()" }]);
    expect(parseSavedViews(null)).toEqual([]);
    expect(parseSavedViews({ name: "x", revset: "y" })).toEqual([]);
  });
});

describe("upsertSavedView", () => {
  const views = [{ name: "WIP", revset: 'description("wip")' }];

  it("appends new names and trims both fields", () => {
    expect(upsertSavedView(views, " Mine ", " mine() ")).toEqual([
      ...views,
      { name: "Mine", revset: "mine()" },
    ]);
  });

  it("replaces an existing name's revset in place", () => {
    const two = [...views, { name: "Mine", revset: "mine()" }];
    expect(upsertSavedView(two, "WIP", "empty()")).toEqual([
      { name: "WIP", revset: "empty()" },
      { name: "Mine", revset: "mine()" },
    ]);
  });

  it("refuses blank names and revsets", () => {
    expect(upsertSavedView(views, "  ", "mine()")).toBeNull();
    expect(upsertSavedView(views, "Mine", "  ")).toBeNull();
  });
});

describe("renameSavedView", () => {
  const views = [
    { name: "WIP", revset: 'description("wip")' },
    { name: "Mine", revset: "mine()" },
  ];

  it("renames in place, keeping order and revset", () => {
    expect(renameSavedView(views, "WIP", " In progress ")).toEqual([
      { name: "In progress", revset: 'description("wip")' },
      { name: "Mine", revset: "mine()" },
    ]);
  });

  it("allows a self-rename (trim only)", () => {
    expect(renameSavedView(views, "WIP", "WIP")).toEqual(views);
  });

  it("refuses missing targets, blank names, and collisions", () => {
    expect(renameSavedView(views, "gone", "New")).toBeNull();
    expect(renameSavedView(views, "WIP", "   ")).toBeNull();
    expect(renameSavedView(views, "WIP", "Mine")).toBeNull();
  });
});

describe("deleteSavedView", () => {
  it("removes by name and leaves the rest", () => {
    const views = [
      { name: "WIP", revset: 'description("wip")' },
      { name: "Mine", revset: "mine()" },
    ];
    expect(deleteSavedView(views, "WIP")).toEqual([
      { name: "Mine", revset: "mine()" },
    ]);
    expect(deleteSavedView(views, "gone")).toEqual(views);
  });
});
