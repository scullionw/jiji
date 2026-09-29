// Which editor CLIs this machine has, and which one menus offer first.
// Detected once at bootstrap (stat calls, cheap). The user can pin a
// preferred editor in Settings; without a pin, the one last used from a
// context menu leads. Both choices stick per device.

import * as api from "$lib/api";

const LAST_EDITOR_KEY = "jiji.editor.last";
const PREFERRED_EDITOR_KEY = "jiji.editor.preferred";

export const editorState = $state({
  detected: [] as api.EditorInfo[],
  lastUsed: localStorage.getItem(LAST_EDITOR_KEY),
  // Editor id pinned first in menus; null follows lastUsed instead.
  preferred: localStorage.getItem(PREFERRED_EDITOR_KEY),
});

export async function syncEditors(): Promise<void> {
  try {
    editorState.detected = await api.detectEditors();
  } catch (error) {
    // No editor items in menus (e.g. the browser harness's stubbed IPC).
    console.warn("editor detection failed", error);
  }
}

/// Detected editors in the order menus offer them — the pinned favorite
/// first when set, else the last-used one — so the primary "Open in X"
/// follows the user's setting, then habit.
export function editorChoices(): api.EditorInfo[] {
  const { detected, lastUsed, preferred } = editorState;
  const first =
    detected.find((editor) => editor.id === preferred) ??
    detected.find((editor) => editor.id === lastUsed);
  if (!first) return detected;
  return [first, ...detected.filter((editor) => editor.id !== first.id)];
}

export function setPreferredEditor(id: string | null): void {
  editorState.preferred = id;
  if (id) localStorage.setItem(PREFERRED_EDITOR_KEY, id);
  else localStorage.removeItem(PREFERRED_EDITOR_KEY);
}

export function openInEditor(
  editor: string,
  path: string,
  line?: number,
): Promise<void> {
  editorState.lastUsed = editor;
  localStorage.setItem(LAST_EDITOR_KEY, editor);
  return api.openInEditor(editor, path, line);
}
