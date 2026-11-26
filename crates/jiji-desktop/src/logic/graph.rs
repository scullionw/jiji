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

/// Lane slots: a workstream's index in snapshot order, mapped onto the six
/// lane hues. Every surface that colors by stream shares this so hue stays
/// identity.
pub const LANE_COUNT: usize = 6;

/// The first stream (the working copy's, when there is one: the snapshot
/// lists it first) owns slot 0, the accent; the rest cycle through the
/// other five, so no other line ever wears the working line's color.
pub fn lane_slot(index: usize) -> usize {
    if index == 0 {
        0
    } else {
        1 + (index - 1) % (LANE_COUNT - 1)
    }
}

pub fn lane_of(snapshot: &RepoSnapshot, stream: &str) -> Option<usize> {
    snapshot
        .workstreams
        .iter()
        .position(|w| w.id == stream)
        .map(lane_slot)
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
            .unwrap_or(lane_slot(si));
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

impl FocusModel {
    /// The row where the base zone begins: the first node that is settled
    /// history. The rows before it are the workstream's own chain. A
    /// change someone else's branch holds is immutable to jj and still
    /// one of the chain's: it is not filed with the base.
    pub fn base_from(&self, snapshot: &RepoSnapshot) -> usize {
        self.graph
            .rows
            .iter()
            .position(
                |row| matches!(row, GraphRow::Node(node) if snapshot.nodes[node.node].is_settled()),
            )
            .unwrap_or(self.graph.rows.len())
    }
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

/// The rows a graph viewport has built, given each row's top and, last,
/// the list's height: those on screen, and one either side for the list's
/// padding and a row the edge cuts. A scroll re-renders the pane at the
/// offset it then paints at, so however far a fling goes in one frame it
/// never reveals a row that frame did not build; building more costs every
/// frame of the fling.
pub fn rows_in_view(tops: &[f32], scroll_top: f32, viewport: f32) -> std::ops::Range<usize> {
    let rows = tops.len().saturating_sub(1);
    let first = tops
        .partition_point(|top| *top <= scroll_top)
        .saturating_sub(2);
    let last = (tops.partition_point(|top| *top < scroll_top + viewport) + 1).min(rows);
    first..last
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

    #[test]
    fn a_viewport_builds_its_rows_and_one_either_side() {
        let tops: Vec<f32> = (0..=100).map(|row| row as f32 * 28.).collect();
        assert_eq!(rows_in_view(&tops, 0., 280.), 0..11);
        // Mid-list, at an offset that cuts a row: rows 20 to 30 show.
        assert_eq!(rows_in_view(&tops, 570., 280.), 19..32);
        assert_eq!(rows_in_view(&tops, 2520., 280.), 89..100);
    }

    /// Each row's top and, last, the content's height.
    fn tops_of(heights: &[f32]) -> Vec<f32> {
        let mut tops = vec![0.];
        for height in heights {
            tops.push(tops.last().unwrap() + height);
        }
        tops
    }

    #[test]
    fn nothing_to_show_builds_nothing_and_beyond_the_rows_builds_their_edge() {
        assert_eq!(rows_in_view(&[0.], 0., 800.), 0..0);
        let tops = tops_of(&[30., 30.]);
        // Scrolled far above or below, only the row by that edge is built.
        assert_eq!(rows_in_view(&tops, -800., 600.), 0..1);
        assert_eq!(rows_in_view(&tops, 600., 600.), 1..2);
    }

    #[test]
    fn a_viewport_builds_every_row_it_crosses_and_at_most_one_either_side() {
        let heights: Vec<f32> = (0..500)
            .map(|i| if i % 7 == 0 { 18. } else { 64. * 20.8 + 10.4 })
            .collect();
        let tops = tops_of(&heights);
        let mut top = -1200.;
        while top < tops[500] + 1200. {
            let built = rows_in_view(&tops, top, 820.);
            let crossed: Vec<usize> = (0..500)
                .filter(|&i| tops[i + 1] > top && tops[i] < top + 820.)
                .collect();
            for row in &crossed {
                assert!(built.contains(row), "row {row} at {top}");
            }
            let extra = built.len() - crossed.len();
            assert!(extra <= 2, "{extra} extra rows at {top}");
            top += 1379.;
        }
    }

    #[test]
    fn a_thousand_change_graph_builds_a_screenful_wherever_it_is_read() {
        let tops = tops_of(&[NODE_ROW_HEIGHT; 1000]);
        for top in [0., 8500., 17000., 27000.] {
            let built = rows_in_view(&tops, top, 820.);
            assert!(!built.is_empty() && built.len() < 50, "{built:?} at {top}");
        }
    }

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
            held: false,
            old_copies_held: false,
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

    /// Someone else's branch on a stack makes its changes immutable to
    /// jj. In Focus mode they are still the workstream's chain, above
    /// the base zone. (Found in review, and seen in a headless run: they
    /// were drawn under the heading "Trunk".)
    #[test]
    fn a_held_change_stays_in_the_focus_chain_and_out_of_the_base_zone() {
        let mut held = node("a1", &["base"], NodeKind::Immutable, 1);
        held.held = true;
        let snapshot = with(
            snapshot(),
            vec![
                node("base", &[], NodeKind::Immutable, 0),
                held,
                node("a2", &["a1"], NodeKind::Mutable, 2),
            ],
            vec![stream("a", &["a2", "a1"], true)],
        );
        let focus = build_focus_model(&snapshot, &snapshot.workstreams[0]);
        assert_eq!(ids(&focus.graph, &snapshot), ["a2", "a1", "base"]);
        let split = focus.base_from(&snapshot);
        assert!(
            matches!(&focus.graph.rows[split], GraphRow::Node(row) if snapshot.nodes[row.node].id == "base"),
            "the base zone starts at trunk, not at the held change"
        );
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
    fn only_the_first_stream_wears_the_accent_lane() {
        assert_eq!(lane_slot(0), 0);
        let rest: Vec<usize> = (1..12).map(lane_slot).collect();
        assert!(rest.iter().all(|&slot| (1..LANE_COUNT).contains(&slot)));
        assert_eq!(&rest[..6], &[1, 2, 3, 4, 5, 1]);
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

    // ── Ported from the Tauri app's graph.test.ts ──

    const NOON: &str = "2026-06-10T12:00:00Z";

    fn at(id: &str, kind: NodeKind, parents: &[&str], time: &str) -> GraphNode {
        GraphNode {
            timestamp: time.into(),
            ..node(id, parents, kind, 0)
        }
    }

    fn elided(mut node: GraphNode, links: &[&str]) -> GraphNode {
        node.elided_parents = links.iter().map(|id| (*id).to_owned()).collect();
        node
    }

    fn behind(mut stream: WorkstreamSummary, commits: u32) -> WorkstreamSummary {
        stream.behind_trunk = commits;
        stream
    }

    fn trunk_at(snapshot: &mut RepoSnapshot, target: &str) {
        let main = jiji_core::snapshot::BookmarkState {
            name: "main".into(),
            target: target.into(),
            remote: Some("origin".into()),
            sync: SyncState::Synced,
            is_trunk: true,
            is_local: true,
            tracked: true,
            is_merged: false,
            target_description: String::new(),
            target_author: String::new(),
            target_timestamp: String::new(),
        };
        snapshot.trunk_bookmark = "main".into();
        snapshot.bookmarks.push(main);
    }

    /// Every row's id, a `~` row named for the node it hangs under, as
    /// the old tests spelled them.
    fn rows_of(model: &GraphModel, snapshot: &RepoSnapshot) -> Vec<String> {
        let mut last = String::new();
        model
            .rows
            .iter()
            .map(|row| match row {
                GraphRow::Node(row) => {
                    last = snapshot.nodes[row.node].id.clone();
                    last.clone()
                }
                GraphRow::Elision(_) => format!("~{last}"),
            })
            .collect()
    }

    fn row<'a>(model: &'a GraphModel, snapshot: &RepoSnapshot, id: &str) -> &'a NodeRow {
        model.row_of(snapshot, id).expect("the node draws").1
    }

    fn elisions(model: &GraphModel) -> Vec<&ElisionRow> {
        model
            .rows
            .iter()
            .filter_map(|row| match row {
                GraphRow::Elision(gap) => Some(gap),
                GraphRow::Node(_) => None,
            })
            .collect()
    }

    /// A solid rail on `col` in lane `lane` (a stream's slot, `None` for
    /// immutable history).
    fn r(col: usize, lane: Option<usize>) -> Rail {
        Rail {
            col,
            lane,
            elided: false,
        }
    }

    fn dashed(col: usize, lane: Option<usize>) -> Rail {
        Rail {
            col,
            lane,
            elided: true,
        }
    }

    const A: Option<usize> = Some(0);
    const B: Option<usize> = Some(1);
    const BASE: Option<usize> = None;

    #[test]
    fn two_stacks_merge_onto_a_shared_trunk_base() {
        let mut snapshot = with(
            snapshot(),
            vec![
                at("wc", NodeKind::WorkingCopy, &["a1"], NOON),
                at("a1", NodeKind::Mutable, &["trunk"], NOON),
                at("b2", NodeKind::Mutable, &["b1"], NOON),
                at("b1", NodeKind::Mutable, &["trunk"], NOON),
                at("trunk", NodeKind::Immutable, &[], NOON),
            ],
            vec![
                stream("ws-a", &["wc", "a1"], true),
                behind(stream("ws-b", &["b2", "b1"], false), 2),
            ],
        );
        trunk_at(&mut snapshot, "trunk");
        let model = build_graph_model(&snapshot);
        // The working stack first, the sibling next, trunk last with the
        // end of drawn history below it.
        assert_eq!(
            rows_of(&model, &snapshot),
            ["wc", "a1", "b2", "b1", "trunk", "~trunk"]
        );
        let wc = row(&model, &snapshot, "wc");
        assert!(wc.column == 0 && wc.is_working_copy && wc.is_stack_head);
        assert_eq!(row(&model, &snapshot, "a1").column, 0);
        let b2 = row(&model, &snapshot, "b2");
        assert!(b2.column == 1 && b2.is_stack_head);
        assert_eq!(b2.behind_trunk, 2);
        // The working stack's rail passes the sibling's rows, and the
        // sibling joins it right at its bottom row (jj's ├─╯) wearing its
        // own stream: trunk takes one rail for both.
        assert_eq!(b2.pass_through, [r(0, A)]);
        let b1 = row(&model, &snapshot, "b1");
        assert_eq!(b1.pass_through, [r(0, A)]);
        assert_eq!(b1.edges_out, [r(0, B)]);
        let trunk = row(&model, &snapshot, "trunk");
        assert!(trunk.column == 0 && trunk.stream.is_none());
        assert_eq!(trunk.edges_in, [r(0, A)]);
        assert!(trunk.edges_out.is_empty());
        assert_eq!(row_bookmarks(&snapshot, trunk)[0].name, "main");
        let end = elisions(&model)[0];
        assert_eq!(end.marks, [r(0, BASE)]);
        assert!(!end.continues && end.pass_through.is_empty());
        assert_eq!(model.column_count, 2);
    }

    #[test]
    fn a_sibling_forking_off_the_middle_of_the_working_stack_interleaves() {
        let mut snapshot = with(
            snapshot(),
            vec![
                at("wc", NodeKind::WorkingCopy, &["x"], NOON),
                at("x", NodeKind::Mutable, &["low"], NOON),
                at("low", NodeKind::Mutable, &["trunk"], NOON),
                at("s1", NodeKind::Mutable, &["x"], NOON),
                at("trunk", NodeKind::Immutable, &[], NOON),
            ],
            vec![
                stream("ws-a", &["wc", "x", "low"], true),
                stream("ws-b", &["s1"], false),
            ],
        );
        trunk_at(&mut snapshot, "trunk");
        let model = build_graph_model(&snapshot);
        // s1 is x's child, so it draws above x for all the working stack's
        // priority, and bends into the rail already waiting for x.
        assert_eq!(
            rows_of(&model, &snapshot),
            ["wc", "s1", "x", "low", "trunk", "~trunk"]
        );
        let s1 = row(&model, &snapshot, "s1");
        assert_eq!(s1.column, 1);
        assert_eq!(s1.edges_out, [r(0, B)]);
        let x = row(&model, &snapshot, "x");
        assert_eq!(x.column, 0);
        assert_eq!(x.edges_in, [r(0, A)]);
    }

    #[test]
    fn a_merge_s_second_parent_routes_into_the_column_waiting_for_it() {
        let snapshot = with(
            snapshot(),
            vec![
                at("t1", NodeKind::Mutable, &["p2"], NOON),
                at("m", NodeKind::Mutable, &["p1", "p2"], NOON),
                at("p1", NodeKind::Mutable, &[], NOON),
                at("p2", NodeKind::Mutable, &[], NOON),
            ],
            vec![
                stream("ws-1", &["t1"], true),
                stream("ws-2", &["m", "p1"], false),
            ],
        );
        let model = build_graph_model(&snapshot);
        assert_eq!(rows_of(&model, &snapshot), ["t1", "m", "p1", "p2"]);
        // The first parent keeps m's column; the second merges into
        // ws-1's, which already waits for p2. Both rails leave m, so both
        // wear its stream, and ws-1's own rail passes underneath.
        let m = row(&model, &snapshot, "m");
        assert_eq!(m.column, 1);
        assert_eq!(m.edges_out, [r(1, B), r(0, B)]);
        assert_eq!(m.pass_through, [r(0, A)]);
        let p2 = row(&model, &snapshot, "p2");
        assert_eq!(p2.column, 0);
        assert_eq!(p2.edges_in, [r(0, A)]);
        // p2 is mutable and parentless: the rail ends with no `~`.
        assert!(elisions(&model).is_empty());
    }

    #[test]
    fn a_merge_parent_nothing_waits_for_opens_a_column() {
        let mut snapshot = with(
            snapshot(),
            vec![
                at("wc", NodeKind::WorkingCopy, &["a1", "b1"], NOON),
                at("a1", NodeKind::Mutable, &["trunk"], NOON),
                at("b1", NodeKind::Mutable, &["trunk"], NOON),
                at("trunk", NodeKind::Immutable, &[], NOON),
            ],
            vec![
                stream("ws-a", &["wc", "a1"], true),
                stream("ws-b", &["b1"], false),
            ],
        );
        trunk_at(&mut snapshot, "trunk");
        let model = build_graph_model(&snapshot);
        assert_eq!(row(&model, &snapshot, "wc").edges_out, [r(0, A), r(1, A)]);
        // b1 lands on the column the merge opened for it.
        let b1 = row(&model, &snapshot, "b1");
        assert_eq!(b1.column, 1);
        assert_eq!(b1.edges_in, [r(1, A)]);
    }

    #[test]
    fn each_stack_stays_with_the_base_it_grew_from() {
        // The working stack sits on an old base nothing links to trunk; a
        // sibling sits on trunk. Each base lands right under the work that
        // grew from it, the walk following a branch to its end first.
        let mut snapshot = with(
            snapshot(),
            vec![
                at(
                    "wc",
                    NodeKind::WorkingCopy,
                    &["old"],
                    "2026-06-11T10:00:00Z",
                ),
                at("b1", NodeKind::Mutable, &["trunk"], "2026-06-10T11:00:00Z"),
                at("trunk", NodeKind::Immutable, &[], "2026-06-10T08:00:00Z"),
                at("old", NodeKind::Immutable, &[], "2026-06-01T12:00:00Z"),
            ],
            vec![
                behind(stream("ws-a", &["wc"], true), 5),
                stream("ws-b", &["b1"], false),
            ],
        );
        trunk_at(&mut snapshot, "trunk");
        let model = build_graph_model(&snapshot);
        assert_eq!(
            rows_of(&model, &snapshot),
            ["wc", "old", "~old", "b1", "trunk", "~trunk"]
        );
        // One line at a time: nothing passes the closed-off base, and the
        // freed column serves the sibling.
        assert!(elisions(&model)[0].pass_through.is_empty());
        assert_eq!(row(&model, &snapshot, "wc").behind_trunk, 5);
        assert_eq!(row(&model, &snapshot, "b1").column, 0);
        assert_eq!(model.column_count, 1);
    }

    #[test]
    fn a_branch_is_followed_to_its_base_before_the_next_head() {
        // A lens drawing unclaimed history: h1's parent is older than the
        // other head, yet stays with h1, git's topo-order.
        let snapshot = with(
            snapshot(),
            vec![
                at("h1", NodeKind::Immutable, &["p1"], "2026-06-10T10:00:00Z"),
                at("h2", NodeKind::Immutable, &["base"], "2026-06-08T10:00:00Z"),
                at("p1", NodeKind::Immutable, &["base"], "2026-06-05T10:00:00Z"),
                at("base", NodeKind::Immutable, &[], "2026-06-01T10:00:00Z"),
            ],
            vec![],
        );
        let model = build_graph_model(&snapshot);
        assert_eq!(
            rows_of(&model, &snapshot),
            ["h1", "p1", "h2", "base", "~base"]
        );
    }

    #[test]
    fn every_stack_joins_the_rail_already_waiting_for_its_base() {
        let heads = ["b", "c", "d", "e", "f"];
        let mut nodes = vec![
            at("wc", NodeKind::WorkingCopy, &["a1"], NOON),
            at("a1", NodeKind::Mutable, &["trunk"], NOON),
        ];
        let mut streams = vec![stream("ws-a", &["wc", "a1"], true)];
        for head in heads {
            let (top, bottom) = (format!("{head}2"), format!("{head}1"));
            nodes.push(at(&top, NodeKind::Mutable, &[&bottom], NOON));
            nodes.push(at(&bottom, NodeKind::Mutable, &["trunk"], NOON));
            streams.push(stream(&format!("ws-{head}"), &[&top, &bottom], false));
        }
        nodes.push(at("trunk", NodeKind::Immutable, &[], NOON));
        let mut snapshot = with(snapshot(), nodes, streams);
        trunk_at(&mut snapshot, "trunk");
        let model = build_graph_model(&snapshot);
        // Two columns, not six; each bottom row bends into the working
        // stack's rail, which keeps passing the row the bend lands in.
        assert_eq!(model.column_count, 2);
        for (ix, head) in heads.iter().enumerate() {
            let bottom = row(&model, &snapshot, &format!("{head}1"));
            assert_eq!(bottom.column, 1);
            assert_eq!(bottom.edges_out, [r(0, Some(lane_slot(ix + 1)))]);
            assert_eq!(bottom.pass_through, [r(0, A)]);
        }
        assert_eq!(row(&model, &snapshot, "trunk").edges_in, [r(0, A)]);
    }

    #[test]
    fn teammates_immutable_branches_join_one_base_the_same_way() {
        // The Team lens: remote branches are immutable but not trunk's
        // spine, so they converge as stacks do.
        let mut snapshot = with(
            snapshot(),
            vec![
                at(
                    "wc",
                    NodeKind::WorkingCopy,
                    &["trunk"],
                    "2026-06-11T10:00:00Z",
                ),
                at(
                    "r1",
                    NodeKind::Immutable,
                    &["trunk"],
                    "2026-06-10T10:00:00Z",
                ),
                at(
                    "s1",
                    NodeKind::Immutable,
                    &["trunk"],
                    "2026-06-09T10:00:00Z",
                ),
                at("trunk", NodeKind::Immutable, &[], "2026-06-01T10:00:00Z"),
            ],
            vec![stream("ws-a", &["wc"], true)],
        );
        trunk_at(&mut snapshot, "trunk");
        let model = build_graph_model(&snapshot);
        assert_eq!(model.column_count, 2);
        assert_eq!(row(&model, &snapshot, "r1").edges_out, [r(0, BASE)]);
        let s1 = row(&model, &snapshot, "s1");
        assert_eq!(s1.column, 1);
        assert_eq!(s1.edges_out, [r(0, BASE)]);
        assert_eq!(row(&model, &snapshot, "trunk").edges_in, [r(0, A)]);
    }

    #[test]
    fn rails_join_only_leftward_so_the_leftmost_keeps_its_column() {
        // m merges p and q; the walk reaches q first, which waits for base
        // on column 1. p, on column 0, does not bend right into that
        // younger rail: the two meet at base. r bends left into p's.
        let snapshot = with(
            snapshot(),
            vec![
                at("m", NodeKind::Mutable, &["p", "q"], "2026-06-11T10:00:00Z"),
                at("r", NodeKind::Immutable, &["p"], "2026-06-08T10:00:00Z"),
                at("q", NodeKind::Immutable, &["base"], "2026-06-10T10:00:00Z"),
                at("p", NodeKind::Immutable, &["base"], "2026-06-09T10:00:00Z"),
                at("base", NodeKind::Immutable, &[], "2026-06-01T10:00:00Z"),
            ],
            vec![stream("ws-m", &["m"], true)],
        );
        let model = build_graph_model(&snapshot);
        assert_eq!(
            rows_of(&model, &snapshot),
            ["m", "q", "r", "p", "base", "~base"]
        );
        assert_eq!(row(&model, &snapshot, "q").column, 1);
        let r_row = row(&model, &snapshot, "r");
        assert_eq!(r_row.column, 2);
        assert_eq!(r_row.edges_out, [r(0, BASE)]);
        let p = row(&model, &snapshot, "p");
        assert_eq!(p.column, 0);
        assert_eq!(p.edges_out, [r(0, BASE)]);
        assert_eq!(
            row(&model, &snapshot, "base").edges_in,
            [r(0, BASE), r(1, BASE)]
        );
        assert_eq!(model.column_count, 3);
    }

    #[test]
    fn trunk_s_spine_never_bends_into_a_waiting_rail() {
        let mut snapshot = with(
            snapshot(),
            vec![
                at(
                    "wc",
                    NodeKind::WorkingCopy,
                    &["old"],
                    "2026-06-11T10:00:00Z",
                ),
                elided(
                    at("trunk", NodeKind::Immutable, &[], "2026-06-10T08:00:00Z"),
                    &["old"],
                ),
                at("old", NodeKind::Immutable, &[], "2026-06-01T12:00:00Z"),
            ],
            vec![stream("ws-a", &["wc"], true)],
        );
        trunk_at(&mut snapshot, "trunk");
        let model = build_graph_model(&snapshot);
        assert_eq!(
            rows_of(&model, &snapshot),
            ["wc", "trunk", "~trunk", "old", "~old"]
        );
        let trunk = row(&model, &snapshot, "trunk");
        assert_eq!(trunk.column, 1);
        assert_eq!(trunk.edges_out, [dashed(1, BASE)]);
        let gap = elisions(&model)[0];
        assert_eq!(gap.marks, [r(1, BASE)]);
        assert_eq!(gap.pass_through, [r(0, A)]);
        assert_eq!(
            row(&model, &snapshot, "old").edges_in,
            [r(0, A), r(1, BASE)]
        );
    }

    #[test]
    fn sibling_stacks_order_by_their_head_s_recency_not_their_listing() {
        let snapshot = with(
            snapshot(),
            vec![
                at("o1", NodeKind::Mutable, &["base"], "2026-06-01T10:00:00Z"),
                at("n1", NodeKind::Mutable, &["base"], "2026-06-10T10:00:00Z"),
                at("base", NodeKind::Immutable, &[], "2026-05-01T10:00:00Z"),
            ],
            vec![
                stream("ws-old", &["o1"], false),
                stream("ws-new", &["n1"], false),
            ],
        );
        let model = build_graph_model(&snapshot);
        assert_eq!(rows_of(&model, &snapshot), ["n1", "o1", "base", "~base"]);
    }

    #[test]
    fn bases_linked_through_elided_history_form_one_continuing_spine() {
        let mut snapshot = with(
            snapshot(),
            vec![
                at(
                    "wc",
                    NodeKind::WorkingCopy,
                    &["trunk"],
                    "2026-06-11T10:00:00Z",
                ),
                at("b1", NodeKind::Mutable, &["old"], "2026-06-09T10:00:00Z"),
                elided(
                    at("trunk", NodeKind::Immutable, &[], "2026-06-10T08:00:00Z"),
                    &["old"],
                ),
                at("old", NodeKind::Immutable, &[], "2026-06-01T12:00:00Z"),
            ],
            vec![
                stream("ws-a", &["wc"], true),
                stream("ws-b", &["b1"], false),
            ],
        );
        trunk_at(&mut snapshot, "trunk");
        let model = build_graph_model(&snapshot);
        assert_eq!(
            rows_of(&model, &snapshot),
            ["wc", "trunk", "~trunk", "b1", "old", "~old"]
        );
        // The elided link draws dashed and keeps trunk's column alive.
        assert_eq!(row(&model, &snapshot, "trunk").edges_out, [dashed(0, BASE)]);
        let gaps = elisions(&model);
        assert_eq!(gaps[0].marks, [r(0, BASE)]);
        assert!(gaps[0].continues);
        // The sibling bends into the spine, already waiting for old
        // beneath the gap; old takes the spine alone, and history ends.
        let b1 = row(&model, &snapshot, "b1");
        assert_eq!(b1.column, 1);
        assert_eq!(b1.edges_out, [r(0, B)]);
        assert_eq!(b1.pass_through, [r(0, BASE)]);
        let old = row(&model, &snapshot, "old");
        assert_eq!(old.column, 0);
        assert_eq!(old.edges_in, [r(0, BASE)]);
        assert!(!gaps[1].continues);
    }

    #[test]
    fn directly_linked_bases_draw_without_a_gap_row() {
        let snapshot = with(
            snapshot(),
            vec![
                at("wc", NodeKind::WorkingCopy, &["t2"], "2026-06-11T10:00:00Z"),
                at("b1", NodeKind::Mutable, &["t1"], "2026-06-09T10:00:00Z"),
                at("t2", NodeKind::Immutable, &["t1"], "2026-06-10T08:00:00Z"),
                at("t1", NodeKind::Immutable, &[], "2026-06-01T12:00:00Z"),
            ],
            vec![
                stream("ws-a", &["wc"], true),
                stream("ws-b", &["b1"], false),
            ],
        );
        let model = build_graph_model(&snapshot);
        assert_eq!(rows_of(&model, &snapshot), ["wc", "t2", "b1", "t1", "~t1"]);
        assert_eq!(row(&model, &snapshot, "t2").edges_out, [r(0, BASE)]);
    }

    #[test]
    fn a_column_freed_by_a_merge_serves_the_next_head() {
        let snapshot = with(
            snapshot(),
            vec![
                at("a2", NodeKind::Mutable, &["a1"], NOON),
                at("a1", NodeKind::Mutable, &["base"], NOON),
                at("b1", NodeKind::Mutable, &["a1"], NOON),
                at("c1", NodeKind::Mutable, &["base"], NOON),
                at("base", NodeKind::Immutable, &[], NOON),
            ],
            vec![
                stream("ws-a", &["a2", "a1"], true),
                stream("ws-b", &["b1"], false),
                stream("ws-c", &["c1"], false),
            ],
        );
        let model = build_graph_model(&snapshot);
        assert_eq!(
            rows_of(&model, &snapshot),
            ["a2", "b1", "a1", "c1", "base", "~base"]
        );
        assert_eq!(row(&model, &snapshot, "c1").column, 1);
        assert_eq!(model.column_count, 2);
    }

    #[test]
    fn a_stream_s_ids_missing_from_the_snapshot_are_passed_over() {
        let snapshot = with(
            snapshot(),
            vec![at("wc", NodeKind::WorkingCopy, &[], NOON)],
            vec![stream("ws", &["gone", "wc"], true)],
        );
        let model = build_graph_model(&snapshot);
        assert_eq!(rows_of(&model, &snapshot), ["wc"]);
        // The first node present stands in as the stack's visible top.
        let wc = row(&model, &snapshot, "wc");
        assert!(wc.is_stack_head);
        assert_eq!(wc.stream.as_deref(), Some("ws"));
    }

    fn two_stacks_on_trunk() -> RepoSnapshot {
        let mut snapshot = with(
            snapshot(),
            vec![
                at("wc", NodeKind::WorkingCopy, &["a1"], NOON),
                at("a1", NodeKind::Mutable, &["trunk"], NOON),
                at("b2", NodeKind::Mutable, &["b1"], NOON),
                at("b1", NodeKind::Mutable, &["trunk"], NOON),
                at("trunk", NodeKind::Immutable, &[], NOON),
            ],
            vec![
                stream("ws-a", &["wc", "a1"], true),
                behind(stream("ws-b", &["b2", "b1"], false), 2),
            ],
        );
        trunk_at(&mut snapshot, "trunk");
        snapshot
    }

    #[test]
    fn the_focus_view_draws_one_stream_on_its_base_in_one_column() {
        let snapshot = two_stacks_on_trunk();
        let focus = build_focus_model(&snapshot, &snapshot.workstreams[1]);
        // The sibling stays out, so the lane is one straight rail.
        assert_eq!(
            rows_of(&focus.graph, &snapshot),
            ["b2", "b1", "trunk", "~trunk"]
        );
        assert_eq!(focus.graph.column_count, 1);
        assert!(focus.graph.node_rows().all(|(_, row)| row.column == 0));
        // Trunk is the base drawn, so the stale-base count stays quiet.
        assert!(focus.trunk_on_base);
        assert_eq!(focus.behind_trunk, 0);
    }

    #[test]
    fn the_focus_view_keeps_the_stale_base_count_behind_trunk() {
        let mut snapshot = with(
            snapshot(),
            vec![
                at("wc", NodeKind::WorkingCopy, &["old"], NOON),
                at("trunk", NodeKind::Immutable, &[], NOON),
                at("old", NodeKind::Immutable, &[], NOON),
            ],
            vec![behind(stream("ws-a", &["wc"], true), 5)],
        );
        trunk_at(&mut snapshot, "trunk");
        let focus = build_focus_model(&snapshot, &snapshot.workstreams[0]);
        assert_eq!(rows_of(&focus.graph, &snapshot), ["wc", "old", "~old"]);
        assert!(!focus.trunk_on_base);
        assert_eq!(focus.behind_trunk, 5);
    }

    #[test]
    fn the_focus_view_ends_the_lane_at_its_base_past_elided_links() {
        let mut snapshot = with(
            snapshot(),
            vec![
                at("wc", NodeKind::WorkingCopy, &["trunk"], NOON),
                elided(
                    at("trunk", NodeKind::Immutable, &[], "2026-06-10T08:00:00Z"),
                    &["old"],
                ),
                at("old", NodeKind::Immutable, &[], NOON),
            ],
            vec![stream("ws-a", &["wc"], true)],
        );
        trunk_at(&mut snapshot, "trunk");
        let focus = build_focus_model(&snapshot, &snapshot.workstreams[0]);
        assert_eq!(rows_of(&focus.graph, &snapshot), ["wc", "trunk", "~trunk"]);
        assert!(!elisions(&focus.graph)[0].continues);
    }

    #[test]
    fn the_focus_view_drops_a_parent_another_stack_claims() {
        let mut snapshot = with(
            snapshot(),
            vec![
                at("wc", NodeKind::WorkingCopy, &["x"], NOON),
                at("x", NodeKind::Mutable, &["trunk"], NOON),
                at("s1", NodeKind::Mutable, &["x"], NOON),
                at("trunk", NodeKind::Immutable, &[], NOON),
            ],
            vec![
                stream("ws-a", &["wc", "x"], true),
                stream("ws-b", &["s1"], false),
            ],
        );
        trunk_at(&mut snapshot, "trunk");
        let focus = build_focus_model(&snapshot, &snapshot.workstreams[1]);
        // s1's parent is mutable and ws-a's: no base, the rail just ends.
        assert_eq!(rows_of(&focus.graph, &snapshot), ["s1"]);
        assert!(elisions(&focus.graph).is_empty());
    }

    fn one_stack_and_a_sibling() -> RepoSnapshot {
        let mut snapshot = with(
            snapshot(),
            vec![
                at("wc", NodeKind::WorkingCopy, &["trunk"], NOON),
                at("b1", NodeKind::Mutable, &["trunk"], NOON),
                at("trunk", NodeKind::Immutable, &[], NOON),
            ],
            vec![
                stream("ws-a", &["wc"], true),
                stream("ws-b", &["b1"], false),
            ],
        );
        trunk_at(&mut snapshot, "trunk");
        snapshot
    }

    #[test]
    fn only_nodes_are_selectable_and_each_knows_its_stream() {
        let snapshot = one_stack_and_a_sibling();
        let model = build_graph_model(&snapshot);
        assert_eq!(ids(&model, &snapshot), ["wc", "b1", "trunk"]);
        assert_eq!(row(&model, &snapshot, "b1").stream.as_deref(), Some("ws-b"));
        assert_eq!(row(&model, &snapshot, "trunk").stream, None);
        assert!(model.row_of(&snapshot, "nope").is_none());
    }

    #[test]
    fn the_hot_stream_is_the_selection_s_then_the_focused_one() {
        let snapshot = one_stack_and_a_sibling();
        assert_eq!(
            emphasized_stream(&snapshot, Some("b1"), None).as_deref(),
            Some("ws-b")
        );
        // A base selection keeps the focused (or active) stream hot.
        assert_eq!(
            emphasized_stream(&snapshot, Some("trunk"), Some("ws-b")).as_deref(),
            Some("ws-b")
        );
        assert_eq!(
            emphasized_stream(&snapshot, None, None).as_deref(),
            Some("ws-a")
        );
    }

    #[test]
    fn the_focused_stream_falls_back_to_the_active_then_the_first() {
        let snapshot = one_stack_and_a_sibling();
        let focused = |id| resolve_focused_workstream(&snapshot, id).map(|w| w.id.as_str());
        assert_eq!(focused(Some("ws-b")), Some("ws-b"));
        assert_eq!(focused(Some("missing")), Some("ws-a"));
        assert_eq!(focused(None), Some("ws-a"));
    }

    #[test]
    fn full_lanes_draw_at_full_spacing_then_a_narrow_band() {
        assert_eq!(rail_x(0, 6), 12.);
        assert_eq!(rail_x(5, 6), 77.);
        // The first narrow lane steps in closer than a full one would.
        assert_eq!(rail_x(6, 6), 86.);
        assert_eq!(rail_x(7, 6), 91.);
    }

    #[test]
    fn past_the_narrow_band_lanes_saturate_so_the_gutter_is_bounded() {
        let last = rail_x(6 + NARROW_LANES - 1, 6);
        assert_eq!(rail_x(6 + NARROW_LANES, 6), last);
        assert_eq!(rail_x(500, 6), last);
        assert_eq!(gutter_width(500, 6), last + 12.);
    }

    #[test]
    fn a_graph_inside_the_budget_keeps_the_classic_gutter() {
        assert_eq!(gutter_width(1, MAX_FULL_LANES), 24.);
        assert_eq!(gutter_width(2, MAX_FULL_LANES), 37.);
        assert_eq!(gutter_width(0, MAX_FULL_LANES), 24.);
    }

    #[test]
    fn each_column_s_tier_follows_the_budget() {
        assert_eq!(lane_tier(0, 6), LaneTier::Full);
        assert_eq!(lane_tier(5, 6), LaneTier::Full);
        assert_eq!(lane_tier(6, 6), LaneTier::Narrow);
        assert_eq!(lane_tier(6 + NARROW_LANES - 1, 6), LaneTier::Narrow);
        assert_eq!(lane_tier(6 + NARROW_LANES, 6), LaneTier::Overflow);
    }

    #[test]
    fn the_pane_s_spare_width_goes_to_full_lanes_never_the_text_s() {
        assert_eq!(full_lanes_for(460., 3), MIN_FULL_LANES);
        assert_eq!(full_lanes_for(460., 10), 10);
        // Twenty lanes in the default pane: as many full lanes as leave the
        // text its reserve once the rest pack into the narrow band.
        let full = full_lanes_for(460., 20);
        assert_eq!(full, 9);
        assert!(gutter_width(20, full) <= 460. - TEXT_RESERVE);
        assert!(gutter_width(20, full + 1) > 460. - TEXT_RESERVE);
        assert_eq!(full_lanes_for(340., 20), MIN_FULL_LANES);
        assert_eq!(full_lanes_for(1200., 40), MAX_FULL_LANES);
    }
}
