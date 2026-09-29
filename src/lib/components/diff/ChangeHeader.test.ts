// @vitest-environment jsdom
import { cleanup, fireEvent, render } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { GraphNode } from "$lib/bindings/GraphNode";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import { app } from "$lib/state/app.svelte";
import ChangeHeader from "./ChangeHeader.svelte";
import { discardDraft } from "./drafts.svelte";

vi.hoisted(() => {
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    value: (media: string) => ({ media, matches: true, addEventListener() {}, removeEventListener() {} }),
  });
});
vi.mock("$lib/state/actions", async (importOriginal) => ({
  ...(await importOriginal<typeof import("$lib/state/actions")>()),
  describeChange: vi.fn(async () => ({})),
}));

function node(id: string, description = `Change ${id}`): GraphNode {
  return {
    id, changeId: id, commitId: id, description, author: "Test",
    timestamp: "2026-09-01T00:00:00Z", kind: "mutable", parents: [], elidedParents: [],
    bookmarks: [], tags: [], isEmpty: false, hasConflict: false, isDivergent: false,
  };
}
// A fresh snapshot object each call, like every published refresh.
function snapshot(descriptions: Record<string, string> = {}): RepoSnapshot {
  return {
    repoPath: "/repo", repoName: "repo", backend: "mock", view: { kind: "workstreams" },
    truncated: false, trunkBookmark: "", workingCopy: "a", workspaces: [], workstreams: [],
    nodes: ["a", "b"].map((id) => node(id, descriptions[id])), bookmarks: [], gitRemotes: [],
    conflicts: [], operations: [], resolveTool: null,
  };
}
function props(repo: RepoSnapshot, id: string) {
  return {
    snapshot: repo, node: repo.nodes.find((n) => n.id === id)!, files: null,
    layout: "unified" as const, onlayout: vi.fn(), tree: false, ontree: vi.fn(),
    compare: { kind: "parent" as const }, compareFrom: null, oncompare: vi.fn(),
    onjump: vi.fn(), onjumpfile: vi.fn(), onclose: vi.fn(),
  };
}
const editor = (root: HTMLElement) => root.querySelector("textarea");
const pencil = (root: HTMLElement) =>
  root.querySelector<HTMLButtonElement>('button[title^="Edit description"]')!;

async function openAndType(root: HTMLElement, text: string) {
  await fireEvent.click(pencil(root));
  await fireEvent.input(editor(root)!, { target: { value: text } });
}

beforeEach(() => {
  app.snapshot = snapshot();
  for (const id of ["a", "b"]) discardDraft("/repo", id);
});
afterEach(() => {
  cleanup();
  app.snapshot = null;
});

describe("ChangeHeader describe drafts", () => {
  it("keeps an unsaved draft through Esc and restores it on reopen", async () => {
    const view = render(ChangeHeader, props(snapshot(), "a"));
    await openAndType(view.container, "half-written message");
    await fireEvent.keyDown(editor(view.container)!, { key: "Escape" });
    expect(editor(view.container)).toBeNull();
    expect(pencil(view.container).classList.contains("has-draft")).toBe(true);

    await fireEvent.click(pencil(view.container));
    expect(editor(view.container)?.value).toBe("half-written message");
    expect(view.container.textContent).toContain("Unsaved draft restored");
  });

  it("keeps the draft when the selection moves to another change", async () => {
    const view = render(ChangeHeader, props(snapshot(), "a"));
    await openAndType(view.container, "half-written message");
    await view.rerender(props(snapshot(), "b"));
    expect(editor(view.container)).toBeNull();
    expect(pencil(view.container).classList.contains("has-draft")).toBe(false);

    await view.rerender(props(snapshot(), "a"));
    await fireEvent.click(pencil(view.container));
    expect(editor(view.container)?.value).toBe("half-written message");
  });

  it("drops the draft once saved", async () => {
    const view = render(ChangeHeader, props(snapshot(), "a"));
    await openAndType(view.container, "finished message");
    await fireEvent.click(view.getByText("Save"));
    await vi.waitFor(() => expect(editor(view.container)).toBeNull());

    expect(pencil(view.container).classList.contains("has-draft")).toBe(false);
    await fireEvent.click(pencil(view.container));
    expect(editor(view.container)?.value).toBe("Change a");
  });

  it("never offers a draft over a description changed elsewhere", async () => {
    const view = render(ChangeHeader, props(snapshot(), "a"));
    await openAndType(view.container, "stale message");
    await fireEvent.keyDown(editor(view.container)!, { key: "Escape" });

    await view.rerender(props(snapshot({ a: "Described from the CLI" }), "a"));
    expect(pencil(view.container).classList.contains("has-draft")).toBe(false);
    await fireEvent.click(pencil(view.container));
    expect(editor(view.container)?.value).toBe("Described from the CLI");
  });

  it("discards a restored draft back to the description", async () => {
    const view = render(ChangeHeader, props(snapshot(), "a"));
    await openAndType(view.container, "unwanted message");
    await fireEvent.keyDown(editor(view.container)!, { key: "Escape" });
    await fireEvent.click(pencil(view.container));

    await fireEvent.click(view.getByText("Discard"));
    expect(editor(view.container)?.value).toBe("Change a");
    await fireEvent.keyDown(editor(view.container)!, { key: "Escape" });
    expect(pencil(view.container).classList.contains("has-draft")).toBe(false);
  });
});

describe("ChangeHeader across refreshes", () => {
  it("keeps the describe editor open with its text through a snapshot refresh", async () => {
    const view = render(ChangeHeader, props(snapshot(), "a"));
    await openAndType(view.container, "half-written message");

    // A watcher tick republishes the same change as a new object.
    await view.rerender(props(snapshot(), "a"));

    expect(editor(view.container)?.value).toBe("half-written message");
  });
});
