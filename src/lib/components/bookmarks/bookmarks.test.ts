import { describe, expect, it } from "vitest";

import type { BookmarkState } from "$lib/bindings/BookmarkState";
import { bookmarkKey, groupBookmarks, mergedBookmarks } from "./bookmarks";

function bookmark(
  name: string,
  overrides: Partial<BookmarkState> = {},
): BookmarkState {
  return {
    name,
    target: "abcd1234",
    remote: null,
    sync: "localOnly",
    isTrunk: false,
    isLocal: true,
    tracked: false,
    isMerged: false,
    targetDescription: "",
    targetAuthor: "",
    targetTimestamp: "",
    ...overrides,
  };
}

const fixture: BookmarkState[] = [
  bookmark("feature-x", { targetTimestamp: "2026-06-10T10:00:00Z" }),
  bookmark("main", {
    isTrunk: true,
    tracked: true,
    remote: "origin",
    sync: "synced",
    targetTimestamp: "2026-06-01T10:00:00Z",
  }),
  bookmark("landed", {
    isMerged: true,
    targetTimestamp: "2026-05-01T10:00:00Z",
  }),
  bookmark("alice/api", {
    isLocal: false,
    remote: "origin",
    sync: "remoteOnly",
    targetAuthor: "alice",
    targetTimestamp: "2026-06-09T10:00:00Z",
  }),
  bookmark("old-spike", {
    isLocal: false,
    remote: "upstream",
    sync: "remoteOnly",
    isMerged: true,
    targetTimestamp: "2026-04-01T10:00:00Z",
  }),
];

describe("groupBookmarks", () => {
  it("splits local from per-remote groups, trunk pinned, newest first", () => {
    const groups = groupBookmarks(fixture, "");
    expect(groups.local.map((b) => b.name)).toEqual([
      "main",
      "feature-x",
      "landed",
    ]);
    expect(groups.remotes.map((g) => g.remote)).toEqual([
      "origin",
      "upstream",
    ]);
    expect(groups.remotes[0].bookmarks.map((b) => b.name)).toEqual([
      "alice/api",
    ]);
  });

  it("filters across name, author, and remote", () => {
    expect(
      groupBookmarks(fixture, "alice").remotes[0].bookmarks.map((b) => b.name),
    ).toEqual(["alice/api"]);
    expect(groupBookmarks(fixture, "alice").local).toEqual([]);
    expect(groupBookmarks(fixture, "upstream").remotes).toHaveLength(1);
    expect(
      groupBookmarks(fixture, "feature").local.map((b) => b.name),
    ).toEqual(["feature-x"]);
  });
});

describe("mergedBookmarks", () => {
  it("lists cleanup candidates, locals first", () => {
    expect(mergedBookmarks(fixture).map((b) => b.name)).toEqual([
      "landed",
      "old-spike",
    ]);
  });
});

describe("bookmarkKey", () => {
  it("keeps same-name local and remote entries distinct", () => {
    expect(bookmarkKey(bookmark("x"))).not.toBe(
      bookmarkKey(bookmark("x", { isLocal: false, remote: "origin" })),
    );
  });
});
