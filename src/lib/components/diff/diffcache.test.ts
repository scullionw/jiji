import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ChangeDiff } from "$lib/bindings/ChangeDiff";

const changeDiff = vi.fn();
const compareDiff = vi.fn();
vi.mock("$lib/api", () => ({
  changeDiff: (id: string, path: string) => changeDiff(id, path),
  compareDiff: (from: string, id: string, path: string) => compareDiff(from, id, path),
}));

const { cachedDiff, fetchDiff, prefetchDiff } = await import("./diffcache");

function diffFor(id: string, from: string | null = null): ChangeDiff {
  return { id, from, files: [], truncated: false };
}

// The module-level cache persists across tests; unique op ids keep each
// test's keys disjoint.
let op = 0;
let opId = "";

beforeEach(() => {
  opId = `op${op++}`;
  changeDiff.mockReset();
  compareDiff.mockReset();
  changeDiff.mockImplementation((id: string) => Promise.resolve(diffFor(id)));
  compareDiff.mockImplementation((from: string, id: string) =>
    Promise.resolve(diffFor(id, from)),
  );
});

describe("fetchDiff", () => {
  it("isolates identical operation and change ids in different repositories", async () => {
    const first = await fetchDiff("/repo one", opId, null, "same");
    const second = await fetchDiff("/repo two", opId, null, "same");
    expect(first).not.toBe(second);
    expect(changeDiff).toHaveBeenNthCalledWith(1, "same", "/repo one");
    expect(changeDiff).toHaveBeenNthCalledWith(2, "same", "/repo two");
    expect(cachedDiff("/repo one", opId, null, "same")).toBe(first);
    expect(cachedDiff("/repo two", opId, null, "same")).toBe(second);
  });

  it("does not share in-flight requests across repositories", async () => {
    let resolve!: (diff: ChangeDiff) => void;
    changeDiff.mockReturnValueOnce(new Promise((done) => { resolve = done; }));
    const pending = fetchDiff("/first", opId, null, "same");
    const second = await fetchDiff("/second", opId, null, "same");
    expect(cachedDiff("/first", opId, null, "same")).toBeUndefined();
    resolve(diffFor("same"));
    expect(await pending).not.toBe(second);
    expect(changeDiff).toHaveBeenCalledTimes(2);
  });

  it("marks asynchronous cache hits as recently used", async () => {
    await fetchDiff("/repo", opId, null, "keep");
    for (let i = 0; i < 15; i++) await fetchDiff("/repo", opId, null, `fill${i}`);
    await fetchDiff("/repo", opId, null, "keep");
    await fetchDiff("/repo", opId, null, "overflow");
    expect(cachedDiff("/repo", opId, null, "keep")).toBeDefined();
    expect(cachedDiff("/repo", opId, null, "fill0")).toBeUndefined();
  });

  it("fetches a parent diff once and serves repeats from the cache", async () => {
    const first = await fetchDiff("/repo", opId, null, "aaa");
    const again = await fetchDiff("/repo", opId, null, "aaa");
    expect(first).toBe(again);
    expect(changeDiff).toHaveBeenCalledTimes(1);
    expect(cachedDiff("/repo", opId, null, "aaa")).toBe(first);
  });

  it("routes comparisons through compareDiff and keys them separately", async () => {
    const parent = await fetchDiff("/repo", opId, null, "aaa");
    const compared = await fetchDiff("/repo", opId, "base", "aaa");
    expect(parent.from).toBeNull();
    expect(compared.from).toBe("base");
    expect(cachedDiff("/repo", opId, "base", "aaa")).toBe(compared);
    expect(cachedDiff("/repo", opId, null, "aaa")).toBe(parent);
  });

  it("shares one in-flight request between concurrent callers", async () => {
    let release!: (diff: ChangeDiff) => void;
    changeDiff.mockReturnValue(new Promise((r) => (release = r)));
    const a = fetchDiff("/repo", opId, null, "aaa");
    const b = fetchDiff("/repo", opId, null, "aaa");
    release(diffFor("aaa"));
    expect(await a).toBe(await b);
    expect(changeDiff).toHaveBeenCalledTimes(1);
  });

  it("does not cache failures", async () => {
    changeDiff.mockRejectedValueOnce(new Error("nope"));
    await expect(fetchDiff("/repo", opId, null, "aaa")).rejects.toThrow("nope");
    expect(cachedDiff("/repo", opId, null, "aaa")).toBeUndefined();
    await expect(fetchDiff("/repo", opId, null, "aaa")).resolves.toEqual(diffFor("aaa"));
  });

  it("keys by operation, so a new operation refetches", async () => {
    await fetchDiff("/repo", opId, null, "aaa");
    expect(cachedDiff("/repo", `${opId}-next`, null, "aaa")).toBeUndefined();
  });
});

describe("cachedDiff", () => {
  it("misses before any fetch", () => {
    expect(cachedDiff("/repo", opId, null, "zzz")).toBeUndefined();
  });

  it("evicts the least recently used entry past the cap", async () => {
    await fetchDiff("/repo", opId, null, "keep");
    for (let i = 0; i < 15; i++) await fetchDiff("/repo", opId, null, `fill${i}`);
    // Touch the oldest so eviction takes fill0 instead.
    cachedDiff("/repo", opId, null, "keep");
    await fetchDiff("/repo", opId, null, "one-more");
    expect(cachedDiff("/repo", opId, null, "keep")).toBeDefined();
    expect(cachedDiff("/repo", opId, null, "fill0")).toBeUndefined();
  });
});

describe("prefetchDiff", () => {
  it("warms the cache in the background", async () => {
    prefetchDiff("/repo", opId, null, "warm");
    await vi.waitFor(() =>
      expect(cachedDiff("/repo", opId, null, "warm")).toBeDefined(),
    );
    expect(changeDiff).toHaveBeenCalledTimes(1);
  });

  it("does not refetch what is cached or already in flight", async () => {
    await fetchDiff("/repo", opId, null, "aaa");
    prefetchDiff("/repo", opId, null, "aaa");
    expect(changeDiff).toHaveBeenCalledTimes(1);
  });

  it("swallows failures", async () => {
    changeDiff.mockRejectedValueOnce(new Error("offline"));
    prefetchDiff("/repo", opId, null, "flaky");
    await new Promise((r) => setTimeout(r, 0));
    expect(cachedDiff("/repo", opId, null, "flaky")).toBeUndefined();
  });
});
