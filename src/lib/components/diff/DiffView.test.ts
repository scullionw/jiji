// @vitest-environment jsdom
import { cleanup, fireEvent, render, waitFor } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ChangeDiff } from "$lib/bindings/ChangeDiff";
import type { GraphNode } from "$lib/bindings/GraphNode";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import { app } from "$lib/state/app.svelte";
import DiffView from "./DiffView.svelte";

const { fetchDiff } = vi.hoisted(() => {
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    value: (media: string) => ({ media, matches: true, addEventListener() {}, removeEventListener() {} }),
  });
  return { fetchDiff: vi.fn() };
});
vi.mock("./diffcache", () => ({ cachedDiff: () => undefined, fetchDiff, prefetchDiff: vi.fn() }));

function node(id: string): GraphNode {
  return {
    id, changeId: id, commitId: id, description: `Change ${id}`, author: "Test",
    timestamp: "2026-09-01T00:00:00Z", kind: "mutable", parents: [], elidedParents: [],
    bookmarks: [], tags: [], isEmpty: false, hasConflict: false, isDivergent: false,
  };
}
function snapshot(op = "op1"): RepoSnapshot {
  return {
    repoPath: "/repo", repoName: "repo", backend: "mock", view: { kind: "workstreams" },
    truncated: false, trunkBookmark: "", workingCopy: "a", workspaces: [], workstreams: [],
    nodes: [node("a"), node("b")], bookmarks: [], gitRemotes: [], conflicts: [], resolveTool: null,
    operations: [{ id: op, description: "Test operation", timestamp: "", isCurrent: true,
      user: "Test", isSnapshot: false, effects: [], moreEffects: 0 }],
  };
}
function diff(id: string): ChangeDiff {
  return {
    id, from: null, truncated: false,
    files: [{ path: `${id}.bin`, status: "added", renamedFrom: null, hasConflict: false,
      content: { kind: "binary", oldSize: null, newSize: 123 } }],
  };
}
function mount() {
  const repo = snapshot();
  app.snapshot = repo;
  return render(DiffView, {
    snapshot: repo, node: repo.nodes[0], onjump: vi.fn(), onclose: vi.fn(),
  });
}

beforeEach(() => {
  fetchDiff.mockReset();
  localStorage.clear();
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.stubGlobal("requestAnimationFrame", () => 1);
  vi.stubGlobal("cancelAnimationFrame", () => {});
  HTMLElement.prototype.scrollTo = vi.fn();
  HTMLElement.prototype.scrollIntoView = vi.fn();
});
afterEach(() => { cleanup(); app.snapshot = null; app.intent = null; vi.unstubAllGlobals(); });

describe("diff request lifecycle", () => {
  it("never associates the previous file content with a newly selected change", async () => {
    fetchDiff.mockResolvedValueOnce(diff("a"));
    const view = mount();
    await waitFor(() => expect(view.container.querySelector("[data-diff-change]")?.getAttribute("data-diff-change")).toBe("a"));
    let finish!: (value: ChangeDiff) => void;
    fetchDiff.mockReturnValueOnce(new Promise((resolve) => { finish = resolve; }));
    await view.rerender({ node: node("b") });
    expect(view.container.querySelector("[data-diff-change]")).toBeNull();
    finish(diff("b"));
    await waitFor(() => expect(view.container.querySelector("[data-diff-change]")?.getAttribute("data-diff-change")).toBe("b"));
  });

  it("keeps stale content read-only and reports a failed refresh with a working retry", async () => {
    fetchDiff.mockResolvedValueOnce(diff("a"));
    const view = mount();
    await waitFor(() => expect(view.container.querySelector("[data-diff-change]")).not.toBeNull());
    fetchDiff.mockRejectedValueOnce(new Error("Repository is busy"));
    await view.rerender({ snapshot: snapshot("op2") });
    await waitFor(() => expect(view.getByRole("alert").textContent).toContain("Repository is busy"));
    expect(view.container.querySelector("[data-diff-change]")).toBeNull();
    expect(view.getByText("Binary file — 123 B")).toBeDefined();
    fetchDiff.mockResolvedValueOnce(diff("a"));
    await fireEvent.click(view.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(view.queryByRole("alert")).toBeNull());
    await waitFor(() => expect(view.container.querySelector("[data-diff-change]")).not.toBeNull());
    expect(fetchDiff).toHaveBeenLastCalledWith("/repo", "op2", null, "a");
  });

  it("discards late responses after selection changes", async () => {
    let finish!: (value: ChangeDiff) => void;
    fetchDiff.mockReturnValueOnce(new Promise((resolve) => { finish = resolve; }));
    const view = mount();
    fetchDiff.mockResolvedValueOnce(diff("b"));
    await view.rerender({ node: node("b") });
    await waitFor(() => expect(view.container.querySelector("[data-diff-change]")?.getAttribute("data-diff-change")).toBe("b"));
    finish(diff("a"));
    await Promise.resolve();
    expect(view.container.querySelector("[data-diff-change]")?.getAttribute("data-diff-change")).toBe("b");
    expect(view.queryByText("a.bin")).toBeNull();
  });
});
