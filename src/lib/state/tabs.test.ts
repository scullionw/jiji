import { describe, expect, it } from "vitest";
import { tabTitles } from "./tabs.svelte";

function titles(paths: (string | null)[]) {
  const all = paths.map((path, id) => ({ id, path, name: null }));
  return [...tabTitles(all).values()];
}

describe("tabTitles", () => {
  it("leaves unique names alone", () => {
    expect(titles(["/Users/me/jiji", "/Users/me/api", null])).toEqual([
      { label: "jiji", hint: null },
      { label: "api", hint: null },
      { label: "New Tab", hint: null },
    ]);
  });

  it("tells same-named repos apart by their parent directory", () => {
    expect(titles(["/Users/me/work/api", "/Users/me/oss/api", "/Users/me/jiji"])).toEqual([
      { label: "api", hint: "work" },
      { label: "api", hint: "oss" },
      { label: "jiji", hint: null },
    ]);
  });

  it("climbs as far as it takes to differ", () => {
    expect(titles(["/a/wt/api", "/b/wt/api"]).map((t) => t.hint)).toEqual([
      "a/wt",
      "b/wt",
    ]);
  });

  it("names the root when one repo sits directly under it", () => {
    expect(titles(["/api", "/srv/api"]).map((t) => t.hint)).toEqual(["/", "srv"]);
  });

  it("uses the repo's own name over its folder name", () => {
    const all = [
      { id: 1, path: "/x/one", name: "shared" },
      { id: 2, path: "/y/two", name: "shared" },
    ];
    expect([...tabTitles(all).values()].map((t) => t.hint)).toEqual(["x", "y"]);
  });
});
