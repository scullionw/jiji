//! Shapes a `RepoSnapshot` into the dense, jj-native graph the workbench
//! renders: every workstream and the immutable bases they sit on in one
//! continuous tree. Rows come out the way `jj log` orders them — the
//! working stack pinned on top, then newest-first, each branch kept
//! together down to the base it grew from, with the trunk line one
//! connected spine and explicit elision markers (jj's `~`) where history is
//! hidden. Rails stay narrow by construction: a rail joins the rail already
//! waiting for the same parent as soon as it can (jj's `├─╯`), so twenty
//! stacks on trunk cost two columns, not twenty.

use std::collections::{HashMap, HashSet};

use jiji_core::snapshot::{
    BookmarkState, GraphNode, NodeKind, RepoSnapshot, SyncState, WorkstreamSummary,
};

use super::inspect::Tone;

/// One rail in a row's gutter. Rails are colored by the workstream that
/// owns them (its lane slot); `None` means immutable/unowned context.
/// Elided rails cross a gap in drawn history and render dashed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rail {
    pub col: usize,
    pub lane: Option<usize>,
    pub elided: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeRow {
    /// Index into `snapshot.nodes`.
    pub node: usize,
    pub column: usize,
    /// Workstream that claims this node (id and lane slot); `None` for
    /// immutable bases.
    pub stream: Option<String>,
    pub lane: Option<usize>,
    /// Rails entering from the row above, ending at this node.
    pub edges_in: Vec<Rail>,
    /// Rails leaving this node toward the row below.
    pub edges_out: Vec<Rail>,
    /// Rails crossing this row without touching the node.
    pub pass_through: Vec<Rail>,
    pub is_working_copy: bool,
    /// True for the top of a workstream chain.
    pub is_stack_head: bool,
    /// Commits trunk has that this stack lacks; head rows only, else 0.
    pub behind_trunk: u32,
    /// Indices into `snapshot.bookmarks` pointing at this node, trunk
    /// first.
    pub bookmarks: Vec<usize>,
}

/// jj's `~`: history exists here but is not shown. When the gap leads to a
/// drawn ancestor the marked columns continue below the row; otherwise the
/// rail ends.
#[derive(Debug, Clone, PartialEq)]
pub struct ElisionRow {
    /// Columns the `~` markers sit on.
    pub marks: Vec<Rail>,
    /// Whether the marked rails resume beneath the markers.
    pub continues: bool,
    pub pass_through: Vec<Rail>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GraphRow {
    Node(NodeRow),
    Elision(ElisionRow),
}

impl NodeRow {
    /// The rightmost column this row draws in: its node, and every rail
    /// that enters, leaves or crosses it.
    pub fn last_column(&self) -> usize {
        [&self.edges_in, &self.edges_out, &self.pass_through]
            .into_iter()
            .flatten()
            .map(|rail| rail.col)
            .fold(self.column, usize::max)
    }
}

impl ElisionRow {
    /// The rightmost column this row draws in.
    pub fn last_column(&self) -> usize {
        self.marks
            .iter()
            .chain(&self.pass_through)
            .map(|rail| rail.col)
            .max()
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct GraphModel {
    pub rows: Vec<GraphRow>,
    pub column_count: usize,
}

/// Drawn parents plus elided-history links: everything that constrains
/// ordering and rail routing, deduplicated, limited to drawn nodes.
fn linked_parents<'a>(node: &'a GraphNode, index: &HashMap<&str, usize>) -> Vec<&'a str> {
    let mut out: Vec<&str> = Vec::new();
    for id in node.parents.iter().chain(&node.elided_parents) {
        if index.contains_key(id.as_str()) && !out.contains(&id.as_str()) {
            out.push(id);
        }
    }
    out
}

fn time_of(node: &GraphNode) -> i64 {
    chrono::DateTime::parse_from_rfc3339(&node.timestamp)
        .map(|at| at.timestamp_millis())
        .unwrap_or(0)
}

/// Lane slots: a workstream's index in snapshot order, cycled through the
/// six lane hues. Every surface that colors by stream shares this so hue
/// stays identity.
pub const LANE_COUNT: usize = 6;

pub fn lane_of(snapshot: &RepoSnapshot, stream: &str) -> Option<usize> {
    snapshot
        .workstreams
        .iter()
        .position(|w| w.id == stream)
        .map(|slot| slot % LANE_COUNT)
}

pub fn build_graph_model(snapshot: &RepoSnapshot) -> GraphModel {
    build(snapshot, &snapshot.workstreams, |_| true, None)
}

/// `include` filters the drawn nodes; `lane_override` pins every stream's
/// lane (the focus view draws one workstream in its own hue).
fn build(
    snapshot: &RepoSnapshot,
    workstreams: &[WorkstreamSummary],
    include: impl Fn(&GraphNode) -> bool,
    lane_override: Option<usize>,
) -> GraphModel {
    let nodes: Vec<(usize, &GraphNode)> = snapshot
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| include(node))
        .collect();
    let index: HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(local, (_, node))| (node.id.as_str(), local))
        .collect();

    // Stream membership. The working (active) stack is pinned on top;
    // everything else orders by recency — a stream by its head's timestamp,
    // unclaimed nodes and bases by their own — so each stack lands right
    // above the base it grew from, the way jj log reads.
    let mut stream_of: HashMap<&str, (&str, usize)> = HashMap::new();
    let mut head_of: HashMap<&str, &WorkstreamSummary> = HashMap::new();
    let mut rank: HashMap<&str, [i64; 4]> = HashMap::new();
    for (si, ws) in workstreams.iter().enumerate() {
        let present: Vec<&str> = ws
            .node_ids
            .iter()
            .map(String::as_str)
            .filter(|id| index.contains_key(id))
            .collect();
        let stream_time = present
            .iter()
            .map(|id| time_of(nodes[index[id]].1))
            .max()
            .unwrap_or(0)
            .max(0);
        let lane = lane_override
            .or_else(|| lane_of(snapshot, &ws.id))
            .unwrap_or(si % LANE_COUNT);
        for (i, id) in present.iter().enumerate() {
            if rank.contains_key(id) {
                continue;
            }
            stream_of.insert(id, (ws.id.as_str(), lane));
            rank.insert(
                id,
                [i64::from(!ws.is_active), -stream_time, si as i64, i as i64],
            );
            if i == 0 {
                head_of.insert(id, ws);
            }
        }
    }
    let stream_count = workstreams.len() as i64;
    for (_, node) in &nodes {
        rank.entry(node.id.as_str())
            .or_insert([1, -time_of(node), stream_count, 0]);
    }
    let compare = |a: &&str, b: &&str| rank[*a].cmp(&rank[*b]).then_with(|| a.cmp(b));

    // Kahn's walk, children before parents (elided links count as parents
    // so the spine stays ordered). A branch is followed to its end before
    // the walk returns to the next head — the parent that just became
    // ready goes next, the way git's topo-order and jj's grouped log keep
    // one line of history together instead of interleaving parallel lines
    // by date. Only among heads does the rank above decide.
    let mut child_count: HashMap<&str, usize> = nodes
        .iter()
        .map(|(_, node)| (node.id.as_str(), 0))
        .collect();
    for (_, node) in &nodes {
        for parent in linked_parents(node, &index) {
            *child_count.get_mut(parent).unwrap() += 1;
        }
    }
    let mut ready: Vec<&str> = nodes
        .iter()
        .map(|(_, node)| node.id.as_str())
        .filter(|id| child_count[id] == 0)
        .collect();
    let mut order: Vec<usize> = Vec::with_capacity(nodes.len());
    let mut placed: HashSet<&str> = HashSet::new();
    let mut last: Option<usize> = None;
    while !ready.is_empty() {
        let follow = last.and_then(|last| {
            linked_parents(nodes[last].1, &index)
                .into_iter()
                .find(|parent| child_count[parent] == 0 && !placed.contains(parent))
        });
        let id = match follow {
            Some(id) => {
                ready.retain(|candidate| *candidate != id);
                id
            }
            None => {
                ready.sort_by(compare);
                ready.remove(0)
            }
        };
        let local = index[id];
        placed.insert(id);
        order.push(local);
        last = Some(local);
        for parent in linked_parents(nodes[local].1, &index) {
            let count = child_count.get_mut(parent).unwrap();
            *count -= 1;
            if *count == 0 {
                ready.push(parent);
            }
        }
    }
    // Real snapshots are acyclic; never drop rows if one ever is not.
    for (local, (_, node)) in nodes.iter().enumerate() {
        if !placed.contains(node.id.as_str()) {
            order.push(local);
        }
    }

    let mut bookmarks_by_target: HashMap<&str, Vec<usize>> = HashMap::new();
    for (ix, bookmark) in snapshot.bookmarks.iter().enumerate() {
        bookmarks_by_target
            .entry(bookmark.target.as_str())
            .or_default()
            .push(ix);
    }
    for list in bookmarks_by_target.values_mut() {
        list.sort_by_key(|&ix| !snapshot.bookmarks[ix].is_trunk);
    }

    // The trunk spine: trunk's first-parent line through the drawn nodes.
    // It is the graph's reference line, so it never bends into another
    // rail — everything else converges on it.
    let mut spine: HashSet<&str> = HashSet::new();
    let mut cursor = snapshot
        .bookmarks
        .iter()
        .find(|b| b.is_trunk)
        .map(|b| b.target.as_str());
    while let Some(id) = cursor {
        let Some(&local) = index.get(id) else { break };
        if !spine.insert(nodes[local].1.id.as_str()) {
            break;
        }
        cursor = linked_parents(nodes[local].1, &index).first().copied();
    }

    // Column machine: each active column waits for one parent id. A node
    // lands on the leftmost column waiting for it (other waiters merge in),
    // or opens a new column when nothing expects it (a head).
    struct Slot<'a> {
        expects: &'a str,
        lane: Option<usize>,
    }
    let mut columns: Vec<Option<Slot>> = Vec::new();
    let mut rows: Vec<GraphRow> = Vec::with_capacity(order.len() + 8);
    let mut drawn: HashSet<&str> = HashSet::new();
    let mut column_count = 0;

    fn first_free(columns: &mut Vec<Option<Slot>>) -> usize {
        match columns.iter().position(Option::is_none) {
            Some(free) => free,
            None => {
                columns.push(None);
                columns.len() - 1
            }
        }
    }

    for local in order {
        let (snapshot_ix, node) = nodes[local];
        let id = node.id.as_str();
        let (stream, lane) = match stream_of.get(id) {
            Some(&(stream, lane)) => (Some(stream), Some(lane)),
            None => (None, None),
        };
        let mut edges_in = Vec::new();
        let mut column = None;
        for (i, slot) in columns.iter_mut().enumerate() {
            let Some(waiting) = slot else { continue };
            if waiting.expects != id {
                continue;
            }
            edges_in.push(Rail {
                col: i,
                lane: waiting.lane,
                elided: false,
            });
            if column.is_none() {
                column = Some(i);
            } else {
                *slot = None;
            }
        }
        let column = column.unwrap_or_else(|| first_free(&mut columns));
        drawn.insert(id);

        // Where the node's parent links go. A link joins a column already
        // waiting for the same parent when that column sits to the left —
        // the rail bends into it right here, jj's `├─╯`, and the parent
        // later draws one incoming rail for all of them. Joining only
        // leftward keeps the leftmost (oldest, hottest) rail straight and
        // lets the freed column serve the next head. Trunk's spine never
        // joins: it is the line others converge on. Otherwise the first
        // link keeps the node's own column; further links merge into
        // whatever waits for them or open a fresh column. Elided links
        // route the same way but render dashed with a `~` row following.
        // Parents already drawn above us only happen on cyclic (broken)
        // data — skip them so no column waits forever.
        let parents: Vec<&str> = linked_parents(node, &index)
            .into_iter()
            .filter(|parent| !drawn.contains(parent))
            .collect();
        let mut edges_out = Vec::new();
        let mut elided_marks = Vec::new();
        let mut joined: HashSet<usize> = HashSet::new();
        let may_join = !spine.contains(id);
        let mut own_free = true;
        for (ix, parent) in parents.iter().enumerate() {
            let elided = !node.parents.iter().any(|direct| direct == parent);
            let waiting = columns
                .iter()
                .position(|slot| slot.as_ref().is_some_and(|slot| slot.expects == *parent));
            let target = match waiting {
                Some(waiting) if ix > 0 || (may_join && waiting < column) => {
                    joined.insert(waiting);
                    waiting
                }
                _ => {
                    let target = if own_free {
                        own_free = false;
                        column
                    } else {
                        first_free(&mut columns)
                    };
                    if target >= columns.len() {
                        columns.resize_with(target + 1, || None);
                    }
                    columns[target] = Some(Slot {
                        expects: parent,
                        lane,
                    });
                    target
                }
            };
            // A rail leaving a node wears the node's stream, even where it
            // joins another stream's column: the bend is this change
            // reaching its parent, not the other line sprouting a branch.
            let rail = Rail {
                col: target,
                lane,
                elided,
            };
            edges_out.push(rail);
            if elided {
                elided_marks.push(Rail {
                    elided: false,
                    ..rail
                });
            }
        }
        if own_free && column < columns.len() {
            columns[column] = None;
        }
        column_count = column_count.max(columns.len()).max(column + 1);
        while columns.last().is_some_and(Option::is_none) {
            columns.pop();
        }

        // Columns the row leaves alone keep running straight through it —
        // a joined column too: its own rail passes while ours bends into
        // it.
        let involved: HashSet<usize> = std::iter::once(column)
            .chain(edges_in.iter().map(|rail| rail.col))
            .chain(edges_out.iter().map(|rail| rail.col))
            .collect();
        let pass_through: Vec<Rail> = columns
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| {
                let slot = slot.as_ref()?;
                (!involved.contains(&i) || joined.contains(&i)).then_some(Rail {
                    col: i,
                    lane: slot.lane,
                    elided: false,
                })
            })
            .collect();

        let head = head_of.get(id);
        rows.push(GraphRow::Node(NodeRow {
            node: snapshot_ix,
            column,
            stream: stream.map(str::to_owned),
            lane,
            edges_in,
            edges_out,
            pass_through,
            is_working_copy: node.kind == NodeKind::WorkingCopy,
            is_stack_head: head.is_some(),
            behind_trunk: head.map_or(0, |ws| ws.behind_trunk),
            bookmarks: bookmarks_by_target.get(id).cloned().unwrap_or_default(),
        }));

        // jj's `~`: a gap row right under the node, either on the way to a
        // drawn ancestor (rails continue) or where shown history ends.
        let terminal = parents.is_empty() && node.kind == NodeKind::Immutable;
        if !elided_marks.is_empty() || terminal {
            let marks = if terminal {
                vec![Rail {
                    col: column,
                    lane: None,
                    elided: false,
                }]
            } else {
                elided_marks
            };
            let pass_through = columns
                .iter()
                .enumerate()
                .filter_map(|(i, slot)| {
                    let slot = slot.as_ref()?;
                    (!marks.iter().any(|mark| mark.col == i)).then_some(Rail {
                        col: i,
                        lane: slot.lane,
                        elided: false,
                    })
                })
                .collect();
            rows.push(GraphRow::Elision(ElisionRow {
                marks,
                continues: !terminal,
                pass_through,
            }));
        }
    }

    GraphModel { rows, column_count }
}

impl GraphModel {
    /// Selectable node rows (row index, node index), top to bottom, for
    /// keyboard moves.
    pub fn node_rows(&self) -> impl Iterator<Item = (usize, &NodeRow)> {
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(ix, row)| match row {
                GraphRow::Node(node) => Some((ix, node)),
                GraphRow::Elision(_) => None,
            })
    }

    /// The row a node draws on.
    pub fn row_of(&self, snapshot: &RepoSnapshot, id: &str) -> Option<(usize, &NodeRow)> {
        self.node_rows()
            .find(|(_, row)| snapshot.nodes[row.node].id == id)
    }
}

/// Flat list of selectable node ids, top to bottom.
pub fn selectable_ids(model: &GraphModel, snapshot: &RepoSnapshot) -> Vec<String> {
    model
        .node_rows()
        .map(|(_, row)| snapshot.nodes[row.node].id.clone())
        .collect()
}

/// The focus mode's view of one workstream: the same row model the full
/// graph uses (so markers, chips, and rails carry over), built from just
/// the chain and the immutable base(s) it sits directly on. Elided links
/// and sibling-claimed parents stay out, so the lane ends at its base.
#[derive(Debug, Clone, PartialEq)]
pub struct FocusModel {
    pub graph: GraphModel,
    /// True when the trunk bookmark points at one of the drawn bases.
    pub trunk_on_base: bool,
    /// Trunk commits the base lacks; 0 when the stack sits on trunk.
    pub behind_trunk: u32,
}

pub fn build_focus_model(snapshot: &RepoSnapshot, workstream: &WorkstreamSummary) -> FocusModel {
    let by_id: HashMap<&str, &GraphNode> = snapshot
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let chain: HashSet<&str> = workstream
        .node_ids
        .iter()
        .map(String::as_str)
        .filter(|id| by_id.contains_key(id))
        .collect();
    let mut bases: HashSet<&str> = HashSet::new();
    for id in &chain {
        for parent in &by_id[id].parents {
            let parent = parent.as_str();
            if !chain.contains(parent)
                && by_id
                    .get(parent)
                    .is_some_and(|node| node.kind == NodeKind::Immutable)
            {
                bases.insert(parent);
            }
        }
    }
    // The builder only links to nodes it is given, so filtering is enough:
    // parents outside the lane (siblings, elided spine ancestors) simply do
    // not draw, and each base ends in a terminal `~`.
    let graph = build(
        snapshot,
        std::slice::from_ref(workstream),
        |node| chain.contains(node.id.as_str()) || bases.contains(node.id.as_str()),
        lane_of(snapshot, &workstream.id),
    );
    let trunk_on_base = snapshot
        .bookmarks
        .iter()
        .any(|b| b.is_trunk && bases.contains(b.target.as_str()));
    FocusModel {
        graph,
        trunk_on_base,
        // The workstream-level count means "base is stale" only when trunk
        // is not the base we are drawing.
        behind_trunk: if trunk_on_base {
            0
        } else {
            workstream.behind_trunk
        },
    }
}

/// The workstream the workbench treats as current: the explicitly focused
/// one, falling back to the active (working-copy) workstream, then the
/// first.
pub fn resolve_focused_workstream<'a>(
    snapshot: &'a RepoSnapshot,
    focused_id: Option<&str>,
) -> Option<&'a WorkstreamSummary> {
    snapshot
        .workstreams
        .iter()
        .find(|w| Some(w.id.as_str()) == focused_id)
        .or_else(|| snapshot.workstreams.iter().find(|w| w.is_active))
        .or_else(|| snapshot.workstreams.first())
}

/// The workstream rendered hot in the graph: the one owning the selection
/// when there is one, else the focused/active workstream.
pub fn emphasized_stream(
    snapshot: &RepoSnapshot,
    selected: Option<&str>,
    focused: Option<&str>,
) -> Option<String> {
    selected
        .and_then(|id| super::inspect::stack_position(snapshot, id))
        .map(|position| position.workstream.id.clone())
        .or_else(|| resolve_focused_workstream(snapshot, focused).map(|w| w.id.clone()))
}

/// How a bookmark's sync state reads beside its name.
pub fn sync_glyph(sync: SyncState) -> (&'static str, Tone, &'static str) {
    match sync {
        SyncState::Synced => ("✓", Tone::Ok, "in sync with remote"),
        SyncState::Ahead => ("↑", Tone::Warn, "ahead of remote"),
        SyncState::Behind => ("↓", Tone::Warn, "behind remote"),
        SyncState::Diverged => ("⇅", Tone::Danger, "diverged from remote"),
        SyncState::LocalOnly => ("", Tone::Muted, "local only"),
        SyncState::RemoteOnly => ("", Tone::Muted, "remote only"),
    }
}

// ── Gutter geometry, shared by the row renderers ──
//
// Lanes come in tiers: the leftmost `full` lanes — the working stack, the
// spine, whatever is happening now — draw at full spacing; the next
// NARROW_LANES compress into a tight band of hairlines; anything beyond
// shares the band's last slot. The tiering keeps the gutter bounded no
// matter how wide deep history gets, so the row's text always keeps its
// room.

pub const COL_WIDTH: f32 = 13.;
/// Spacing inside the narrow band, and the step from the last full lane
/// into it.
pub const NARROW_WIDTH: f32 = 5.;
pub const NARROW_STEP: f32 = 9.;
pub const NARROW_LANES: usize = 12;
/// Bounds on the full-spacing budget, however wide the pane.
pub const MIN_FULL_LANES: usize = 4;
pub const MAX_FULL_LANES: usize = 14;
/// Room the row's text keeps at any lane count: ids, a readable subject,
/// a chip or two, author, age.
pub const TEXT_RESERVE: f32 = 270.;
/// Padding on either side of the gutter's rail span.
pub const GUTTER_PAD: f32 = 12.;
pub const NODE_ROW_HEIGHT: f32 = 28.;
pub const NODE_ROW_HEIGHT_CLASSIC: f32 = 46.;
pub const ELISION_ROW_HEIGHT: f32 = 18.;
/// Corner radius where an edge bends into or out of a node.
pub const RAIL_CORNER: f32 = 6.;

/// Row size preference: jj-dense one-liners, or the taller two-line rows
/// of classic Git clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowDensity {
    Compact,
    Classic,
}

pub fn node_row_height(density: RowDensity) -> f32 {
    match density {
        RowDensity::Compact => NODE_ROW_HEIGHT,
        RowDensity::Classic => NODE_ROW_HEIGHT_CLASSIC,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneTier {
    Full,
    Narrow,
    Overflow,
}

/// Which tier a column draws in under a given full-lane budget.
pub fn lane_tier(col: usize, full: usize) -> LaneTier {
    if col < full {
        LaneTier::Full
    } else if col < full + NARROW_LANES {
        LaneTier::Narrow
    } else {
        LaneTier::Overflow
    }
}

/// The x of a column's rail.
pub fn rail_x(col: usize, full: usize) -> f32 {
    let full = full.max(1);
    if col < full {
        return GUTTER_PAD + COL_WIDTH * col as f32;
    }
    let narrow = (col - full).min(NARROW_LANES - 1);
    GUTTER_PAD + COL_WIDTH * (full - 1) as f32 + NARROW_STEP + NARROW_WIDTH * narrow as f32
}

pub fn gutter_width(column_count: usize, full: usize) -> f32 {
    rail_x(column_count.saturating_sub(1), full) + GUTTER_PAD
}

/// The gutter one row needs: its own rails, not the widest row's. Text
/// hugs the tree the way `jj log` prints it, so a lone stack's titles
/// start beside their rail however wide deep history runs.
pub fn row_gutter_width(last_column: usize, full: usize) -> f32 {
    gutter_width(last_column + 1, full)
}

/// How many lanes get full spacing in a pane this wide drawing this many
/// columns: the most that still leaves the text its reserve, counting the
/// narrow band the rest would need. Every lane draws full when they all
/// fit; a narrow pane keeps at least MIN_FULL_LANES regardless.
pub fn full_lanes_for(pane_width: f32, column_count: usize) -> usize {
    let room = pane_width - TEXT_RESERVE - 2. * GUTTER_PAD;
    let top = column_count.clamp(1, MAX_FULL_LANES);
    let last = column_count.saturating_sub(1);
    let mut full = top;
    while full > MIN_FULL_LANES {
        if rail_x(last, full) - GUTTER_PAD <= room {
            return full;
        }
        full -= 1;
    }
    MIN_FULL_LANES
}

/// Bookmark states at a node, as the row renders them.
pub fn row_bookmarks<'a>(snapshot: &'a RepoSnapshot, row: &NodeRow) -> Vec<&'a BookmarkState> {
    row.bookmarks
        .iter()
        .map(|&ix| &snapshot.bookmarks[ix])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiji_core::{MockBackend, RepoBackend};

    fn snapshot() -> RepoSnapshot {
        let dir = std::env::temp_dir().join(format!("jiji-graph-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join(".jj"));
        MockBackend::default().open(&dir).unwrap()
    }

    fn node(id: &str, parents: &[&str], kind: NodeKind, minute: u32) -> GraphNode {
        GraphNode {
            id: id.into(),
            change_id: id.into(),
            commit_id: format!("c{id}"),
            description: id.into(),
            author: "a".into(),
            timestamp: format!("2026-01-01T00:{minute:02}:00Z"),
            kind,
            parents: parents.iter().map(|p| (*p).to_owned()).collect(),
            elided_parents: vec![],
            bookmarks: vec![],
            tags: vec![],
            is_empty: false,
            has_conflict: false,
            is_divergent: false,
        }
    }

    fn stream(id: &str, nodes: &[&str], active: bool) -> WorkstreamSummary {
        WorkstreamSummary {
            id: id.into(),
            title: id.into(),
            node_ids: nodes.iter().map(|n| (*n).to_owned()).collect(),
            bookmark: None,
            is_active: active,
            behind_trunk: 0,
        }
    }

    fn with(
        mut base: RepoSnapshot,
        nodes: Vec<GraphNode>,
        streams: Vec<WorkstreamSummary>,
    ) -> RepoSnapshot {
        base.nodes = nodes;
        base.workstreams = streams;
        base.bookmarks.clear();
        base
    }

    fn ids(model: &GraphModel, snapshot: &RepoSnapshot) -> Vec<String> {
        selectable_ids(model, snapshot)
    }

    #[test]
    fn every_node_draws_once_and_children_come_before_parents() {
        let snapshot = snapshot();
        let model = build_graph_model(&snapshot);
        let order = ids(&model, &snapshot);
        assert_eq!(order.len(), snapshot.nodes.len());
        let position: HashMap<&str, usize> = order
            .iter()
            .enumerate()
            .map(|(ix, id)| (id.as_str(), ix))
            .collect();
        for node in &snapshot.nodes {
            for parent in node.parents.iter().chain(&node.elided_parents) {
                if let Some(parent_at) = position.get(parent.as_str()) {
                    assert!(
                        position[node.id.as_str()] < *parent_at,
                        "{} above {parent}",
                        node.id
                    );
                }
            }
        }
    }

    #[test]
    fn the_active_stack_is_pinned_on_top_and_stacks_share_columns() {
        let snapshot = with(
            snapshot(),
            vec![
                node("base", &[], NodeKind::Immutable, 0),
                node("a1", &["base"], NodeKind::Mutable, 1),
                node("a2", &["a1"], NodeKind::WorkingCopy, 2),
                node("b1", &["base"], NodeKind::Mutable, 9),
                node("c1", &["base"], NodeKind::Mutable, 5),
            ],
            vec![
                stream("a", &["a2", "a1"], true),
                stream("b", &["b1"], false),
                stream("c", &["c1"], false),
            ],
        );
        let model = build_graph_model(&snapshot);
        // The active stack first (though it is the oldest), then the other
        // heads newest-first, then the base they all sit on.
        assert_eq!(ids(&model, &snapshot), ["a2", "a1", "b1", "c1", "base"]);
        // Three stacks on one base cost two columns, not three: each later
        // head bends into the rail already waiting for the base.
        assert_eq!(model.column_count, 2);
        let GraphRow::Node(base) = &model.rows[4] else {
            panic!("base row");
        };
        assert_eq!(base.column, 0);
        assert_eq!(base.edges_in.len(), 1);
        // The base is where drawn history ends: a terminal `~` follows.
        let GraphRow::Elision(gap) = &model.rows[5] else {
            panic!("terminal elision");
        };
        assert!(!gap.continues);
        let GraphRow::Node(head) = &model.rows[0] else {
            panic!("head row");
        };
        assert!(head.is_stack_head && head.is_working_copy);
        assert_eq!(head.lane, Some(0));
    }

    #[test]
    fn elided_links_draw_dashed_with_a_continuing_gap_row() {
        let mut old = node("old", &[], NodeKind::Immutable, 0);
        old.elided_parents.clear();
        let mut tip = node("tip", &[], NodeKind::Immutable, 5);
        tip.elided_parents = vec!["old".into()];
        let snapshot = with(snapshot(), vec![tip, old], vec![]);
        let model = build_graph_model(&snapshot);
        let GraphRow::Node(tip) = &model.rows[0] else {
            panic!("tip row");
        };
        assert!(tip.edges_out[0].elided);
        let GraphRow::Elision(gap) = &model.rows[1] else {
            panic!("gap row");
        };
        assert!(gap.continues);
        assert_eq!(gap.marks[0].col, 0);
    }

    #[test]
    fn the_focus_model_keeps_one_lane_and_its_base() {
        let snapshot = with(
            snapshot(),
            vec![
                node("base", &[], NodeKind::Immutable, 0),
                node("a1", &["base"], NodeKind::Mutable, 1),
                node("b1", &["base"], NodeKind::Mutable, 2),
            ],
            vec![stream("a", &["a1"], true), stream("b", &["b1"], false)],
        );
        let focus = build_focus_model(&snapshot, &snapshot.workstreams[1]);
        assert_eq!(ids(&focus.graph, &snapshot), ["b1", "base"]);
        // The lane keeps its own hue even drawn alone.
        let GraphRow::Node(head) = &focus.graph.rows[0] else {
            panic!("head row");
        };
        assert_eq!(head.lane, Some(1));
        assert!(!focus.trunk_on_base);
    }

    #[test]
    fn the_lane_budget_follows_the_pane() {
        // Everything draws full when it fits.
        assert_eq!(full_lanes_for(800., 6), 6);
        // A narrow pane keeps the minimum, however deep history gets.
        assert_eq!(full_lanes_for(320., 30), MIN_FULL_LANES);
        assert_eq!(lane_tier(3, 4), LaneTier::Full);
        assert_eq!(lane_tier(4, 4), LaneTier::Narrow);
        assert_eq!(lane_tier(40, 4), LaneTier::Overflow);
        // Overflow lanes share the narrow band's last slot.
        assert_eq!(rail_x(40, 4), rail_x(4 + NARROW_LANES - 1, 4));
        assert_eq!(gutter_width(1, 6), 2. * GUTTER_PAD);
    }

    #[test]
    fn a_row_gutter_covers_only_its_own_rails() {
        let rail = |col| Rail {
            col,
            lane: None,
            elided: false,
        };
        let lone = NodeRow {
            node: 0,
            column: 0,
            stream: None,
            lane: None,
            edges_in: vec![rail(0)],
            edges_out: vec![rail(0)],
            pass_through: Vec::new(),
            is_working_copy: false,
            is_stack_head: false,
            behind_trunk: 0,
            bookmarks: Vec::new(),
        };
        assert_eq!(lone.last_column(), 0);
        assert_eq!(row_gutter_width(lone.last_column(), 6), gutter_width(1, 6));
        // A rail crossing two lanes over widens the row to reach it.
        let crossed = ElisionRow {
            marks: vec![rail(1)],
            continues: true,
            pass_through: vec![rail(0), rail(2)],
        };
        assert_eq!(crossed.last_column(), 2);
        assert_eq!(
            row_gutter_width(crossed.last_column(), 6),
            gutter_width(3, 6)
        );
    }
}
