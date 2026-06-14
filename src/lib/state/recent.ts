// Persistent recent-repo state, backed by the Tauri store plugin.

import { load, type Store } from "@tauri-apps/plugin-store";
import type { RecentRepo } from "./app.svelte";

const STORE_FILE = "recent-repos.json";
const KEY = "repos";
const MAX_RECENT = 8;

let storePromise: Promise<Store> | null = null;

function getStore(): Promise<Store> {
  storePromise ??= load(STORE_FILE, { defaults: {}, autoSave: true });
  return storePromise;
}

export async function loadRecentRepos(): Promise<RecentRepo[]> {
  const store = await getStore();
  return (await store.get<RecentRepo[]>(KEY)) ?? [];
}

export async function rememberRepo(
  path: string,
  name: string,
): Promise<RecentRepo[]> {
  const store = await getStore();
  const existing = ((await store.get<RecentRepo[]>(KEY)) ?? []).filter(
    (repo) => repo.path !== path,
  );
  const next = [{ path, name, lastOpenedAt: Date.now() }, ...existing].slice(
    0,
    MAX_RECENT,
  );
  await store.set(KEY, next);
  return next;
}
