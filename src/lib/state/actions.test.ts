// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { SnapshotView } from "$lib/bindings/SnapshotView";
import * as api from "$lib/api";
import { persistView, persistedView } from "$lib/components/graph/views";
import {
  activateTab,
  applySnapshot,
  closeTab,
  forgetRecentRepo,
  openDroppedPaths,
  openRepoAt,
  reopenClosedTab,
} from "./actions";
import { ask } from "@tauri-apps/plugin-dialog";
import { forgetRepo } from "./recent";
import { app } from "./app.svelte";
import { createTab, tabs } from "./tabs.svelte";

vi.mock("$lib/api", () => ({
  openRepo: vi.fn(),
  initJjRepo: vi.fn(),
  closeRepo: vi.fn(async () => {}),
  currentSnapshot: vi.fn(async () => null),
  refreshSnapshot: vi.fn(async () => {}),
  setSnapshotView: vi.fn(),
  errorMessage: (error: unknown) => String(error),
}));
vi.mock("./recent", () => ({
  loadRecentRepos: vi.fn(async () => []),
  rememberRepo: vi.fn(async () => []),
  forgetRepo: vi.fn(async () => []),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(), ask: vi.fn() }));
vi.mock("./autoland.svelte", () => ({ attachAutoLand: vi.fn() }));
vi.mock("./cli.svelte", () => ({ syncCliStatus: vi.fn() }));
vi.mock("./editor.svelte", () => ({ syncEditors: vi.fn() }));
vi.mock("./terminal.svelte", () => ({ syncTerminals: vi.fn() }));
vi.mock("./forge.svelte", () => ({ refreshForgePrs: vi.fn() }));

function snapshot(repoPath: string, view: SnapshotView = { kind: "workstreams" }): RepoSnapshot {
  return {
    repoPath,
    repoName: repoPath.split("/").pop()!,
    backend: "mock",
    view,
    truncated: false,
    trunkBookmark: "main",
    workingCopy: "wc",
    workspaces: [],
    workstreams: [],
    nodes: [],
    bookmarks: [],
    gitRemotes: [],
    conflicts: [],
    operations: [],
    resolveTool: null,
  };
}

// A promise the test settles by hand, to hold an open mid-flight.
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  tabs.all = [];
  tabs.activeId = 0;
  tabs.closed = [];
  app.snapshot = null;
  app.error = null;
});

describe("applySnapshot", () => {
  function onScreen(path: string) {
    const tab = createTab(path, "repo");
    tab.snapshot = snapshot(path);
    tabs.activeId = tab.id;
    app.snapshot = tab.snapshot;
    return tab;
  }

  it("keeps the snapshot on screen when an identical one republishes", () => {
    const tab = onScreen("/repos/a");
    const shown = tab.snapshot;
    app.error = "refresh failed";

    applySnapshot(snapshot("/repos/a"));

    expect(tab.snapshot).toBe(shown);
    expect(app.snapshot).toBe(shown);
    // A successful republish still clears a stale error.
    expect(app.error).toBeNull();
  });

  it("replaces the snapshot when anything in it changed", () => {
    const tab = onScreen("/repos/a");
    const next = snapshot("/repos/a");
    next.operations = [
      { id: "op2", description: "new change", timestamp: "", isCurrent: true,
        user: "Test", isSnapshot: false, effects: [], moreEffects: 0 },
    ];

    applySnapshot(next);

    expect(tab.snapshot).toBe(next);
    expect(app.snapshot).toBe(next);
  });
});

describe("opening a plain Git repository", () => {
  const refusal = {
    code: "git_repo_without_jj",
    message: "/repos/plain is a Git repository that jj is not set up in yet",
    path: "/repos/plain",
  };

  it("sets jj up at the Git root after a yes, then opens it", async () => {
    vi.mocked(ask).mockResolvedValueOnce(true);
    vi.mocked(api.openRepo)
      .mockRejectedValueOnce(refusal)
      .mockResolvedValueOnce(snapshot("/repos/plain"));
    vi.mocked(api.initJjRepo).mockResolvedValueOnce("/repos/plain");

    await openRepoAt("/repos/plain/src");

    expect(ask).toHaveBeenCalledOnce();
    expect(api.initJjRepo).toHaveBeenCalledWith("/repos/plain");
    expect(api.openRepo).toHaveBeenLastCalledWith("/repos/plain");
    expect(tabs.all.find((t) => t.id === tabs.activeId)?.path).toBe("/repos/plain");
    expect(app.error).toBeNull();
    expect(app.opening).toBe(false);
  });

  it("leaves the folder alone when the user declines", async () => {
    vi.mocked(ask).mockResolvedValueOnce(false);
    vi.mocked(api.openRepo).mockRejectedValueOnce(refusal);
    vi.mocked(api.initJjRepo).mockClear();

    await openRepoAt("/repos/plain");

    expect(api.initJjRepo).not.toHaveBeenCalled();
    expect(app.error).not.toBeNull();
    expect(app.opening).toBe(false);
  });
});

describe("openRepoAt while the tab loses the screen", () => {
  it("re-applies the sticky lens to the opened repo, not the active one", async () => {
    const other = createTab("/repos/other", "other");
    other.snapshot = snapshot("/repos/other");
    tabs.activeId = other.id;
    app.snapshot = other.snapshot;
    persistView("/repos/fresh", { kind: "all" });

    const pending = deferred<RepoSnapshot>();
    vi.mocked(api.openRepo).mockReturnValue(pending.promise);
    vi.mocked(api.setSnapshotView).mockImplementation(async (view, path) =>
      snapshot(path!, view),
    );

    const opening = openRepoAt("/repos/fresh");
    const fresh = tabs.all.find((t) => t.id !== other.id)!;
    expect(tabs.activeId).toBe(fresh.id);
    // The user goes back to the other repo before the open lands.
    await activateTab(other.id);
    pending.resolve(snapshot("/repos/fresh"));
    await opening;

    expect(api.setSnapshotView).toHaveBeenCalledWith({ kind: "all" }, "/repos/fresh");
    expect(fresh.snapshot?.repoPath).toBe("/repos/fresh");
    expect(fresh.snapshot?.view).toEqual({ kind: "all" });
    expect(other.snapshot?.view).toEqual({ kind: "workstreams" });
    expect(app.snapshot?.repoPath).toBe("/repos/other");
  });

  it("drops the backend session of a tab closed mid-open", async () => {
    // A lone empty tab never closes, so another tab keeps the strip.
    const other = createTab("/repos/other", "other");
    other.snapshot = snapshot("/repos/other");
    tabs.activeId = other.id;
    app.snapshot = other.snapshot;
    const pending = deferred<RepoSnapshot>();
    vi.mocked(api.openRepo).mockReturnValue(pending.promise);

    const opening = openRepoAt("/repos/fresh");
    closeTab(tabs.activeId);
    pending.resolve(snapshot("/repos/fresh"));
    await opening;

    expect(api.closeRepo).toHaveBeenCalledWith("/repos/fresh");
    expect(tabs.all.map((t) => t.path)).toEqual(["/repos/other"]);
  });

  it("keeps the sticky lens when the tab closes before it re-applies", async () => {
    persistView("/repos/fresh", { kind: "mine" });
    vi.mocked(api.openRepo).mockResolvedValue(snapshot("/repos/fresh"));
    const lens = deferred<RepoSnapshot>();
    vi.mocked(api.setSnapshotView).mockReturnValue(lens.promise);

    const opening = openRepoAt("/repos/fresh");
    await vi.waitFor(() => expect(api.setSnapshotView).toHaveBeenCalled());
    closeTab(tabs.activeId);
    // The closed session refuses the lens; that says nothing about it.
    lens.reject({ code: "repo_not_open", message: "not open" });
    await opening;

    expect(persistedView("/repos/fresh")).toEqual({ kind: "mine" });
  });
});

describe("reopenClosedTab", () => {
  // Two repo tabs on screen, "b" active; opens resolve to the path asked.
  function twoTabs() {
    const a = createTab("/repos/a", "a");
    a.snapshot = snapshot("/repos/a");
    const b = createTab("/repos/b", "b");
    b.snapshot = snapshot("/repos/b");
    tabs.activeId = b.id;
    app.snapshot = b.snapshot;
    vi.mocked(api.openRepo).mockImplementation(async (path) => snapshot(path));
    return { a, b };
  }

  it("walks back through closed tabs, newest first", async () => {
    const { a, b } = twoTabs();
    closeTab(b.id);
    closeTab(a.id);
    expect(tabs.all.map((t) => t.path)).toEqual([null]);

    await reopenClosedTab();
    // The empty tab the last close left behind takes the repo.
    expect(tabs.all.map((t) => t.path)).toEqual(["/repos/a"]);
    await reopenClosedTab();
    expect(tabs.all.map((t) => t.path)).toEqual(["/repos/a", "/repos/b"]);
    expect(app.snapshot?.repoPath).toBe("/repos/b");

    await reopenClosedTab();
    expect(api.openRepo).toHaveBeenCalledTimes(2);
  });

  it("skips a closed repo that was opened again another way", async () => {
    const { a, b } = twoTabs();
    closeTab(a.id);
    closeTab(b.id);
    // "b" comes back through recents before ⌘⇧T.
    await openRepoAt("/repos/b");

    await reopenClosedTab();
    expect(tabs.all.map((t) => t.path).sort()).toEqual(["/repos/a", "/repos/b"]);
    expect(tabs.closed).toEqual([]);
  });
});

describe("openDroppedPaths", () => {
  it("opens each dropped folder in its own tab, folding repos already open", async () => {
    // The backend resolves a subfolder to its enclosing repo's root.
    vi.mocked(api.openRepo).mockImplementation(async (path) =>
      snapshot(path.replace(/\/src$/, "")),
    );
    await openRepoAt("/repos/a");

    await openDroppedPaths(["/repos/b", "/repos/a/src"]);

    expect(tabs.all.map((t) => t.path)).toEqual(["/repos/a", "/repos/b"]);
    expect(api.openRepo).toHaveBeenCalledTimes(3);
    // The last drop lands on the repo that was already open.
    expect(app.snapshot?.repoPath).toBe("/repos/a");
  });
});

describe("forgetRecentRepo", () => {
  it("drops the row at once and settles on the store's list", async () => {
    const recent = (path: string) => ({ path, name: path, lastOpenedAt: 0 });
    app.recentRepos = [recent("/repos/a"), recent("/repos/gone")];
    const stored = deferred<ReturnType<typeof recent>[]>();
    vi.mocked(forgetRepo).mockReturnValue(stored.promise);

    const forgetting = forgetRecentRepo("/repos/gone");
    expect(app.recentRepos.map((r) => r.path)).toEqual(["/repos/a"]);
    stored.resolve([recent("/repos/a"), recent("/repos/c")]);
    await forgetting;

    expect(forgetRepo).toHaveBeenCalledWith("/repos/gone");
    expect(app.recentRepos.map((r) => r.path)).toEqual(["/repos/a", "/repos/c"]);
  });
});
