// Unsaved describe drafts, kept for the session per repo and change, so
// closing the editor any way but Save (Esc, Cancel, selecting another
// change, switching tabs) never throws typed text away. Each draft
// remembers the description it started from: once the change is described
// some other way (the CLI, an undo), the draft no longer applies and is
// never offered over the newer text.

import { SvelteMap } from "svelte/reactivity";

interface Draft {
  base: string;
  text: string;
}

const drafts = new SvelteMap<string, Draft>();

const key = (repo: string, changeId: string) => `${repo}\0${changeId}`;

/** Keep what the editor held when it closed; text identical to the
 * description it started from is no draft at all. */
export function keepDraft(
  repo: string,
  changeId: string,
  base: string,
  text: string,
): void {
  if (text === base) drafts.delete(key(repo, changeId));
  else drafts.set(key(repo, changeId), { base, text });
}

/** The kept draft for a change whose description is still `description`
 * (the one the draft started from), else null. Reactive. */
export function draftFor(
  repo: string,
  changeId: string,
  description: string,
): string | null {
  const draft = drafts.get(key(repo, changeId));
  return draft && draft.base === description ? draft.text : null;
}

export function discardDraft(repo: string, changeId: string): void {
  drafts.delete(key(repo, changeId));
}
