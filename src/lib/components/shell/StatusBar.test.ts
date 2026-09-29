// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { app } from "$lib/state/app.svelte";
import StatusBar from "./StatusBar.svelte";

vi.mock("$lib/api", () => ({
  errorMessage: (error: unknown) => String(error),
}));
vi.mock("$lib/motion", () => ({ motionMs: () => 0 }));

afterEach(() => {
  cleanup();
  app.error = null;
});

describe("StatusBar error line", () => {
  it("keeps the full message readable and dismisses on request", async () => {
    const message =
      "Error: Revision `zzz` doesn't exist\nHint: Did you mean `zzzz`?";
    app.error = message;
    const { container } = render(StatusBar);

    // The line truncates; the hover title carries every line verbatim.
    const line = container.querySelector(".danger-text");
    expect(line?.getAttribute("title")).toBe(message);
    await fireEvent.click(screen.getByRole("button", { name: "Dismiss error" }));

    expect(app.error).toBeNull();
    expect(container.querySelector(".danger-text")).toBeNull();
    expect(screen.getByText("Idle — open a repository to begin")).toBeTruthy();
  });
});
