// Shapes a RepoSnapshot into the dense, jj-native graph the workbench
// renders: every workstream and the immutable bases they sit on in one
// continuous tree. Rows come out the way `jj log` orders them — the working
// stack pinned on top, then newest-first, each branch kept together down to
// the base it grew from, with the trunk line one connected spine and
// explicit elision markers (jj's `~`) where history is hidden. Rails stay
// narrow by construction: a rail joins the rail already waiting for the
// same parent as soon as it can (jj's `├─╯`), so twenty stacks on trunk
// cost two columns, not twenty. Pure data — no Svelte.

import type { BookmarkState } from "$lib/bindings/BookmarkState";
import type { GraphNode } from "$lib/bindings/GraphNode";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { SyncState } from "$lib/bindings/SyncState";
import type { WorkstreamSummary } from "$lib/bindings/WorkstreamSummary";

/** One rail in a row's gutter. Rails are colored by the workstream that
 * owns them; null means immutable/unowned context. Elided rails cross a
 * gap in drawn history and render dashed. */
export interface Rail {
  col: number;
  stream: string | null;
  elided?: boolean;
}

export interface NodeRow {
  type: "node";
  node: GraphNode;
  column: number;
  /** Workstream that claims this node; null for immutable bases. */
  stream: string | null;
  /** Rails entering from the row above, ending at this node. */
  edgesIn: Rail[];
  /** Rails leaving this node toward the row below. */
  edgesOut: Rail[];
  /** Rails crossing this row without touching the node. */
  passThrough: Rail[];
  isWorkingCopy: boolean;
  /** True for the top of a workstream chain. */
  isStackHead: boolean;
  /** Commits trunk has that this stack lacks; head rows only, else 0. */
  behindTrunk: number;
  /** Bookmark states pointing at this node, trunk first. */
  bookmarks: BookmarkState[];
}

/** jj's `~`: history exists here but is not shown. When the gap leads to a
 * drawn ancestor the marked columns continue below the row; otherwise the
 * rail ends. */
export interface ElisionRow {
  type: "elision";
  id: string;
  /** Columns the `~` markers sit on. */
  marks: Rail[];
  /** Whether the marked rails resume beneath the markers. */
  continues: boolean;
  passThrough: Rail[];
}

export type GraphRowModel = NodeRow | ElisionRow;

export interface GraphModel {
  rows: GraphRowModel[];
  columnCount: number;
  trunkName: string | null;
}

/** Drawn parents plus elided-history links: everything that constrains
 * ordering and rail routing, deduplicated, limited to drawn nodes. */
function linkedParents(
  node: GraphNode,
  nodesById: Map<string, GraphNode>,
): string[] {
  const direct = new Set(node.parents);
  return [
    ...node.parents,
    ...node.elidedParents.filter((id) => !direct.has(id)),
  ].filter(
    (id, index, all) => nodesById.has(id) && all.indexOf(id) === index,
  );
}

export function buildGraphModel(snapshot: RepoSnapshot): GraphModel {
  const nodesById = new Map(snapshot.nodes.map((node) => [node.id, node]));

  // Stream membership. The working (active) stack is pinned on top;
  // everything else orders by recency — a stream by its head's timestamp,
  // unclaimed nodes and bases by their own — so each stack lands right
  // above the base it grew from, the way jj log reads.
  const streamOf = new Map<string, string>();
  const headOf = new Map<string, WorkstreamSummary>();
  const rank = new Map<string, number[]>();
  const timeOf = (id: string): number =>
    Date.parse(nodesById.get(id)?.timestamp ?? "") || 0;
  snapshot.workstreams.forEach((ws, si) => {
    const present = ws.nodeIds.filter((id) => nodesById.has(id));
    const streamTime = Math.max(0, ...present.map(timeOf));
    present.forEach((id, i) => {
      if (rank.has(id)) return;
      streamOf.set(id, ws.id);
      rank.set(id, [ws.isActive ? 0 : 1, -streamTime, si, i]);
      if (i === 0) headOf.set(id, ws);
    });
  });
  const streamCount = snapshot.workstreams.length;
  for (const node of snapshot.nodes) {
    if (rank.has(node.id)) continue;
    rank.set(node.id, [1, -timeOf(node.id), streamCount, 0]);
  }

  const compareIds = (a: string, b: string): number => {
    const ra = rank.get(a)!;
    const rb = rank.get(b)!;
    for (let i = 0; i < Math.max(ra.length, rb.length); i += 1) {
      const d = (ra[i] ?? 0) - (rb[i] ?? 0);
      if (d !== 0) return d;
    }
    return a < b ? -1 : a > b ? 1 : 0;
  };

  // Kahn's walk, children before parents (elided links count as parents so
  // the spine stays ordered). A branch is followed to its end before the
  // walk returns to the next head — the parent that just became ready
  // goes next, the way git's topo-order and jj's grouped log keep one line
  // of history together instead of interleaving parallel lines by date.
  // Only among heads (nothing above them to continue from) does the rank
  // above decide.
  const childCount = new Map<string, number>(
    snapshot.nodes.map((node) => [node.id, 0]),
  );
  for (const node of snapshot.nodes) {
    for (const parent of linkedParents(node, nodesById)) {
      childCount.set(parent, childCount.get(parent)! + 1);
    }
  }
  const ready = snapshot.nodes
    .filter((node) => childCount.get(node.id) === 0)
    .map((node) => node.id);
  const order: GraphNode[] = [];
  const placed = new Set<string>();
  let last: GraphNode | null = null;
  while (ready.length > 0) {
    let id: string | undefined = last
      ? linkedParents(last, nodesById).find(
          (parent) => childCount.get(parent) === 0 && !placed.has(parent),
        )
      : undefined;
    if (id !== undefined) {
      ready.splice(ready.indexOf(id), 1);
    } else {
      ready.sort(compareIds);
      id = ready.shift()!;
    }
    const node: GraphNode = nodesById.get(id)!;
    placed.add(id);
    order.push(node);
    last = node;
    for (const parent of linkedParents(node, nodesById)) {
      const count = childCount.get(parent)!;
      childCount.set(parent, count - 1);
      if (count - 1 === 0) ready.push(parent);
    }
  }
  // Real snapshots are acyclic; never drop rows if one ever is not.
  for (const node of snapshot.nodes) {
    if (!placed.has(node.id)) order.push(node);
  }

  const bookmarksByTarget = new Map<string, BookmarkState[]>();
  for (const bookmark of snapshot.bookmarks) {
    const list = bookmarksByTarget.get(bookmark.target) ?? [];
    list.push(bookmark);
    bookmarksByTarget.set(bookmark.target, list);
  }
  for (const list of bookmarksByTarget.values()) {
    list.sort((a, b) => Number(b.isTrunk) - Number(a.isTrunk));
  }

  // The trunk spine: trunk's first-parent line through the drawn nodes.
  // It is the graph's reference line, so it never bends into another rail
  // — everything else converges on it.
  const spine = new Set<string>();
  for (
    let id = snapshot.bookmarks.find((b) => b.isTrunk)?.target;
    id !== undefined && nodesById.has(id) && !spine.has(id);
    id = linkedParents(nodesById.get(id)!, nodesById)[0]
  ) {
    spine.add(id);
  }

  // Column machine: each active column waits for one parent id. A node
  // lands on the leftmost column waiting for it (other waiters merge in),
  // or opens a new column when nothing expects it (a head).
  const columns: ({ expects: string; stream: string | null } | null)[] = [];
  const rows: GraphRowModel[] = [];
  const drawn = new Set<string>();
  let columnCount = 0;

  const firstFree = (): number => {
    const free = columns.indexOf(null);
    if (free !== -1) return free;
    columns.push(null);
    return columns.length - 1;
  };

  for (const node of order) {
    const stream = streamOf.get(node.id) ?? null;
    const edgesIn: Rail[] = [];
    let column = -1;
    columns.forEach((slot, i) => {
      if (slot?.expects !== node.id) return;
      edgesIn.push({ col: i, stream: slot.stream });
      if (column === -1) column = i;
      else columns[i] = null;
    });
    if (column === -1) column = firstFree();

    drawn.add(node.id);

    // Where the node's parent links go. A link joins a column already
    // waiting for the same parent when that column sits to the left — the
    // rail bends into it right here, jj's `├─╯`, and the parent later
    // draws one incoming rail for all of them. Joining only leftward keeps
    // the leftmost (oldest, hottest) rail straight and lets the freed
    // column serve the next head. Trunk's spine never joins: it is the
    // line others converge on. Otherwise the first link keeps the node's
    // own column; further links merge into whatever waits for them or
    // open a fresh column. Elided links route the same way but render
    // dashed with a `~` row following. Parents already drawn above us only
    // happen on cyclic (i.e. broken) data — skip them so no column waits
    // forever.
    const directParents = new Set(node.parents);
    const parents = linkedParents(node, nodesById).filter(
      (id) => !drawn.has(id),
    );
    const edgesOut: Rail[] = [];
    const elidedMarks: Rail[] = [];
    const joined = new Set<number>();
    const mayJoin = !spine.has(node.id);
    let ownFree = true;
    parents.forEach((parent, index) => {
      const elided = !directParents.has(parent);
      const waiting = columns.findIndex((slot) => slot?.expects === parent);
      let target: number;
      if (
        waiting !== -1 &&
        (index > 0 || (mayJoin && waiting < column))
      ) {
        target = waiting;
        joined.add(target);
      } else if (ownFree) {
        target = column;
        ownFree = false;
        columns[target] = { expects: parent, stream };
      } else {
        target = firstFree();
        columns[target] = { expects: parent, stream };
      }
      // A rail leaving a node wears the node's stream, even where it joins
      // another stream's column: the bend is this change reaching its
      // parent, not the other line sprouting a branch.
      const rail: Rail = {
        col: target,
        stream,
        ...(elided ? { elided: true } : {}),
      };
      edgesOut.push(rail);
      if (elided) elidedMarks.push({ col: target, stream: rail.stream });
    });
    if (ownFree) columns[column] = null;
    columnCount = Math.max(columnCount, columns.length);
    while (columns.length > 0 && columns[columns.length - 1] === null) {
      columns.pop();
    }

    // Columns the row leaves alone keep running straight through it — a
    // joined column too: its own rail passes while ours bends into it.
    const involved = new Set<number>([
      column,
      ...edgesIn.map((rail) => rail.col),
      ...edgesOut.map((rail) => rail.col),
    ]);
    const passThrough: Rail[] = [];
    columns.forEach((slot, i) => {
      if (slot && (!involved.has(i) || joined.has(i))) {
        passThrough.push({ col: i, stream: slot.stream });
      }
    });

    const ws = headOf.get(node.id);
    rows.push({
      type: "node",
      node,
      column,
      stream,
      edgesIn,
      edgesOut,
      passThrough,
      isWorkingCopy: node.kind === "workingCopy",
      isStackHead: ws !== undefined,
      behindTrunk: ws?.behindTrunk ?? 0,
      bookmarks: bookmarksByTarget.get(node.id) ?? [],
    });

    // jj's `~`: a gap row right under the node, either on the way to a
    // drawn ancestor (rails continue) or where shown history ends.
    const terminal = parents.length === 0 && node.kind === "immutable";
    if (elidedMarks.length > 0 || terminal) {
      const marks = terminal ? [{ col: column, stream: null }] : elidedMarks;
      const marked = new Set(marks.map((rail) => rail.col));
      const elisionPass: Rail[] = [];
      columns.forEach((slot, i) => {
        if (slot && !marked.has(i)) {
          elisionPass.push({ col: i, stream: slot.stream });
        }
      });
      rows.push({
        type: "elision",
        id: `~${node.id}`,
        marks,
        continues: !terminal,
        passThrough: elisionPass,
      });
    }
  }

  return {
    rows,
    columnCount,
    trunkName: snapshot.trunkBookmark || null,
  };
}

/** Flat list of selectable node ids, top to bottom, for keyboard moves. */
export function selectableIds(model: GraphModel): string[] {
  return model.rows
    .filter((row): row is NodeRow => row.type === "node")
    .map((row) => row.node.id);
}

/** The workstream owning a node in this model, if any. */
export function streamOfNode(model: GraphModel, id: string): string | null {
  for (const row of model.rows) {
    if (row.type === "node" && row.node.id === id) return row.stream;
  }
  return null;
}

/** The focus mode's view of one workstream: the same row model the full
 * graph uses (so markers, chips, and rails carry over), built from just the
 * chain and the immutable base(s) it sits directly on. Elided links and
 * sibling-claimed parents stay out, so the lane ends at its base. */
export interface FocusModel {
  graph: GraphModel;
  /** True when the trunk bookmark points at one of the drawn bases. */
  trunkOnBase: boolean;
  /** Trunk commits the base lacks; 0 when the stack sits on trunk. */
  behindTrunk: number;
  trunkName: string | null;
}

export function buildFocusModel(
  snapshot: RepoSnapshot,
  workstream: WorkstreamSummary,
): FocusModel {
  const nodesById = new Map(snapshot.nodes.map((node) => [node.id, node]));
  const chain = new Set(
    workstream.nodeIds.filter((id) => nodesById.has(id)),
  );
  const baseIds = new Set<string>();
  for (const id of chain) {
    for (const parent of nodesById.get(id)!.parents) {
      if (!chain.has(parent) && nodesById.get(parent)?.kind === "immutable") {
        baseIds.add(parent);
      }
    }
  }

  // buildGraphModel only links to nodes present in the snapshot it gets, so
  // filtering is enough: parents outside the lane (siblings, elided spine
  // ancestors) simply do not draw, and each base ends in a terminal `~`.
  const graph = buildGraphModel({
    ...snapshot,
    nodes: snapshot.nodes.filter(
      (node) => chain.has(node.id) || baseIds.has(node.id),
    ),
    workstreams: [workstream],
  });

  const trunkOnBase = snapshot.bookmarks.some(
    (b) => b.isTrunk && baseIds.has(b.target),
  );
  return {
    graph,
    trunkOnBase,
    // The workstream-level count means "base is stale" only when trunk is
    // not the base we are drawing.
    behindTrunk: trunkOnBase ? 0 : workstream.behindTrunk,
    trunkName: snapshot.trunkBookmark || null,
  };
}

// The workstream the workbench treats as current: the explicitly focused
// one, falling back to the active (working-copy) workstream, then the first.
export function resolveFocusedWorkstream(
  snapshot: RepoSnapshot,
  focusedId: string | null,
): WorkstreamSummary | undefined {
  return (
    snapshot.workstreams.find((w) => w.id === focusedId) ??
    snapshot.workstreams.find((w) => w.isActive) ??
    snapshot.workstreams[0]
  );
}

// The workstream rendered hot in the graph: the one owning the selection
// when there is one, else the focused/active workstream.
export function emphasizedStreamId(
  model: GraphModel,
  snapshot: RepoSnapshot,
  selectedNodeId: string | null,
  focusedWorkstreamId: string | null,
): string | null {
  if (selectedNodeId) {
    const stream = streamOfNode(model, selectedNodeId);
    if (stream) return stream;
  }
  return resolveFocusedWorkstream(snapshot, focusedWorkstreamId)?.id ?? null;
}

export const SYNC_GLYPH: Record<
  SyncState,
  { glyph: string; tone: string; label: string }
> = {
  synced: { glyph: "✓", tone: "ok", label: "in sync with remote" },
  ahead: { glyph: "↑", tone: "warn", label: "ahead of remote" },
  behind: { glyph: "↓", tone: "warn", label: "behind remote" },
  diverged: { glyph: "⇅", tone: "danger", label: "diverged from remote" },
  localOnly: { glyph: "", tone: "muted", label: "local only" },
  remoteOnly: { glyph: "", tone: "muted", label: "remote only" },
};

/** Gutter geometry shared by the row renderers. Lanes come in tiers: the
 * leftmost `full` lanes — the working stack, the spine, whatever is
 * happening now — draw at full spacing; the next NARROW_LANES compress
 * into a tight band of hairlines; anything beyond shares the band's last
 * slot. The tiering is what keeps the gutter bounded no matter how wide
 * deep history gets, so the row's text always keeps its room. */
export const COL_WIDTH = 13;
/** Spacing inside the narrow band, and the step from the last full lane
 * into it. */
export const NARROW_WIDTH = 5;
export const NARROW_STEP = 9;
export const NARROW_LANES = 12;
/** Bounds on the full-spacing budget, however wide the pane. */
export const MIN_FULL_LANES = 4;
export const MAX_FULL_LANES = 14;
export const DEFAULT_FULL_LANES = 6;
/** Room the row's text keeps at any lane count: ids, a readable subject,
 * a chip or two, author, age. The lane budget is whatever the pane has
 * left after this. */
export const TEXT_RESERVE = 270;
/** Padding on either side of the gutter's rail span. */
export const GUTTER_PAD = 12;
export const NODE_ROW_HEIGHT = 28;
export const NODE_ROW_HEIGHT_CLASSIC = 46;
export const ELISION_ROW_HEIGHT = 18;

/** Row size preference: jj-dense one-liners, or the taller two-line rows
 * of classic Git clients. Owned by density.svelte.ts; the type lives here
 * so pure modules (palette, tests) can name it. */
export type RowDensity = "compact" | "classic";

export function nodeRowHeight(density: RowDensity): number {
  return density === "classic" ? NODE_ROW_HEIGHT_CLASSIC : NODE_ROW_HEIGHT;
}

/** Lane slots: a workstream's index in snapshot order, cycled through the
 * six `--lane-*` hues. Every surface that colors by stream (rails, node
 * markers, workstream dots) shares this map so hue stays identity. */
export const LANE_COUNT = 6;

export function laneMap(workstreams: WorkstreamSummary[]): Map<string, number> {
  return new Map(workstreams.map((w, i) => [w.id, i % LANE_COUNT]));
}

/** The CSS custom-property reference for a stream's lane, or null for
 * unowned/immutable context. */
export function laneVar(
  lanes: Map<string, number>,
  stream: string | null,
): string | undefined {
  if (stream === null) return undefined;
  return `var(--lane-${lanes.get(stream) ?? 0})`;
}
/** Corner radius where an edge bends into or out of a node. */
export const RAIL_CORNER = 6;

export type LaneTier = "full" | "narrow" | "overflow";

/** Which tier a column draws in under a given full-lane budget. */
export function laneTier(col: number, full = DEFAULT_FULL_LANES): LaneTier {
  if (col < full) return "full";
  if (col < full + NARROW_LANES) return "narrow";
  return "overflow";
}

function laneX(col: number, full: number): number {
  if (col < full) return GUTTER_PAD + COL_WIDTH * col;
  const narrow = Math.min(col - full, NARROW_LANES - 1);
  return GUTTER_PAD + COL_WIDTH * (full - 1) + NARROW_STEP + NARROW_WIDTH * narrow;
}

/** The x of a column's rail. Fractional columns interpolate between their
 * neighbors, so a tweened column count yields a smoothly moving edge. */
export function railX(col: number, full = DEFAULT_FULL_LANES): number {
  const budget = Math.max(1, Math.floor(full));
  const c = Math.max(0, col);
  const lo = Math.floor(c);
  const hi = Math.ceil(c);
  if (lo === hi) return laneX(lo, budget);
  const t = c - lo;
  return laneX(lo, budget) * (1 - t) + laneX(hi, budget) * t;
}

export function gutterWidth(
  columnCount: number,
  full = DEFAULT_FULL_LANES,
): number {
  return railX(Math.max(columnCount - 1, 0), full) + GUTTER_PAD;
}

/** How many lanes get full spacing in a pane this wide drawing this many
 * columns: the most that still leaves the text its reserve, counting the
 * narrow band the rest would need. Every lane draws full when they all
 * fit; a narrow pane keeps at least MIN_FULL_LANES regardless. */
export function fullLanesFor(paneWidth: number, columnCount: number): number {
  const room = paneWidth - TEXT_RESERVE - 2 * GUTTER_PAD;
  const top = Math.min(Math.max(columnCount, 1), MAX_FULL_LANES);
  for (let full = top; full > MIN_FULL_LANES; full -= 1) {
    const span = railX(Math.max(columnCount - 1, 0), full) - GUTTER_PAD;
    if (span <= room) return full;
  }
  return MIN_FULL_LANES;
}

/** A rail's job within its row. `pass` crosses without touching the node,
 * `in`/`out` end at or leave the node, `mark` carries an elision row's `~`. */
export type RailRole = "pass" | "in" | "out" | "mark";

export interface KeyedRail {
  key: string;
  role: RailRole;
  rail: Rail;
}

/** Rails keyed by what they are (role + owning stream), not where they sit.
 * Columns are exactly what a rewrite changes, so a column-keyed rail would
 * be destroyed and recreated on every layout shift; keyed this way the same
 * element survives and its x position can tween sideways — the morph that
 * explains the rewrite. Streams owning several rails in one role are
 * disambiguated by their order in the row (column-ascending), which is
 * stable across rebuilds. */
export function keyedRails(row: NodeRow): KeyedRail[] {
  return keyRails([
    ...row.passThrough.map((rail) => ({ role: "pass" as const, rail })),
    ...row.edgesIn.map((rail) => ({ role: "in" as const, rail })),
    ...row.edgesOut.map((rail) => ({ role: "out" as const, rail })),
  ]);
}

/** Same identity scheme for an elision row's rails and `~` marks. */
export function keyedElisionRails(row: ElisionRow): KeyedRail[] {
  return keyRails([
    ...row.passThrough.map((rail) => ({ role: "pass" as const, rail })),
    ...row.marks.map((rail) => ({ role: "mark" as const, rail })),
  ]);
}

function keyRails(entries: { role: RailRole; rail: Rail }[]): KeyedRail[] {
  const seen = new Map<string, number>();
  return entries.map(({ role, rail }) => {
    const base = `${role}:${rail.stream ?? "·"}`;
    const occurrence = seen.get(base) ?? 0;
    seen.set(base, occurrence + 1);
    return { key: `${base}:${occurrence}`, role, rail };
  });
}

/** An edge entering its node from above. Written against continuous x
 * positions so a rail mid-tween still draws sanely: the corner radius and
 * the horizontal reach both clamp to the shrinking gap, and a rail that
 * has (nearly) arrived at the node column degrades to the straight drop.
 * At rest — integral columns — the output is exactly the classic shape. */
export function railInPath(
  x: number,
  nx: number,
  height: number,
  clear: number,
): string {
  const cy = height / 2;
  const dx = nx - x;
  if (Math.abs(dx) < 0.5) return `M ${x} 0 V ${cy - clear}`;
  const s = dx > 0 ? 1 : -1; // direction of travel toward the node
  const r = Math.min(RAIL_CORNER, Math.abs(dx));
  const reach = Math.min(clear, Math.abs(dx) - r);
  return `M ${x} 0 V ${cy - r} Q ${x} ${cy} ${x + s * r} ${cy} H ${nx - s * reach}`;
}

/** An edge leaving its node toward the row below; same clamping rules. */
export function railOutPath(
  x: number,
  nx: number,
  height: number,
  clear: number,
): string {
  const cy = height / 2;
  const dx = x - nx;
  if (Math.abs(dx) < 0.5) return `M ${x} ${cy + clear} V ${height}`;
  const s = dx > 0 ? 1 : -1; // direction of travel away from the node
  const r = Math.min(RAIL_CORNER, Math.abs(dx));
  const reach = Math.min(clear, Math.abs(dx) - r);
  return `M ${nx + s * reach} ${cy} H ${x - s * r} Q ${x} ${cy} ${x} ${cy + r} V ${height}`;
}
