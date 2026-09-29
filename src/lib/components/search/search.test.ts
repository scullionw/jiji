import { describe, expect, it } from "vitest";
import type { HistorySearch } from "$lib/bindings/HistorySearch";
import type { SearchFileHit } from "$lib/bindings/SearchFileHit";
import { fileHitLabel, searchStatus } from "./search";

function fileHit(overrides: Partial<SearchFileHit> = {}): SearchFileHit {
  return { path: "src/a.rs", added: 0, removed: 0, pathMatch: false, ...overrides };
}

function result(overrides: Partial<HistorySearch> = {}): HistorySearch {
  return { query: "needle", hits: [], scanned: 42, truncated: false, ...overrides };
}

describe("fileHitLabel", () => {
  it("reports the occurrence delta with its direction", () => {
    expect(fileHitLabel(fileHit({ added: 3, removed: 1 }))).toEqual({
      text: "+2",
      tone: "add",
    });
    expect(fileHitLabel(fileHit({ added: 0, removed: 2 }))).toEqual({
      text: "−2",
      tone: "del",
    });
  });

  it("labels a pure filename match as a name hit", () => {
    expect(fileHitLabel(fileHit({ pathMatch: true }))).toEqual({
      text: "name",
      tone: "name",
    });
  });
});

describe("searchStatus", () => {
  it("counts matches and coverage", () => {
    const one = result({ hits: [{} as HistorySearch["hits"][number]], scanned: 1 });
    expect(searchStatus(one)).toBe("1 matching change · searched 1 change");
    expect(searchStatus(result())).toBe(
      "0 matching changes · searched 42 changes",
    );
  });

  it("says when the walk stopped early", () => {
    expect(searchStatus(result({ truncated: true }))).toContain(
      "stopped early",
    );
  });
});
