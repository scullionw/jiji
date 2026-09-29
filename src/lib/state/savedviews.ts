// Persistent saved revset views, backed by the Tauri store plugin.
// One store file, keyed by repo path — a saved view is a per-repo lens
// preset like the sticky lens choice, but named and listed in the picker.

import { load, type Store } from "@tauri-apps/plugin-store";
import {
  parseSavedViews,
  type SavedView,
} from "$lib/components/graph/views";

const STORE_FILE = "saved-views.json";

let storePromise: Promise<Store> | null = null;

function getStore(): Promise<Store> {
  storePromise ??= load(STORE_FILE, { defaults: {}, autoSave: true });
  return storePromise;
}

export async function loadSavedViews(repoPath: string): Promise<SavedView[]> {
  const store = await getStore();
  return parseSavedViews(await store.get<unknown>(repoPath));
}

export async function persistSavedViews(
  repoPath: string,
  views: SavedView[],
): Promise<void> {
  const store = await getStore();
  if (views.length === 0) await store.delete(repoPath);
  else await store.set(repoPath, views);
}
