// The live rewrite preview behind the spec's hover-scrub: whichever
// surface is planning a rewrite (the drag session, ChangeHeader's plan
// panels) publishes the drawn changes the mutation would rewrite, plus the
// prospective destination when one is picked. Graph rows read it and light
// up, so the blast radius shows in the tree itself — not only in the
// plan's prose — and scrubbing across destinations moves the highlight
// live. One owner at a time: a drag stomps a panel's set for its duration,
// and the panel re-publishes when the drag ends (it watches `drag.active`).

export const rewritePreview = $state({
  owner: null as string | null,
  ids: new Set<string>(),
  destination: null as string | null,
});

export function setRewritePreview(
  owner: string,
  ids: Iterable<string>,
  destination: string | null = null,
): void {
  rewritePreview.owner = owner;
  rewritePreview.ids = new Set(ids);
  rewritePreview.destination = destination;
}

/** Clearing checks ownership so a closing panel cannot wipe a live drag. */
export function clearRewritePreview(owner: string): void {
  if (rewritePreview.owner !== owner) return;
  rewritePreview.owner = null;
  rewritePreview.ids = new Set();
  rewritePreview.destination = null;
}
