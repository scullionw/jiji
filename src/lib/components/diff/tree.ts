// Pure shaping for the diff pane's file tree: paths become a directory
// tree, runs of single-child directories compact into one row, and the
// tree flattens against a set of collapsed paths. No Svelte.

import type { FileDiff } from "$lib/bindings/FileDiff";
import type { FileStatus } from "$lib/bindings/FileStatus";
import { fileStats, type DiffStats } from "./diff";

export interface DirRow {
  kind: "dir";
  /// Full repo-relative prefix, no trailing slash — unique among rows and
  /// the collapse key.
  path: string;
  /// What the row shows: one segment, or a compacted run like
  /// "src/lib/components" when every level between holds nothing else.
  label: string;
  depth: number;
  /// Aggregates over every file underneath, for the collapsed summary.
  fileCount: number;
  stats: DiffStats;
  hasConflict: boolean;
}

export interface FileRow {
  kind: "file";
  path: string;
  name: string;
  depth: number;
  /// Position in the diff's file order — the scroll target.
  index: number;
  status: FileStatus;
  renamedFrom: string | null;
  hasConflict: boolean;
  stats: DiffStats;
}

export type TreeRow = DirRow | FileRow;

interface DirNode {
  path: string;
  label: string;
  dirs: Map<string, DirNode>;
  files: FileRow[];
}

/// Every row of the tree, depth-first, fully expanded. Directories come
/// before files at each level, both alphabetical — the file-browser
/// convention, which may differ from the diff's own file order; `index`
/// keeps each file pointed at its section regardless.
export function buildTreeRows(files: FileDiff[]): TreeRow[] {
  const root: DirNode = { path: "", label: "", dirs: new Map(), files: [] };
  files.forEach((file, index) => {
    const segments = file.path.split("/");
    const name = segments.pop()!;
    let node = root;
    for (const segment of segments) {
      let child = node.dirs.get(segment);
      if (!child) {
        child = {
          path: node.path === "" ? segment : `${node.path}/${segment}`,
          label: segment,
          dirs: new Map(),
          files: [],
        };
        node.dirs.set(segment, child);
      }
      node = child;
    }
    node.files.push({
      kind: "file",
      path: file.path,
      name,
      depth: 0,
      index,
      status: file.status,
      renamedFrom: file.renamedFrom,
      hasConflict: file.hasConflict,
      stats: fileStats(file),
    });
  });
  const rows: TreeRow[] = [];
  walk(root, 0, rows);
  return rows;
}

function walk(
  node: DirNode,
  depth: number,
  out: TreeRow[],
): Pick<DirRow, "fileCount" | "stats" | "hasConflict"> {
  let fileCount = 0;
  const stats: DiffStats = { added: 0, removed: 0 };
  let hasConflict = false;
  const names = [...node.dirs.keys()].sort((a, b) => a.localeCompare(b));
  for (const name of names) {
    let dir = node.dirs.get(name)!;
    // Compact pass-through levels: a chain of directories with a single
    // child each renders as one row, so depth is spent on real branching.
    let label = dir.label;
    while (dir.dirs.size === 1 && dir.files.length === 0) {
      const only = dir.dirs.values().next().value!;
      label = `${label}/${only.label}`;
      dir = only;
    }
    const row: DirRow = {
      kind: "dir",
      path: dir.path,
      label,
      depth,
      fileCount: 0,
      stats: { added: 0, removed: 0 },
      hasConflict: false,
    };
    out.push(row);
    const sub = walk(dir, depth + 1, out);
    row.fileCount = sub.fileCount;
    row.stats = sub.stats;
    row.hasConflict = sub.hasConflict;
    fileCount += sub.fileCount;
    stats.added += sub.stats.added;
    stats.removed += sub.stats.removed;
    hasConflict ||= sub.hasConflict;
  }
  const sorted = [...node.files].sort((a, b) => a.name.localeCompare(b.name));
  for (const file of sorted) {
    file.depth = depth;
    out.push(file);
    fileCount += 1;
    stats.added += file.stats.added;
    stats.removed += file.stats.removed;
    hasConflict ||= file.hasConflict;
  }
  return { fileCount, stats, hasConflict };
}

/// The rows on screen: a collapsed directory keeps its own row and drops
/// everything underneath.
export function visibleTreeRows(
  rows: TreeRow[],
  collapsed: ReadonlySet<string>,
): TreeRow[] {
  const out: TreeRow[] = [];
  // Depth of the shallowest collapsed row whose subtree we are inside;
  // rows at or below it are hidden until the walk surfaces again.
  let hideBelow = Infinity;
  for (const row of rows) {
    if (row.depth >= hideBelow) continue;
    out.push(row);
    hideBelow =
      row.kind === "dir" && collapsed.has(row.path) ? row.depth + 1 : Infinity;
  }
  return out;
}

/// Which row highlights for the file the diff is scrolled to: the file
/// itself, or — when an ancestor is collapsed — the shallowest collapsed
/// ancestor, which is the row standing in for it on screen.
export function activeRowKey(
  rows: TreeRow[],
  collapsed: ReadonlySet<string>,
  path: string,
): string {
  for (const row of rows) {
    if (
      row.kind === "dir" &&
      collapsed.has(row.path) &&
      path.startsWith(row.path + "/")
    ) {
      return row.path;
    }
  }
  return path;
}
