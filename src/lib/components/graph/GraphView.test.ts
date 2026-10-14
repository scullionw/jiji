// @vitest-environment jsdom
import { cleanup, render, waitFor } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { density } from "./density.svelte";
import { nodeRowHeight, type GraphModel, type NodeRow } from "./graph";
import GraphView from "./GraphView.svelte";

vi.hoisted(() => {
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    value: (media: string) => ({
      media, matches: true, addEventListener() {}, removeEventListener() {},
    }),
  });
});

function model(count: number): GraphModel {
  return {
    columnCount: 1,
    trunkName: null,
    rows: Array.from({ length: count }, (_, i): NodeRow => ({
      type: "node", column: 0, stream: null, edgesIn: [], edgesOut: [], passThrough: [],
      isWorkingCopy: false, isStackHead: false, behindTrunk: 0, bookmarks: [],
      node: {
        id: `change${i}`, changeId: `change${i}`, commitId: `commit${i}`,
        description: `Change ${i}`, author: "Test", timestamp: "2026-09-01T00:00:00Z",
        kind: "mutable", parents: [], elidedParents: [], bookmarks: [], tags: [],
        isEmpty: false, hasConflict: false, isDivergent: false,
      },
    })),
  };
}

beforeEach(() => {
  density.mode = "compact";
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.stubGlobal("requestAnimationFrame", () => 1);
  vi.stubGlobal("cancelAnimationFrame", () => {});
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

describe("large history rendering", () => {
  it("bounds mounted rows and reveals a selection beyond the viewport", async () => {
    const scroller = document.createElement("div");
    Object.defineProperty(scroller, "clientHeight", { value: 820 });
    const view = render(GraphView, {
      model: model(1000), lanes: new Map(), emphasized: null,
      selectedIds: new Set(["change0"]), activeId: "change0", scroller, onselect: vi.fn(),
    });
    // Of 1,000 rows, the viewport plus a viewport's margin each side is
    // mounted (~90 at 28px rows), not all.
    expect(view.container.querySelectorAll("[data-node-id]").length).toBeLessThan(100);
    expect(view.container.querySelector('[data-node-id="change900"]')).toBeNull();
    await view.rerender({ selectedIds: new Set(["change900"]), activeId: "change900" });
    await waitFor(() => {
      expect(view.container.querySelector('[data-node-id="change900"]')?.getAttribute("aria-pressed")).toBe("true");
    });
    expect(view.container.querySelectorAll("[data-node-id]").length).toBeLessThan(100);
    expect(scroller.scrollTop).toBe(901 * nodeRowHeight("compact") - 820);
  });

  it("updates exact scroll height when row density changes", async () => {
    const scroller = document.createElement("div");
    const view = render(GraphView, {
      model: model(1000), lanes: new Map(), emphasized: null,
      selectedIds: new Set<string>(), scroller, onselect: vi.fn(),
    });
    expect(view.container.querySelector<HTMLElement>(".graph")?.style.height).toBe("28000px");
    density.mode = "classic";
    await waitFor(() => {
      expect(view.container.querySelector<HTMLElement>(".graph")?.style.height).toBe("46000px");
    });
  });
});
