// @vitest-environment jsdom
import { cleanup, fireEvent, render } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { app } from "$lib/state/app.svelte";
import SearchOverlay from "./SearchOverlay.svelte";

vi.hoisted(() => {
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    value: (media: string) => ({ media, matches: true, addEventListener() {}, removeEventListener() {} }),
  });
});
vi.mock("$lib/api", () => ({
  searchHistory: vi.fn(() => new Promise(() => {})),
  errorMessage: (error: unknown) => String(error),
}));

const input = (root: HTMLElement) => root.querySelector("input")!;

afterEach(() => {
  cleanup();
  app.searchSeed = "";
});

describe("SearchOverlay", () => {
  it("reopens on the last query, and a seed still takes precedence", async () => {
    HTMLElement.prototype.scrollIntoView = vi.fn();
    const first = render(SearchOverlay);
    await fireEvent.input(input(first.container), { target: { value: "parseRevset" } });
    first.unmount();

    const second = render(SearchOverlay);
    expect(input(second.container).value).toBe("parseRevset");
    second.unmount();

    app.searchSeed = "from the palette";
    const seeded = render(SearchOverlay);
    expect(input(seeded.container).value).toBe("from the palette");
  });
});
