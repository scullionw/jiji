// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it } from "vitest";
import type { BookmarkState } from "$lib/bindings/BookmarkState";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import { app } from "$lib/state/app.svelte";
import Inspector from "./Inspector.svelte";

function bookmark(
  name: string,
  overrides: Partial<BookmarkState> = {},
): BookmarkState {
  return {
    name,
    target: "change",
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

function snapshot(bookmarks: BookmarkState[]): RepoSnapshot {
  return {
    repoPath: "/tmp/repo",
    repoName: "repo",
    backend: "mock",
    view: { kind: "workstreams" },
    truncated: false,
    trunkBookmark: "",
    workingCopy: "",
    workspaces: [],
    workstreams: [],
    nodes: [],
    bookmarks,
    gitRemotes: [],
    conflicts: [],
    operations: [],
    resolveTool: null,
  };
}

afterEach(() => {
  cleanup();
  app.snapshot = null;
});

describe("Inspector bookmarks", () => {
  it("renders and edits a local bookmark when a remote bookmark shares its name", async () => {
    app.snapshot = snapshot([
      bookmark("ws/logjob", { target: "local-change" }),
      bookmark("ws/logjob", {
        target: "remote-change",
        remote: "origin",
        sync: "remoteOnly",
        isLocal: false,
      }),
    ]);

    const view = render(Inspector);

    expect(screen.getAllByText("ws/logjob")).toHaveLength(2);
    await fireEvent.click(screen.getByTitle("Rename ws/logjob"));
    expect(view.container.querySelectorAll("input.bm-input")).toHaveLength(1);
  });
});
