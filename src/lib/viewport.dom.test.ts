// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { observeViewport } from "./viewport";

let frame: FrameRequestCallback | undefined;
const disconnect = vi.fn();
let resize: () => void;

beforeEach(() => {
  disconnect.mockClear();
  vi.stubGlobal("ResizeObserver", class {
    constructor(notify: () => void) { resize = notify; }
    observe() {}
    disconnect = disconnect;
  });
  vi.stubGlobal("requestAnimationFrame", vi.fn((callback: FrameRequestCallback) => {
    frame = callback;
    return 1;
  }));
  vi.stubGlobal("cancelAnimationFrame", vi.fn(() => { frame = undefined; }));
});
afterEach(() => { vi.unstubAllGlobals(); });

function paint() {
  const callback = frame;
  frame = undefined;
  callback?.(0);
}

describe("shared scroll observation", () => {
  it("measures once for every file and coalesces scroll/resize events", () => {
    const element = document.createElement("div");
    const measure = vi.spyOn(element, "getBoundingClientRect");
    const first = vi.fn();
    const second = vi.fn();
    const stopFirst = observeViewport(element, first);
    const stopSecond = observeViewport(element, second);
    for (let i = 0; i < 20; i++) element.dispatchEvent(new Event("scroll"));
    resize();
    paint();
    expect(measure).toHaveBeenCalledTimes(1);
    expect(first).toHaveBeenCalledTimes(1);
    expect(second).toHaveBeenCalledTimes(1);
    stopFirst();
    element.dispatchEvent(new Event("scroll"));
    paint();
    expect(first).toHaveBeenCalledTimes(1);
    expect(second).toHaveBeenCalledTimes(2);
    expect(disconnect).not.toHaveBeenCalled();
    stopSecond();
    expect(disconnect).toHaveBeenCalledOnce();
    element.dispatchEvent(new Event("scroll"));
    expect(frame).toBeUndefined();
  });

  it("cancels pending work when the final consumer unmounts", () => {
    const element = document.createElement("div");
    const listener = vi.fn();
    const stop = observeViewport(element, listener);
    stop();
    paint();
    expect(listener).not.toHaveBeenCalled();
    expect(disconnect).toHaveBeenCalledOnce();
  });
});
