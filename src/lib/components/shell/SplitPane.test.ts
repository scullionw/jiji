// @vitest-environment jsdom
import { createRawSnippet } from "svelte";
import { cleanup, fireEvent, render, waitFor } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import SplitPane from "./SplitPane.svelte";

let resize: (entries: { contentRect: { width: number } }[]) => void;
beforeEach(() => {
  localStorage.clear();
  vi.stubGlobal("ResizeObserver", class {
    constructor(callback: typeof resize) { resize = callback; }
    observe() {}
    disconnect() {}
  });
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

function pane() {
  const content = createRawSnippet(() => ({ render: () => "<div>Content</div>" }));
  return render(SplitPane, {
    id: "test", min: 340, max: 760, initial: 460, side: "start",
    children: content, panel: content,
  });
}

describe("pane resizing", () => {
  it("reserves content space on narrow windows without overwriting the preference", async () => {
    localStorage.setItem("jiji.pane.test", "700");
    const view = pane();
    resize([{ contentRect: { width: 800 } }]);
    await waitFor(() => expect(view.getByRole("separator").getAttribute("aria-valuenow")).toBe("480"));
    expect(localStorage.getItem("jiji.pane.test")).toBe("700");
    resize([{ contentRect: { width: 1280 } }]);
    await waitFor(() => expect(view.getByRole("separator").getAttribute("aria-valuenow")).toBe("700"));
  });

  it("supports keyboard resizing, bounds, reset and collapsed focus exclusion", async () => {
    const view = pane();
    const divider = view.getByRole("separator");
    await fireEvent.keyDown(divider, { key: "ArrowRight" });
    expect(divider.getAttribute("aria-valuenow")).toBe("476");
    expect(localStorage.getItem("jiji.pane.test")).toBe("476");
    await fireEvent.keyDown(divider, { key: "Home" });
    expect(divider.getAttribute("aria-valuenow")).toBe("340");
    await fireEvent.keyDown(divider, { key: "End" });
    expect(divider.getAttribute("aria-valuenow")).toBe("760");
    await fireEvent.doubleClick(divider);
    expect(divider.getAttribute("aria-valuenow")).toBe("460");
    await view.rerender({ collapsed: true });
    expect(divider.getAttribute("tabindex")).toBe("-1");
    expect(view.container.querySelector("aside")?.inert).toBe(true);
    expect(view.container.querySelector("aside")?.getAttribute("aria-hidden")).toBe("true");
  });
});
