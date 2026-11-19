//! The graph pane: the lens picker, the view toggle, and the rows — one
//! continuous jj-native tree, or one workstream's lane at a time.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::{
    canvas, div, fill, point, prelude::*, px, quad, Anchor, AnyElement, App, BorderStyle, Bounds,
    Context, Div, FontWeight, Hsla, MouseButton, PathBuilder, Pixels, Role, Stateful, TextAlign,
    TextRun, Window,
};
use jiji_core::snapshot::{GraphNode, NodeKind, RepoSnapshot, SnapshotView, WorkstreamSummary};
use jiji_forge::RepoPrState;

use super::motion::{self, Motion};
use super::{ShownDiff, WorkbenchView};
use crate::app::WorkbenchMode;
use crate::logic::graph::{
    emphasized_stream, full_lanes_for, gutter_width, lane_of, lane_tier, rail_x,
    resolve_focused_workstream, row_bookmarks, row_gutter_width, rows_in_view, sync_glyph,
    ElisionRow, GraphModel, GraphRow, LaneTier, NodeRow, Rail, RowDensity, ELISION_ROW_HEIGHT,
};
use crate::logic::inspect::summary_of;
use crate::logic::pr::{pr_badge, pr_for_bookmark, PrTone};
use crate::logic::text::counted;
use crate::store::SavedView;
use crate::theme::{theme, Theme, RADIUS_M, RADIUS_S, TEXT_L, TEXT_M, TEXT_S, TEXT_XS, TOOLBAR_H};
use crate::ui::motion::ease_out;
use crate::ui::ScrollY as _;
use crate::ui::{
    absolute_time, btn, empty_state, h_flex, icon, icon_btn, input::field, keyboard_on, label_box,
    lead_bar, lead_bar_in, menu_label, popover, popover_layer, ref_label, ring, scrollbar,
    segmented, short_age, tag, tooltip, truncated, v_flex, IconName, NodeExt as _, Segment,
    Tabbable as _, Tabular as _,
};
use crate::views::menu;

/// The fixed presets, picker order. Custom is not listed — it is the
/// picker's revset input, not a row.
pub const VIEW_PRESETS: [(SnapshotView, &str, &str); 6] = [
    (
        SnapshotView::Workstreams,
        "Workstreams",
        "Your mutable work on the bases it sits on",
    ),
    (
        SnapshotView::Team,
        "Team",
        "Yours plus everyone's branches, local and remote",
    ),
    (
        SnapshotView::Recent,
        "Recent",
        "All activity from the last two weeks",
    ),
    (
        SnapshotView::Mine,
        "Mine",
        "Everything you authored, including landed work",
    ),
    (
        SnapshotView::Trunk,
        "Trunk history",
        "What landed, newest first",
    ),
    (
        SnapshotView::All,
        "All history",
        "The entire visible history",
    ),
];

/// The picker/trigger label for a lens, including custom.
pub fn view_label(view: &SnapshotView) -> &'static str {
    VIEW_PRESETS
        .iter()
        .find(|(preset, ..)| preset == view)
        .map_or("Custom revset", |(_, label, _)| label)
}

/// Saves a view. A new name appends (list order is save order); saving an
/// existing name replaces that view's revset in place. Refuses when either
/// field trims to nothing.
pub fn upsert_saved_view(views: &[SavedView], name: &str, revset: &str) -> Option<Vec<SavedView>> {
    let (name, revset) = (name.trim(), revset.trim());
    if name.is_empty() || revset.is_empty() {
        return None;
    }
    let saved = SavedView {
        name: name.to_owned(),
        revset: revset.to_owned(),
    };
    let mut next = views.to_vec();
    match next.iter_mut().find(|view| view.name == name) {
        Some(existing) => *existing = saved,
        None => next.push(saved),
    }
    Some(next)
}

/// Renames a view. Refuses when the target is gone, the new name trims to
/// nothing, or another view already owns the new name.
pub fn rename_saved_view(
    views: &[SavedView],
    name: &str,
    new_name: &str,
) -> Option<Vec<SavedView>> {
    let new_name = new_name.trim();
    if new_name.is_empty() || !views.iter().any(|view| view.name == name) {
        return None;
    }
    if new_name != name && views.iter().any(|view| view.name == new_name) {
        return None;
    }
    Some(
        views
            .iter()
            .map(|view| SavedView {
                name: if view.name == name {
                    new_name.to_owned()
                } else {
                    view.name.clone()
                },
                revset: view.revset.clone(),
            })
            .collect(),
    )
}

/// Deletes a view by name; the rest keep their order.
pub fn delete_saved_view(views: &[SavedView], name: &str) -> Vec<SavedView> {
    views
        .iter()
        .filter(|view| view.name != name)
        .cloned()
        .collect()
}

/// Panes this wide or narrower leave authors out of the rows.
const AUTHOR_MIN_PANE: f32 = 560.;
/// A full-spacing rail's stroke: lines on a transit map, not hairlines.
const RAIL_W: f32 = 3.;
/// A station's ring.
const RING_W: f32 = 2.5;
/// How much of its hue a line keeps while another line has the focus.
const CALM: f32 = 0.8;
/// Between a row's parts, and after its last.
const ROW_GAP: f32 = 8.;
const ROW_PAD: f32 = 12.;
/// What a title keeps however many labels its row carries.
const TITLE_MIN: f32 = 96.;
/// A ref's name truncates past this, and is not worth showing under
/// `REF_MIN`.
const REF_MAX: f32 = 120.;
const REF_MIN: f32 = 44.;
/// A label's padding and hairlines.
const LABEL_PAD: f32 = 12.;

/// Rows a rewrite took away keep their own element ids while they fold,
/// apart from the rows drawn in their place.
const GONE_KEYS: usize = 1 << 20;

/// How wide `chars` monospace characters run at `size`.
fn mono_width_at(chars: f32, size: f32) -> f32 {
    chars * size * 0.61
}

/// How wide `text` runs in a label's monospace.
fn mono_width(text: &str) -> f32 {
    mono_width_at(text.chars().count() as f32, TEXT_XS)
}

/// The labels riding one graph row, fitted to the room the title spares.
/// A ref that does not fit whole truncates into what is left; one that
/// cannot is counted in a closing "+N" that names it.
struct RowLabels {
    room: f32,
    used: f32,
    /// Refs still to be offered: while any remain, the count keeps its
    /// seat.
    refs_left: usize,
    shown: Vec<AnyElement>,
    hidden: Vec<String>,
}

impl RowLabels {
    /// Room the closing count needs.
    const MORE: f32 = 24.;
    const GAP: f32 = 4.;

    fn new(room: f32, refs: usize) -> Self {
        Self {
            room,
            used: 0.,
            refs_left: refs,
            shown: Vec::new(),
            hidden: Vec::new(),
        }
    }

    fn left(&self) -> f32 {
        let seat = if self.refs_left > 0 || !self.hidden.is_empty() {
            Self::MORE
        } else {
            0.
        };
        self.room - self.used - seat
    }

    /// Room for a ref that wants `natural` and reads down to `least`:
    /// `None` once it (or one before it) had to be counted instead.
    fn grant(&mut self, natural: f32, least: f32, name: &str) -> Option<f32> {
        self.refs_left = self.refs_left.saturating_sub(1);
        let left = self.left();
        if !self.hidden.is_empty() || left < least.min(natural) {
            self.hidden.push(name.to_owned());
            return None;
        }
        let granted = natural.min(left);
        self.used += granted + Self::GAP;
        Some(granted)
    }

    /// A label that may leave without a trace: a state the node marker or
    /// the row already carries.
    fn fits(&mut self, width: f32) -> bool {
        let fits = self.hidden.is_empty() && width <= self.left();
        if fits {
            self.used += width + Self::GAP;
        }
        fits
    }

    fn add(&mut self, label: impl IntoElement) {
        self.shown.push(label.into_any_element());
    }

    fn into_row(self, id: impl Into<gpui::ElementId>, t: &Theme) -> Div {
        let mut row = h_flex()
            .flex_shrink_0()
            .gap(px(Self::GAP))
            .children(self.shown);
        if !self.hidden.is_empty() {
            row = row.child(
                div()
                    .id(id)
                    .flex_shrink_0()
                    .font_family(t.mono.clone())
                    .text_size(px(TEXT_XS))
                    .text_color(t.text3)
                    .tooltip(tooltip(self.hidden.join(", ")))
                    .child(format!("+{}", self.hidden.len())),
            );
        }
        row
    }
}

/// Everything a row needs to draw itself, shared by a whole pass.
struct RowStyle<'a> {
    snapshot: &'a RepoSnapshot,
    theme: Rc<Theme>,
    /// Workstream rendered hot; other streams render calm.
    emphasized: Option<String>,
    selected: &'a HashSet<String>,
    full: usize,
    gutter: f32,
    density: RowDensity,
    prs: Option<std::sync::Arc<RepoPrState>>,
    /// The comparison on the diff surface, when one is.
    comparison: Option<&'a ShownDiff>,
    /// Whether the pane has room for authors beside the descriptions.
    authors: bool,
    /// The width the rows lay out in.
    width: f32,
    /// The row the keyboard travels from (the selection's anchor), and
    /// whether the pane has the keys — from the keyboard, for the ring.
    anchor: Option<String>,
    keys: GraphKeys,
}

/// Whether the graph pane has focus, and whether the keyboard gave it.
#[derive(Clone, Copy)]
pub(super) struct GraphKeys {
    focused: bool,
    ringed: bool,
}

impl RowStyle<'_> {
    fn rail_color(&self, rail: &Rail, hot_lane: Option<usize>) -> Hsla {
        let t = &self.theme;
        match rail.lane {
            None => Theme::blend(t.text3, 0.55, t.bg1),
            Some(lane) if Some(lane) == hot_lane => t.lane(lane),
            Some(lane) => Theme::blend(t.lane(lane), CALM, t.bg1),
        }
    }

    /// How much of a row its labels may take. The title keeps what it
    /// needs, up to half the row; ids, author and age keep theirs.
    fn label_room(&self, node: &GraphNode, title: &str, gutter: f32) -> f32 {
        let classic = self.density == RowDensity::Classic;
        let mut text = self.width - gutter - ROW_GAP - ROW_PAD;
        if !classic {
            let ids = if node.is_divergent { 19. } else { 8. };
            let age = 30. + if self.authors { 82. } else { 0. };
            text -= mono_width_at(ids, TEXT_S) + age + 3. * ROW_GAP;
        }
        let size = if classic { TEXT_L } else { TEXT_M };
        let wanted = if title.is_empty() {
            120.
        } else {
            title.chars().count() as f32 * size * 0.54
        };
        let title_room = wanted.min((text * 0.5).max(TITLE_MIN));
        (text - title_room).max(0.)
    }

    /// The lane slot of the emphasized workstream.
    fn hot_lane(&self) -> Option<usize> {
        self.emphasized
            .as_deref()
            .and_then(|stream| lane_of(self.snapshot, stream))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum RailRole {
    Pass,
    In,
    Out,
}

#[derive(Clone, Copy)]
enum Marker {
    WorkingCopy,
    Conflict,
    Base,
    /// A mutable change: ring color, and whether it fills in (selected).
    Change {
        ring: Hsla,
        fill: Option<Hsla>,
    },
}

fn stroke(
    window: &mut Window,
    color: Hsla,
    width: f32,
    dashed: bool,
    points: impl FnOnce(&mut PathBuilder),
) {
    let mut builder = PathBuilder::stroke(px(width));
    if dashed {
        builder = builder.dash_array(&[px(2.), px(3.)]);
    }
    points(&mut builder);
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

/// A rail segment that runs straight down: a crisp quad, not a path.
fn vertical(window: &mut Window, x: Pixels, y0: Pixels, y1: Pixels, width: f32, color: Hsla) {
    if y1 > y0 {
        window.paint_quad(fill(
            Bounds::from_corners(point(x - px(width / 2.), y0), point(x + px(width / 2.), y1)),
            color,
        ));
    }
}

struct GutterPaint {
    rails: Vec<(RailRole, Rail, Hsla)>,
    column: usize,
    full: usize,
    marker: Marker,
    /// Immutable bases keep history below them; a dashed stub hands the
    /// rail off to the elision row underneath.
    stub_below: Option<Hsla>,
    radius: f32,
    clear: f32,
    narrow: bool,
    classic: bool,
    knockout: Option<Hsla>,
    working_copy: Hsla,
    danger: Hsla,
    base_fill: Hsla,
}

impl GutterPaint {
    fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window, _cx: &mut App) {
        let (left, top, bottom) = (bounds.left(), bounds.top(), bounds.bottom());
        let height: f32 = bounds.size.height.into();
        let cy = top + px(height / 2.);
        let nx = left + px(rail_x(self.column, self.full));
        let clear = px(self.clear);

        for (role, rail, color) in &self.rails {
            let tier = lane_tier(rail.col, self.full);
            let width = if tier == LaneTier::Full { RAIL_W } else { 1. };
            let color = if tier == LaneTier::Overflow {
                Theme::fade(*color, 0.55)
            } else {
                *color
            };
            let x = left + px(rail_x(rail.col, self.full));
            let dx: f32 = (nx - x).into();
            let straight = dx.abs() < 0.5;
            match role {
                RailRole::Pass => vertical(window, x, top, bottom, width, color),
                RailRole::In if straight => vertical(window, x, top, cy - clear, width, color),
                RailRole::Out if straight && !rail.elided => {
                    vertical(window, x, cy + clear, bottom, width, color)
                }
                RailRole::Out if straight => {
                    stroke(window, color, width, true, |path| {
                        path.move_to(point(x, cy + clear));
                        path.line_to(point(x, bottom));
                    });
                }
                RailRole::In => {
                    // Off its own track into the station from above: an
                    // S-bend through the band over the node, the way a
                    // transit line changes tracks.
                    let end = cy - clear;
                    let mid = top + (end - top) / 2.;
                    stroke(window, color, width, rail.elided, |path| {
                        path.move_to(point(x, top));
                        path.cubic_bezier_to(point(nx, end), point(x, mid), point(nx, mid));
                    });
                }
                RailRole::Out => {
                    let start = cy + clear;
                    let mid = start + (bottom - start) / 2.;
                    stroke(window, color, width, rail.elided, |path| {
                        path.move_to(point(nx, start));
                        path.cubic_bezier_to(point(x, bottom), point(nx, mid), point(x, mid));
                    });
                }
            }
        }
        if let Some(color) = self.stub_below {
            let width = if self.narrow { 1. } else { RAIL_W };
            stroke(window, color, width, true, |path| {
                path.move_to(point(nx, cy + clear));
                path.line_to(point(nx, bottom));
            });
        }

        let r = px(self.radius);
        let disc = |radius: Pixels| {
            Bounds::from_corners(
                point(nx - radius, cy - radius),
                point(nx + radius, cy + radius),
            )
        };
        // Past the narrow band's end a node shares its slot with whatever
        // else overflowed: knock it out from the rails so it never reads as
        // sitting on a neighbor's line.
        if let Some(fill_color) = self.knockout {
            let radius = r + px(2.);
            window.paint_quad(fill(disc(radius), fill_color).corner_radii(radius));
        }
        // Node markers, jj-native: @ working copy, ○ mutable, ◆ immutable,
        // × conflict.
        match self.marker {
            Marker::WorkingCopy => {
                // You are here: a ring around a dot, the one marker of its
                // kind on the map.
                let (outer, ring, inner) = if self.narrow {
                    (4., 1.4, 1.8)
                } else if self.classic {
                    (7.5, 2., 3.5)
                } else {
                    (6.5, 2., 3.)
                };
                window.paint_quad(quad(
                    disc(px(outer)),
                    px(outer),
                    gpui::transparent_black(),
                    px(ring),
                    self.working_copy,
                    BorderStyle::Solid,
                ));
                window.paint_quad(fill(disc(px(inner)), self.working_copy).corner_radii(px(inner)));
            }
            Marker::Conflict => {
                // A closed station: a red disc with the cross cut from it.
                let rr = r + px(if self.narrow { 0.5 } else { 1.5 });
                window.paint_quad(fill(disc(rr), self.danger).corner_radii(rr));
                let xr = rr * 0.42;
                let width = if self.narrow { 1.1 } else { 1.7 };
                let ink = gpui::white();
                stroke(window, ink, width, false, |path| {
                    path.move_to(point(nx - xr, cy - xr));
                    path.line_to(point(nx + xr, cy + xr));
                });
                stroke(window, ink, width, false, |path| {
                    path.move_to(point(nx + xr, cy - xr));
                    path.line_to(point(nx - xr, cy + xr));
                });
            }
            Marker::Base => {
                let d = r * 1.3;
                let mut builder = PathBuilder::fill();
                builder.move_to(point(nx, cy - d));
                builder.line_to(point(nx + d, cy));
                builder.line_to(point(nx, cy + d));
                builder.line_to(point(nx - d, cy));
                builder.close();
                if let Ok(path) = builder.build() {
                    window.paint_path(path, self.base_fill);
                }
            }
            Marker::Change { ring, fill: inside } => {
                let width = if self.narrow { 1.2 } else { RING_W };
                // The selected node fills in: a solid dot reads as "this
                // one" at any lane color, without a halo.
                window.paint_quad(quad(
                    disc(r + px(width / 2.)),
                    r + px(width / 2.),
                    inside.unwrap_or(gpui::transparent_black()),
                    px(width),
                    ring,
                    BorderStyle::Solid,
                ));
            }
        }
    }
}

/// What a graph row says to assistive technology: its title and change id,
/// then what sets it apart, then its bookmarks — what a sighted reader
/// takes from the row at a glance.
fn row_name(node: &GraphNode, title: &str, working_copy: bool, immutable: bool) -> String {
    let mut name = format!(
        "{}, {}",
        if title.is_empty() {
            "No description yet"
        } else {
            title
        },
        node.id.chars().take(8).collect::<String>()
    );
    let marks = [
        (working_copy, "working copy"),
        (node.has_conflict, "conflicted"),
        (node.is_divergent, "divergent"),
        (immutable, "immutable"),
    ];
    for word in marks.iter().filter(|(on, _)| *on).map(|(_, word)| *word) {
        name.push_str(", ");
        name.push_str(word);
    }
    for bookmark in &node.bookmarks {
        name.push_str(", ");
        name.push_str(bookmark);
    }
    name
}

fn gutter_canvas(paint: GutterPaint, width: f32, height: f32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, cx| paint.paint(bounds, window, cx),
    )
    .w(px(width))
    .h(px(height))
    .flex_shrink_0()
}

impl WorkbenchView {
    // ── Pane head ──

    pub(super) fn render_graph_pane(
        &mut self,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let mode = self.mode;
        let classic = self.density == RowDensity::Classic;
        let keys = GraphKeys {
            focused: self.graph_focus.is_focused(window),
            ringed: keyboard_on(&self.graph_focus, window),
        };
        let head = h_flex()
            .h(px(TOOLBAR_H))
            .flex_shrink_0()
            .gap(px(8.))
            .pl(px(8.))
            .pr(px(8.))
            .border_b_1()
            .border_color(t.border2)
            .child(self.render_view_menu(window, cx))
            .child(div().flex_1())
            .child(segmented(
                "workbench-mode",
                "Workbench view",
                vec![
                    Segment::text(WorkbenchMode::Graph, "Graph"),
                    Segment::text(WorkbenchMode::Focus, "Focus"),
                ],
                &mode,
                cx,
                {
                    let this = cx.entity().downgrade();
                    move |mode, window, cx| {
                        let mode = *mode;
                        let _ = this.update(cx, |this, cx| this.set_mode(mode, window, cx));
                    }
                },
            ))
            .child(
                icon_btn(
                    "density",
                    if classic {
                        IconName::RowsClassic
                    } else {
                        IconName::RowsCompact
                    },
                )
                .size(26., 14.)
                .on(classic)
                .label("Classic rows")
                .pressed(classic)
                .tooltip(if classic {
                    "Classic rows — switch to compact, jj-dense rows"
                } else {
                    "Compact rows — switch to larger, classic rows"
                })
                .on_click(cx.listener(|this, _, _, cx| this.toggle_density(cx))),
            );

        let body = match mode {
            WorkbenchMode::Graph => self
                .render_graph(width, keys, window, cx)
                .into_any_element(),
            WorkbenchMode::Focus => self.render_focus(width, keys, cx).into_any_element(),
        };
        v_flex().size_full().child(head).child(
            div()
                .id("graph-pane")
                .role(Role::ListBox)
                .aria_label("Change graph")
                .a11y(|node| node.set_multiselectable())
                .key_context("GraphPane")
                .track_focus(&self.graph_focus)
                .relative()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .id("graph-scroller")
                        .size_full()
                        .scroll_y()
                        .track_scroll(&self.scroll)
                        .on_scroll(cx.listener(|_, _, _, cx| cx.notify()))
                        .child(body),
                )
                .child(scrollbar("graph-scrollbar", &self.scroll)),
        )
    }

    // ── The lens picker ──

    /// The pane's title: the lens it shows (a custom lens wears its saved
    /// name when it has one), and on the default lens how many
    /// workstreams it holds.
    fn trigger_label(&self) -> (String, Option<usize>) {
        match &self.snapshot.view {
            SnapshotView::Workstreams => {
                ("Workstreams".into(), Some(self.snapshot.workstreams.len()))
            }
            SnapshotView::Custom { revset } => (
                self.view_menu
                    .saved
                    .iter()
                    .find(|view| &view.revset == revset)
                    .map_or_else(|| "Custom revset".to_owned(), |view| view.name.clone()),
                None,
            ),
            other => (view_label(other).to_owned(), None),
        }
    }

    fn toggle_view_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.view_menu.popover.toggle(window, cx);
        if self.view_menu.popover.is_open() {
            self.view_menu.error = None;
            self.view_menu.saving = false;
            self.view_menu.renaming = None;
            let revset = match &self.snapshot.view {
                SnapshotView::Custom { revset } => revset.clone(),
                _ => String::new(),
            };
            self.view_menu
                .revset
                .update(cx, |input, cx| input.set_text(revset, cx));
        }
        cx.notify();
    }

    fn apply_view(&mut self, view: SnapshotView, window: &mut Window, cx: &mut Context<Self>) {
        if self.view_menu.busy {
            return;
        }
        self.view_menu.busy = true;
        self.view_menu.error = None;
        cx.notify();
        let task = self
            .app
            .update(cx, |model, cx| model.set_snapshot_view(view, true, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.view_menu.busy = false;
                match result {
                    Ok(()) => this.view_menu.popover.close(window, cx),
                    // The backend's refusal, inline: the graph keeps
                    // rendering what it was.
                    Err(error) => this.view_menu.error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn apply_revset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let revset = self.view_menu.revset.read(cx).text().trim().to_owned();
        if !revset.is_empty() {
            self.apply_view(SnapshotView::Custom { revset }, window, cx);
        }
    }

    fn persist_views(&mut self, views: Vec<SavedView>, cx: &mut Context<Self>) {
        let path = self.snapshot.repo_path.clone();
        self.app
            .update(cx, |model, _| model.store.set_saved_views(&path, &views));
        self.view_menu.saved = views;
        cx.notify();
    }

    pub(super) fn commit_save_view(&mut self, cx: &mut Context<Self>) {
        let name = self.view_menu.name.read(cx).text().to_owned();
        let revset = self.view_menu.revset.read(cx).text().to_owned();
        let Some(next) = upsert_saved_view(&self.view_menu.saved, &name, &revset) else {
            return;
        };
        self.view_menu.error = None;
        self.view_menu.saving = false;
        self.view_menu
            .name
            .update(cx, |input, cx| input.set_text("", cx));
        self.persist_views(next, cx);
    }

    pub(super) fn commit_rename_view(&mut self, cx: &mut Context<Self>) {
        let Some(current) = self.view_menu.renaming.clone() else {
            return;
        };
        let new_name = self.view_menu.rename.read(cx).text().trim().to_owned();
        if new_name.is_empty() {
            return;
        }
        match rename_saved_view(&self.view_menu.saved, &current, &new_name) {
            Some(next) => {
                self.view_menu.error = None;
                self.view_menu.renaming = None;
                self.persist_views(next, cx);
            }
            None => {
                self.view_menu.error = Some(format!(
                    "A saved view named \u{201c}{new_name}\u{201d} already exists."
                ));
                cx.notify();
            }
        }
    }

    fn render_view_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let open = self.view_menu.popover.is_open();
        let lensed = self.snapshot.view != SnapshotView::Workstreams;
        let (label, count) = self.trigger_label();
        let mut trigger = h_flex()
            .id("view-menu")
            .role(Role::Button)
            .aria_label(format!("Graph view: {label}"))
            .aria_description("Change what the graph shows")
            .aria_expanded(open)
            .tabbable(cx)
            .gap(px(6.))
            .h(px(26.))
            .px(px(8.))
            .min_w_0()
            .rounded(px(RADIUS_M))
            .text_size(px(TEXT_M))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(if lensed { t.accent_strong } else { t.text1 })
            .cursor_pointer()
            .hover(|style| style.bg(t.bg_hover))
            .tooltip(tooltip("Change what the graph shows"))
            .on_click(cx.listener(|this, _, window, cx| this.toggle_view_menu(window, cx)))
            .child(div().min_w_0().child(truncated(label)))
            .children(count.map(|count| {
                div()
                    .flex_shrink_0()
                    .font_weight(FontWeight::NORMAL)
                    .text_size(px(TEXT_S))
                    .text_color(t.text3)
                    .tabular()
                    .child(count.to_string())
            }))
            .child(icon(IconName::ChevronDown, 12., t.text3));
        if open {
            trigger = trigger.bg(t.bg_hover);
        }
        let anchor = div().relative().min_w_0().child(trigger);
        if !open {
            return anchor;
        }

        let busy = self.view_menu.busy;
        let current = self.snapshot.view.clone();
        // One lens among several: a radio button, ticked when on screen.
        let preset_row =
            |id: gpui::ElementId, label: String, desc: String, mono: bool, on: bool| {
                let named = h_flex()
                    .id(id)
                    .role(Role::RadioButton)
                    .aria_label(label.clone())
                    .aria_description(desc.clone())
                    .aria_toggled(if on {
                        gpui::Toggled::True
                    } else {
                        gpui::Toggled::False
                    });
                let mut text = v_flex().flex_1().min_w_0().child(
                    div()
                        .truncate()
                        .text_size(px(TEXT_S))
                        .font_weight(FontWeight::MEDIUM)
                        .child(label),
                );
                let mut sub = div()
                    .truncate()
                    .text_size(px(TEXT_XS))
                    .text_color(t.text3)
                    .child(desc);
                if mono {
                    sub = sub.font_family(t.mono.clone());
                }
                text = text.child(sub);
                named
                    .when(!busy, |el| el.tabbable_in(t.accent))
                    .flex_1()
                    .min_w_0()
                    .gap(px(8.))
                    .px(px(8.))
                    .py(px(5.))
                    .rounded(px(RADIUS_S))
                    .when(!busy, |el| {
                        el.cursor_pointer().hover(|style| style.bg(t.bg_hover))
                    })
                    .when(busy, |el| el.opacity(0.6))
                    .child(text)
                    .child(div().w(px(14.)).flex_shrink_0().when(on, |el| {
                        el.child(icon(IconName::Check, 13., t.accent_strong))
                    }))
            };

        let mut panel = popover(cx)
            .w(px(320.))
            .p(px(4.))
            .flex()
            .flex_col()
            .child(menu_label("Show", cx));
        let mut lenses = v_flex()
            .id("lenses")
            .role(Role::RadioGroup)
            .aria_label("Show");
        for (ix, (view, label, desc)) in VIEW_PRESETS.iter().enumerate() {
            let target = view.clone();
            lenses = lenses.child(
                preset_row(
                    ("lens", ix).into(),
                    (*label).to_owned(),
                    (*desc).to_owned(),
                    false,
                    *view == current,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.apply_view(target.clone(), window, cx)
                })),
            );
        }
        panel = panel.child(lenses);

        let saved = self.view_menu.saved.clone();
        if !saved.is_empty() {
            panel = panel.child(menu_label("Saved", cx));
            for (ix, view) in saved.iter().enumerate() {
                if self.view_menu.renaming.as_deref() == Some(view.name.as_str()) {
                    let empty = self.view_menu.rename.read(cx).text().trim().is_empty();
                    panel = panel.child(
                        h_flex()
                            .gap(px(6.))
                            .px(px(8.))
                            .py(px(3.))
                            .child(field(&self.view_menu.rename, window, cx).flex_1())
                            .child(btn("rename-view", "Rename").disabled(empty).on_click(
                                cx.listener(|this, _, _, cx| this.commit_rename_view(cx)),
                            )),
                    );
                    continue;
                }
                let as_view = SnapshotView::Custom {
                    revset: view.revset.clone(),
                };
                let target = as_view.clone();
                let rename_name = view.name.clone();
                let delete_name = view.name.clone();
                panel = panel.child(
                    h_flex()
                        .gap(px(2.))
                        .child(
                            preset_row(
                                ("saved", ix).into(),
                                view.name.clone(),
                                view.revset.clone(),
                                true,
                                as_view == current,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.apply_view(target.clone(), window, cx)
                                },
                            )),
                        )
                        .child(
                            icon_btn(("rename-saved", ix), IconName::Edit)
                                .size(22., 12.)
                                .label(format!("Rename view {}", view.name))
                                .tooltip("Rename view")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.view_menu.error = None;
                                    this.view_menu.renaming = Some(rename_name.clone());
                                    let name = rename_name.clone();
                                    this.view_menu.rename.update(cx, |input, cx| {
                                        input.set_text(name, cx);
                                        input.focus(window, cx);
                                    });
                                    cx.notify();
                                })),
                        )
                        .child(
                            icon_btn(("delete-saved", ix), IconName::Trash)
                                .size(22., 12.)
                                .label(format!("Delete view {}", view.name))
                                .tooltip("Delete view")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this.view_menu.renaming.as_deref() == Some(&delete_name) {
                                        this.view_menu.renaming = None;
                                    }
                                    let next =
                                        delete_saved_view(&this.view_menu.saved, &delete_name);
                                    this.persist_views(next, cx);
                                })),
                        ),
                );
            }
        }

        let typed = !self.view_menu.revset.read(cx).text().trim().is_empty();
        panel = panel.child(menu_label("Revset", cx)).child(
            h_flex()
                .gap(px(6.))
                .px(px(8.))
                .pb(px(6.))
                .child(
                    field(&self.view_menu.revset, window, cx)
                        .flex_1()
                        .font_family(t.mono.clone()),
                )
                .child(
                    btn("apply-revset", "Show")
                        .disabled(busy || !typed)
                        .on_click(cx.listener(|this, _, window, cx| this.apply_revset(window, cx))),
                ),
        );
        if self.view_menu.saving {
            let nameless = self.view_menu.name.read(cx).text().trim().is_empty();
            panel = panel.child(
                h_flex()
                    .gap(px(6.))
                    .px(px(8.))
                    .pb(px(6.))
                    .child(field(&self.view_menu.name, window, cx).flex_1())
                    .child(
                        btn("save-view", "Save")
                            .disabled(nameless || !typed)
                            .on_click(cx.listener(|this, _, _, cx| this.commit_save_view(cx))),
                    ),
            );
        } else if typed {
            panel = panel.child(
                div()
                    .id("save-as-view")
                    .role(Role::Button)
                    .aria_label("Save as view")
                    .tabbable(cx)
                    .px(px(8.))
                    .pb(px(6.))
                    .text_size(px(TEXT_S))
                    .text_color(t.accent_strong)
                    .cursor_pointer()
                    .hover(|style| style.text_color(t.text1))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.view_menu.saving = true;
                        this.view_menu
                            .name
                            .update(cx, |input, cx| input.focus(window, cx));
                        cx.notify();
                    }))
                    .child("Save as view…"),
            );
        }
        if let Some(error) = &self.view_menu.error {
            panel = panel.child(
                div()
                    .id("view-menu-error")
                    .alert(error.clone())
                    .px(px(8.))
                    .pb(px(6.))
                    .text_size(px(TEXT_XS))
                    .line_height(gpui::relative(1.5))
                    .text_color(t.danger)
                    .child(error.clone()),
            );
        }
        anchor.child(popover_layer(
            Anchor::TopLeft,
            28.,
            self.view_menu
                .popover
                .panel("view-menu-panel", Role::Dialog, "Graph view", panel),
            cx.listener(|this, _, window, cx| {
                this.view_menu.popover.close(window, cx);
                cx.notify();
            }),
        ))
    }

    // ── Rows ──

    #[allow(clippy::too_many_arguments)]
    fn row_style<'a>(
        &'a self,
        snapshot: &'a RepoSnapshot,
        model: &GraphModel,
        width: f32,
        selected: &'a HashSet<String>,
        emphasized: Option<String>,
        keys: GraphKeys,
        cx: &App,
    ) -> RowStyle<'a> {
        let full = full_lanes_for(width, model.column_count);
        RowStyle {
            snapshot,
            theme: theme(cx),
            emphasized,
            selected,
            full,
            gutter: gutter_width(model.column_count, full),
            density: self.density,
            prs: self.app.read(cx).forge.prs.clone(),
            comparison: self.shown.as_ref().filter(|shown| shown.from.is_some()),
            authors: width > AUTHOR_MIN_PANE,
            width,
            anchor: self.app.read(cx).ui().selected_node.clone(),
            keys,
        }
    }

    fn node_row(
        &self,
        row: &NodeRow,
        key: usize,
        style: &RowStyle,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let t = &style.theme;
        let node = &style.snapshot.nodes[row.node];
        let is_base = node.kind == NodeKind::Immutable;
        let hot_lane = style.hot_lane();
        let hot = !is_base && row.stream.is_some() && row.stream == style.emphasized;
        let selected = style.selected.contains(&node.id);
        let classic = style.density == RowDensity::Classic;
        let height = crate::logic::graph::node_row_height(style.density);

        // Lanes past the pane's full-spacing budget draw in the narrow
        // band: hairline rails, small markers. A node there still gets its
        // whole text row — only its marker is compact.
        let tier = lane_tier(row.column, style.full);
        let narrow = tier != LaneTier::Full;
        let radius = match (narrow, classic) {
            (true, true) => 3.,
            (true, false) => 2.5,
            (false, true) => 5.5,
            (false, false) => 4.75,
        };
        // Rails run up to a marker's edge and stop there: the line meets
        // the station, never shows through its ring.
        let clear = if narrow {
            radius + 2.
        } else if row.is_working_copy {
            if classic {
                7.5
            } else {
                6.5
            }
        } else {
            radius + RING_W / 2. - 0.5
        };
        let lane_color = row.lane.map_or(t.accent, |lane| t.lane(lane));
        let marker = if row.is_working_copy {
            Marker::WorkingCopy
        } else if node.has_conflict {
            Marker::Conflict
        } else if is_base {
            Marker::Base
        } else {
            Marker::Change {
                // A station wears its line's color: full on the line in
                // focus, the line's own calm tone elsewhere.
                ring: if hot || selected {
                    lane_color
                } else {
                    Theme::blend(lane_color, CALM, t.bg1)
                },
                fill: selected.then_some(lane_color),
            }
        };
        let mut rails =
            Vec::with_capacity(row.pass_through.len() + row.edges_in.len() + row.edges_out.len());
        for (role, list) in [
            (RailRole::Pass, &row.pass_through),
            (RailRole::In, &row.edges_in),
            (RailRole::Out, &row.edges_out),
        ] {
            rails.extend(
                list.iter()
                    .map(|rail| (role, *rail, style.rail_color(rail, hot_lane))),
            );
        }
        let base_rail = Theme::blend(t.text3, 0.5, t.bg1);
        // The row's own rails set its gutter: text hugs the tree.
        let gutter_w = row_gutter_width(row.last_column(), style.full);
        let gutter = gutter_canvas(
            GutterPaint {
                rails,
                column: row.column,
                full: style.full,
                marker,
                stub_below: (is_base && row.edges_out.is_empty()).then_some(base_rail),
                radius,
                clear,
                narrow,
                classic,
                knockout: (tier == LaneTier::Overflow).then_some(t.bg1),
                working_copy: t.working_copy,
                danger: t.danger,
                base_fill: Theme::blend(t.text3, 0.8, t.bg1),
            },
            gutter_w,
            height,
        );

        // jj's change id: the shortest unique prefix in full ink, the rest
        // receding — the part you would actually type stands out.
        let short = |text: &str, from: usize, to: usize| -> String {
            text.chars().skip(from).take(to - from).collect()
        };
        let ids: AnyElement = if node.is_divergent {
            // jj's ?? state: the change id names several commits, so it
            // renders alarmed and the commit id becomes the row's real
            // identity.
            h_flex()
                .id(("divergent", key))
                .flex_shrink_0()
                .gap(px(8.))
                .font_family(t.mono.clone())
                .text_size(px(TEXT_S))
                .tooltip(tooltip(format!(
                    "Divergent change: several visible commits share {} — this copy is commit {}",
                    node.change_id, node.commit_id
                )))
                .child(
                    h_flex()
                        .text_color(t.danger)
                        .child(short(&node.change_id, 0, 8))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("??")),
                )
                .child(
                    div()
                        .text_color(t.text3)
                        .child(short(&node.commit_id, 0, 8)),
                )
                .into_any_element()
        } else {
            // The part you would type wears the line's color, the way jj
            // colors it in a terminal; history keeps it neutral.
            let ink = if is_base {
                t.text2
            } else if hot || selected {
                lane_color
            } else {
                Theme::blend(lane_color, 0.78, t.text2)
            };
            h_flex()
                .flex_shrink_0()
                .font_family(t.mono.clone())
                .text_size(px(TEXT_S))
                .text_color(t.text3)
                .child(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .text_color(ink)
                        .child(short(&node.id, 0, 2)),
                )
                .child(short(&node.id, 2, 8))
                .into_any_element()
        };

        let title = summary_of(&node.description);
        let mut desc = div()
            .min_w(px(40.))
            .flex_shrink_1()
            .text_size(px(if classic { TEXT_L } else { TEXT_M }));
        desc = if title.is_empty() {
            desc.text_color(t.text3)
                .italic()
                .child(truncated("No description yet"))
        } else {
            desc.text_color(if is_base {
                t.text3
            } else if hot || selected {
                t.text1
            } else {
                t.text2
            })
            .when(classic && !is_base, |el| el.font_weight(FontWeight::MEDIUM))
            .child(truncated(title.to_owned()))
        };

        let is_compare_base = style
            .comparison
            .is_some_and(|shown| shown.from.as_deref() == Some(node.id.as_str()));

        // Labels ride the row only as far as the title can spare: whatever
        // does not fit is counted, never clipped.
        let bookmarks = row_bookmarks(style.snapshot, row);
        let mut labels = RowLabels::new(
            style.label_room(node, title, gutter_w),
            bookmarks.len() + node.tags.len(),
        );
        if is_compare_base && labels.fits(62.) {
            labels.add(
                tag("diff base", t.accent_strong)
                    .id(("diff-base", key))
                    .tooltip(tooltip(
                        "The diff pane measures the selection against this change",
                    )),
            );
        }
        for (bx, bookmark) in bookmarks.into_iter().enumerate() {
            let (glyph, tone, sync_label) = sync_glyph(bookmark.sync);
            let full_name = match &bookmark.remote {
                Some(remote) => format!("{}@{remote}", bookmark.name),
                None => bookmark.name.clone(),
            };
            // Remote-only refs label with their remote and render muted:
            // they are someone else's position, not a local bookmark.
            let shown = if bookmark.is_local {
                bookmark.name.clone()
            } else {
                full_name.clone()
            };
            let (ink, line) = if !bookmark.is_local {
                (t.text3, t.border2)
            } else if bookmark.is_trunk {
                // Trunk is the one ref the whole graph measures against.
                (t.accent_strong, Theme::fade(t.accent, 0.5))
            } else {
                (t.text2, t.border1)
            };
            let fixed = LABEL_PAD + if glyph.is_empty() { 0. } else { 10. };
            let natural = mono_width(&shown).min(REF_MAX) + fixed;
            let Some(granted) = labels.grant(natural, REF_MIN + fixed, &full_name) else {
                continue;
            };
            let mut label = label_box(ink, line)
                .id(("bm", key * 64 + bx))
                .gap(px(3.))
                .font_family(t.mono.clone())
                .tooltip(tooltip(format!("{full_name} — {sync_label}")))
                .on_mouse_down(
                    MouseButton::Right,
                    menu::bookmark_menu(bookmark.name.clone(), bookmark.remote.clone()),
                )
                .child(div().max_w(px(granted - fixed)).child(truncated(shown)));
            if !glyph.is_empty() {
                label = label.child(
                    div()
                        .text_size(px(10.))
                        .text_color(t.tone(tone))
                        .child(glyph),
                );
            }
            labels.add(label);
            if let Some(pr) = pr_for_bookmark(bookmark, style.prs.as_deref()) {
                // The bookmark's pull request, right beside its label.
                // Tones read like GitHub's states: open green, draft muted,
                // merged accent, closed danger.
                let badge = pr_badge(pr);
                let ink = match badge.tone {
                    PrTone::Open => t.ok,
                    PrTone::Draft => t.text3,
                    PrTone::Merged => t.accent_strong,
                    PrTone::Closed => t.danger,
                };
                let marks = [badge.review, badge.checks];
                let room = mono_width(&badge.label)
                    + LABEL_PAD
                    + 13. * marks.iter().flatten().count() as f32;
                if labels.fits(room) {
                    let mut el = tag(badge.label, ink)
                        .gap(px(3.))
                        .font_family(t.mono.clone())
                        .id(("pr", key * 64 + bx))
                        .on_mouse_down(MouseButton::Right, menu::pr_menu(pr.number))
                        .tooltip(tooltip(badge.title));
                    for glyph in marks.into_iter().flatten() {
                        el = el.child(
                            div()
                                .text_size(px(10.))
                                .text_color(t.tone(glyph.tone))
                                .child(glyph.glyph),
                        );
                    }
                    labels.add(el);
                }
            }
        }
        // The marker already says "conflict"; the word joins it when the
        // refs leave room.
        if node.has_conflict && labels.fits(54.) {
            labels.add(tag("conflict", t.danger));
        }
        // Colocated git tags, read-only: markers only, no tag management.
        for name in &node.tags {
            let fixed = LABEL_PAD + 13.;
            let natural = mono_width(name).min(REF_MAX) + fixed;
            if let Some(granted) = labels.grant(natural, REF_MIN + fixed, name) {
                labels.add(ref_label(Some(IconName::Tag), name.clone(), cx).max_w(px(granted)));
            }
        }
        if row.is_stack_head && row.behind_trunk > 0 {
            let text = format!("↓{}", row.behind_trunk);
            if labels.fits(mono_width(&text)) {
                labels.add(
                    div()
                        .id(("behind", key))
                        .flex_shrink_0()
                        .font_family(t.mono.clone())
                        .text_size(px(TEXT_XS))
                        .text_color(t.warn)
                        .tooltip(tooltip(format!(
                            "{} change{} behind trunk",
                            row.behind_trunk,
                            if row.behind_trunk == 1 { "" } else { "s" }
                        )))
                        .child(text),
                );
            }
        }
        if node.is_empty && labels.fits(34.) {
            labels.add(
                div()
                    .flex_shrink_0()
                    .text_size(px(TEXT_XS))
                    .text_color(t.text3)
                    .child("empty"),
            );
        }
        let tags = labels.into_row(("more-refs", key), t);

        // The description owns the row: long author names give way first,
        // and in a squeezed pane the author leaves for the tooltip.
        let meta = h_flex()
            .id(("meta", key))
            .flex_shrink_0()
            .gap(px(6.))
            .text_size(px(if classic { TEXT_S } else { TEXT_XS }))
            .text_color(t.text3)
            .tooltip(tooltip(format!(
                "{} · {}",
                node.author,
                absolute_time(&node.timestamp)
            )))
            .when(style.authors, |meta| {
                meta.child(div().max_w(px(76.)).child(truncated(node.author.clone())))
            })
            .child(
                div()
                    .min_w(px(24.))
                    .flex()
                    .justify_end()
                    .tabular()
                    .child(short_age(&node.timestamp)),
            );
        // What pushes the meta to the row's far edge. (A spacer, not an
        // auto margin: the layout engine drops a row's gaps when an auto
        // margin absorbs free space.)
        let spacer = || div().flex_1().min_w_0();

        let id = node.id.clone();
        let focus = self.graph_focus.clone();
        let anchor = style.anchor.as_deref() == Some(node.id.as_str());
        // Row states paint edge to edge behind the content; the tree's
        // rails run straight through them, like a table row, not a
        // picked-up card.
        let mut el = h_flex()
            .id(("node", key))
            .role(Role::ListBoxOption)
            .aria_label(row_name(node, title, row.is_working_copy, is_base))
            .aria_description(format!(
                "{} · {}",
                node.author,
                absolute_time(&node.timestamp)
            ))
            .aria_selected(selected)
            .when(style.keys.focused && anchor, |el| {
                el.aria_active_descendant()
            })
            .when(style.keys.ringed && anchor, |el| el.shadow(ring(t.accent)))
            .relative()
            .w_full()
            .h(px(height))
            .gap(px(8.))
            .pr(px(12.))
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    window.focus(&focus, cx);
                    this.select(id.clone(), event.modifiers(), cx);
                }),
            )
            .on_mouse_down(MouseButton::Right, menu::change_menu(node.id.clone()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener({
                    let id = node.id.clone();
                    move |this, event: &gpui::MouseDownEvent, _, _| {
                        this.press_row(&id, event.position)
                    }
                }),
            )
            .child(super::dnd::row_probe(
                self.drag.rows.clone(),
                node.id.clone(),
            ));
        // The drag in hand, or the plan a header panel is scrubbing: the
        // row in hand dims, the prospective parent answers, and the rows
        // the rewrite would touch wear a warm, quiet wash.
        match self.washes.of(&node.id) {
            Some(super::dnd::RowWash::Source) => el = el.opacity(0.4),
            Some(super::dnd::RowWash::Target { allowed }) => {
                let (fill, line) = if allowed {
                    (t.accent_dim, t.accent)
                } else {
                    (Theme::fade(t.danger, 0.07), Theme::fade(t.danger, 0.45))
                };
                el = el.child(
                    div()
                        .absolute()
                        .inset_0()
                        .bg(fill)
                        .border_1()
                        .border_color(line),
                );
            }
            Some(super::dnd::RowWash::Affected) => {
                let mut wash = div().absolute().inset_0().bg(Theme::fade(t.warn, 0.09));
                if !selected {
                    wash = wash.border_l_2().border_color(Theme::fade(t.warn, 0.7));
                }
                el = el.child(wash);
            }
            None => {}
        }
        // The selection takes its line's hue: a tint of it across the row
        // and the line's color on the leading bar. History selects in the
        // neutral fill.
        let hue = (!is_base).then_some(lane_color);
        el = if selected {
            el.bg(hue.map_or(t.selected, |hue| {
                Theme::fade(hue, if t.is_dark() { 0.16 } else { 0.11 })
            }))
        } else if is_compare_base {
            el.bg(Theme::fade(t.accent, 0.05))
        } else {
            el.hover(|style| style.bg(t.bg_hover))
        };
        // The leading bar marks the selection, and the far end of an
        // active comparison with it, so the pair reads at a glance.
        if selected {
            el = el.child(lead_bar_in(hue.unwrap_or(t.accent)));
        } else if is_compare_base {
            el = el.child(lead_bar(cx));
        }
        el = el.child(gutter);
        if classic {
            // Classic mode stacks the row into two lines: subject and refs
            // on top, identity and authorship beneath.
            el.child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(h_flex().min_w_0().gap(px(8.)).child(desc).child(tags))
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap(px(8.))
                            .child(ids)
                            .child(spacer())
                            .child(meta),
                    ),
            )
        } else {
            el.child(ids)
                .child(desc)
                .child(tags)
                .child(spacer())
                .child(meta)
        }
    }

    /// jj's `~`: history exists here but is not shown.
    fn elision_row(&self, row: &ElisionRow, key: usize, style: &RowStyle) -> Stateful<Div> {
        let t = style.theme.clone();
        let hot_lane = style.hot_lane();
        let full = style.full;
        let continues = row.continues;
        let passes: Vec<(Rail, Hsla)> = row
            .pass_through
            .iter()
            .map(|rail| (*rail, style.rail_color(rail, hot_lane)))
            .collect();
        let marks: Vec<(Rail, Hsla)> = row
            .marks
            .iter()
            .map(|rail| (*rail, style.rail_color(rail, hot_lane)))
            .collect();
        let (mono, tilde) = (t.mono.clone(), t.text3);
        let gutter = canvas(
            |_, _, _| (),
            move |bounds, (), window, cx| {
                let cy = bounds.top() + bounds.size.height / 2.;
                let rail = |window: &mut Window, rail: &Rail, color: Hsla, from: Pixels| {
                    let tier = lane_tier(rail.col, full);
                    let color = if tier == LaneTier::Overflow {
                        Theme::fade(color, 0.55)
                    } else {
                        color
                    };
                    vertical(
                        window,
                        bounds.left() + px(rail_x(rail.col, full)),
                        from,
                        bounds.bottom(),
                        if tier == LaneTier::Full { RAIL_W } else { 1. },
                        color,
                    );
                };
                for (pass, color) in &passes {
                    rail(window, pass, *color, bounds.top());
                }
                for (mark, color) in &marks {
                    if continues {
                        rail(window, mark, *color, cy + px(6.));
                    }
                    let narrow = lane_tier(mark.col, full) != LaneTier::Full;
                    let font_size = if narrow { 8. } else { 11. };
                    let run = TextRun {
                        len: 1,
                        font: gpui::font(mono.clone()),
                        color: tilde,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };
                    let line =
                        window
                            .text_system()
                            .shape_line("~".into(), px(font_size), &[run], None);
                    let line_height = px(font_size * 1.3);
                    let origin = point(
                        bounds.left() + px(rail_x(mark.col, full)) - line.width / 2.,
                        cy - line_height / 2.,
                    );
                    let _ = line.paint(origin, line_height, TextAlign::Left, None, window, cx);
                }
            },
        )
        .w(px(row_gutter_width(row.last_column(), full)))
        .h(px(ELISION_ROW_HEIGHT))
        .flex_shrink_0();
        h_flex()
            .id(("elision", key))
            .w_full()
            .h(px(ELISION_ROW_HEIGHT))
            .gap(px(8.))
            .tooltip(tooltip("History between these commits is not shown"))
            .child(gutter)
            .child(
                div()
                    .text_size(px(TEXT_XS))
                    .text_color(t.text3)
                    .child("elided revisions"),
            )
    }

    fn rows(
        &self,
        rows: &[GraphRow],
        key_base: usize,
        style: &RowStyle,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        rows.iter()
            .enumerate()
            .map(|(ix, row)| match row {
                GraphRow::Node(node) => self
                    .node_row(node, key_base + ix, style, cx)
                    .into_any_element(),
                GraphRow::Elision(gap) => self
                    .elision_row(gap, key_base + ix, style)
                    .into_any_element(),
            })
            .collect()
    }

    fn note(&self, text: impl IntoElement, cx: &App) -> Div {
        let t = theme(cx);
        div()
            .px(px(16.))
            .py(px(10.))
            .text_size(px(TEXT_S))
            .text_color(t.text3)
            .child(text)
    }

    /// Every workstream in one tree. Only the rows near the viewport are
    /// built; the rest are just height.
    fn render_graph(
        &mut self,
        width: f32,
        keys: GraphKeys,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let model = self.model.clone();
        let snapshot = self.snapshot.clone();
        if model.rows.is_empty() {
            return div().size_full().child(empty_state(
                IconName::Workbench,
                "Nothing to show",
                if snapshot.view == SnapshotView::Workstreams {
                    "This repository has no visible changes yet."
                } else {
                    "Nothing matches this view."
                },
                Some("jj new".into()),
                cx,
            ));
        }
        let selected = self.selected_ids(cx);
        let ui = self.app.read(cx).ui();
        let emphasized = emphasized_stream(
            &snapshot,
            ui.selected_node.as_deref(),
            ui.focused_workstream.as_deref(),
        );

        // A move that has played out is over; until then it asks for
        // frames, and none once it is.
        let now = cx.background_executor().now();
        if self
            .motion
            .as_ref()
            .is_some_and(|motion| motion.progress(now) >= 1.)
        {
            self.motion = None;
        }
        let progress = self.motion.as_ref().map(|motion| motion.progress(now));

        let tops = motion::tops(&model, |row| self.row_height(row));
        let total = tops[model.rows.len()];
        let scroll_top: f32 = (-self.scroll.offset().y).into();
        let viewport: f32 = self.scroll.bounds().size.height.into();
        let viewport = if viewport > 0. { viewport } else { 900. };
        let mut shown = rows_in_view(&tops, scroll_top, viewport);
        // Rows on their way in from where they stood are drawn from the
        // start of the move.
        let known = self
            .motion
            .as_ref()
            .map(|_| motion::row_keys(&snapshot, &model));
        if let (Some(motion), Some(known)) = (&self.motion, &known) {
            for (ix, key) in known.iter().enumerate() {
                let arriving = key
                    .as_deref()
                    .is_some_and(|key| motion.came_from(key, scroll_top, scroll_top + viewport));
                if arriving {
                    shown = shown.start.min(ix)..shown.end.max(ix + 1);
                }
            }
        }
        let first = shown.start;

        let style = self.row_style(
            &snapshot,
            &model,
            width,
            &selected,
            emphasized.clone(),
            keys,
            cx,
        );
        let rows = self.rows(&model.rows[shown], first, &style, cx);
        let mut list = div().relative().w_full().h(px(total));
        // The rows a rewrite took away fold where they stood, under the
        // rows moving into their place.
        if let (Some(motion), Some(progress)) = (&self.motion, progress) {
            let old = self.row_style(
                &motion.old_snapshot,
                &motion.old_model,
                width,
                &selected,
                emphasized,
                keys,
                cx,
            );
            for (ix, top) in &motion.gone {
                let row = &motion.old_model.rows[*ix];
                let height = self.row_height(row) * (1. - ease_out(progress));
                list = list.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top(px(*top))
                        .h(px(height))
                        .overflow_hidden()
                        .opacity(1. - progress)
                        .children(self.rows(std::slice::from_ref(row), GONE_KEYS + ix, &old, cx)),
                );
            }
        }
        for (ix, row) in rows.into_iter().enumerate() {
            let index = first + ix;
            let (top, strength) = match (&self.motion, &known, progress) {
                (Some(motion), Some(known), Some(progress)) => {
                    motion.place(known[index].as_deref(), tops[index], progress)
                }
                _ => (tops[index], 1.),
            };
            let id = Motion::key(&snapshot, &model.rows[index]);
            list = list.child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(top))
                    .when(strength < 1., |el| el.opacity(strength))
                    .debug_selector(|| format!("graph-row-{}", id.unwrap_or("elision")))
                    .child(row),
            );
        }
        if self.motion.is_some() {
            window.request_animation_frame();
        }
        let mut body = v_flex().w_full().py(px(4.)).child(list);
        if snapshot.truncated {
            body = body.child(self.note(
                "History goes deeper — this view stops at its newest 1,000 commits.",
                cx,
            ));
        }
        if snapshot.workstreams.is_empty() && snapshot.view == SnapshotView::Workstreams {
            let t = theme(cx);
            body = body.child(
                self.note(
                    h_flex()
                        .gap(px(4.))
                        .child("Everything here is already on trunk.")
                        .child(div().font_family(t.mono.clone()).child("jj new"))
                        .child("starts a change."),
                    cx,
                ),
            );
        }
        body
    }

    /// One workstream at a time: the chain under its own band, the
    /// immutable base it sits on in a recessed zone below, the other lanes
    /// as a list.
    fn render_focus(
        &mut self,
        width: f32,
        keys: GraphKeys,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let snapshot = self.snapshot.clone();
        let focused = self.app.read(cx).ui().focused_workstream.clone();
        let lane = resolve_focused_workstream(&snapshot, focused.as_deref()).cloned();
        let (Some(lane), Some((_, focus))) = (lane, self.focus_model.clone()) else {
            return div().size_full().child(empty_state(
                IconName::Workbench,
                "No mutable work",
                "Everything here is already on trunk. New changes will show up as a workstream.",
                Some("jj new".into()),
                cx,
            ));
        };
        let selected = self.selected_ids(cx);
        let style = self.row_style(
            &self.snapshot,
            &focus.graph,
            width,
            &selected,
            Some(lane.id.clone()),
            keys,
            cx,
        );
        // The chain runs edge to edge under the workstream's own band; the
        // immutable base(s) and their terminal `~` rows sit in a recessed
        // zone below.
        let split_at = focus
            .graph
            .rows
            .iter()
            .position(|row| matches!(row, GraphRow::Node(node) if snapshot.nodes[node.node].kind == NodeKind::Immutable))
            .unwrap_or(focus.graph.rows.len());
        let chain = self.rows(&focus.graph.rows[..split_at], 0, &style, cx);
        let base = self.rows(&focus.graph.rows[split_at..], split_at, &style, cx);
        let gutter = style.gutter;
        let slot = lane_of(&snapshot, &lane.id).unwrap_or(0);

        let mut meta = h_flex()
            .flex_shrink_0()
            .gap(px(4.))
            .text_size(px(TEXT_S))
            .text_color(t.text3)
            .child(counted(lane.node_ids.len(), "change"));
        if focus.behind_trunk > 0 {
            meta = meta.child(
                div()
                    .text_color(t.warn)
                    .child(format!("· {} behind trunk", focus.behind_trunk)),
            );
        }
        let mut head = h_flex()
            .h(px(36.))
            .gap(px(8.))
            .pl(px(8.))
            .pr(px(12.))
            .border_b_1()
            .border_color(t.border2)
            .child(
                div()
                    .w(px(8.))
                    .flex()
                    .justify_center()
                    .child(crate::ui::node_mark(
                        crate::ui::Mark::Station { filled: true },
                        t.lane(slot),
                        10.,
                    )),
            )
            .child(
                div()
                    .min_w_0()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(truncated(lane.title.clone())),
            );
        if let Some(bookmark) = &lane.bookmark {
            head = head.child(
                ref_label(None, bookmark.clone(), cx)
                    .flex_shrink_1()
                    .min_w_0(),
            );
        }
        head = head.child(div().flex_1());
        let mut lane_block = v_flex()
            .w_full()
            .border_b_1()
            .border_color(t.border2)
            .child(head.child(meta))
            .child(v_flex().py(px(4.)).children(chain));
        if !base.is_empty() {
            let mut zone = v_flex()
                .border_t_1()
                .border_color(t.border2)
                .bg(t.recess)
                .pb(px(4.))
                .child(
                    h_flex()
                        .h(px(24.))
                        .child(div().w(px(gutter)).flex_shrink_0())
                        .child(
                            div()
                                .text_size(px(TEXT_XS))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(t.text3)
                                .child(if focus.trunk_on_base {
                                    "Trunk"
                                } else {
                                    "Based on"
                                }),
                        ),
                )
                .children(base);
            if focus.behind_trunk > 0 && !snapshot.trunk_bookmark.is_empty() {
                zone = zone.child(
                    div()
                        .pl(px(46.))
                        .pr(px(16.))
                        .py(px(4.))
                        .text_size(px(TEXT_S))
                        .text_color(t.warn)
                        .child(format!(
                            "{} is {} ahead of this base",
                            snapshot.trunk_bookmark,
                            counted(focus.behind_trunk as usize, "change")
                        )),
                );
            }
            lane_block = lane_block.child(zone);
        }

        let mut body = v_flex().w_full().pb(px(12.)).child(lane_block);
        let siblings: Vec<&WorkstreamSummary> = snapshot
            .workstreams
            .iter()
            .filter(|w| w.id != lane.id)
            .collect();
        if !siblings.is_empty() {
            body = body.child(
                h_flex()
                    .h(px(28.))
                    .mt(px(12.))
                    .px(px(12.))
                    .gap(px(6.))
                    .text_size(px(TEXT_XS))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text3)
                    .child("Other workstreams")
                    .child(
                        div()
                            .font_family(t.mono.clone())
                            .font_weight(FontWeight::NORMAL)
                            .child(siblings.len().to_string()),
                    ),
            );
            for (ix, sibling) in siblings.into_iter().enumerate() {
                body = body.child(self.sibling_lane(sibling, ix, cx));
            }
        }
        div().w_full().child(body)
    }

    fn sibling_lane(
        &self,
        workstream: &WorkstreamSummary,
        key: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let color = t.lane(lane_of(&self.snapshot, &workstream.id).unwrap_or(0));
        let count = workstream.node_ids.len();
        // The line in miniature: a short run of track with up to three
        // stations on it.
        let stations = count.clamp(1, 3);
        let mut mini = v_flex()
            .relative()
            .w(px(12.))
            .flex_shrink_0()
            .items_center()
            .gap(px(3.))
            .child(
                div()
                    .absolute()
                    .top(px(3.))
                    .bottom(px(3.))
                    .left(px(5.))
                    .w(px(2.))
                    .bg(color),
            );
        for ix in 0..stations {
            let is_wc = workstream.is_active && ix == 0;
            mini = mini.child(
                div()
                    .size(px(8.))
                    .rounded(px(4.))
                    .border_2()
                    .border_color(if is_wc { t.working_copy } else { color })
                    .bg(t.bg1),
            );
        }
        let mut line = h_flex().min_w_0().gap(px(6.)).child(
            div()
                .min_w_0()
                .text_color(t.text1)
                .child(truncated(workstream.title.clone())),
        );
        if let Some(bookmark) = &workstream.bookmark {
            line = line.child(
                ref_label(None, bookmark.clone(), cx)
                    .flex_shrink_1()
                    .min_w_0(),
            );
        }
        if workstream.is_active {
            line = line.child(
                div()
                    .flex_shrink_0()
                    .text_size(px(TEXT_XS))
                    .text_color(t.working_copy)
                    .child("working copy"),
            );
        }
        let mut meta = format!("{count} change{}", if count == 1 { "" } else { "s" });
        if workstream.behind_trunk > 0 {
            meta.push_str(&format!(" · {} behind trunk", workstream.behind_trunk));
        }
        let id = workstream.id.clone();
        h_flex()
            .id(("sibling", key))
            .w_full()
            .gap(px(10.))
            .px(px(12.))
            .py(px(6.))
            .cursor_pointer()
            .hover(|style| style.bg(t.bg_hover))
            .on_click(cx.listener(move |this, _, _, cx| this.focus_workstream(id.clone(), cx)))
            .on_mouse_down(MouseButton::Left, |_, _, _| {})
            .child(mini)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(line)
                    .child(div().text_size(px(TEXT_XS)).text_color(t.text3).child(meta)),
            )
            .child(icon(IconName::ChevronRight, 14., t.text3))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(name: &str, revset: &str) -> SavedView {
        SavedView {
            name: name.into(),
            revset: revset.into(),
        }
    }

    #[test]
    fn refs_that_fit_ride_whole() {
        let mut labels = RowLabels::new(200., 2);
        assert_eq!(labels.grant(60., 50., "one"), Some(60.));
        assert_eq!(labels.grant(60., 50., "two"), Some(60.));
        assert!(labels.hidden.is_empty());
    }

    #[test]
    fn a_ref_truncates_into_what_the_title_spares() {
        let mut labels = RowLabels::new(90., 1);
        // The last ref needs no seat kept for a count, so it takes it all.
        assert_eq!(labels.grant(140., 56., "long"), Some(90.));
        assert!(labels.hidden.is_empty());
    }

    #[test]
    fn what_cannot_fit_is_counted_and_so_is_everything_after() {
        let mut labels = RowLabels::new(120., 3);
        assert_eq!(labels.grant(80., 56., "one"), Some(80.));
        // 120 - 84 used - the count's seat leaves no readable ref.
        assert_eq!(labels.grant(80., 56., "two"), None);
        // A short one would fit, but refs do not skip the queue.
        assert_eq!(labels.grant(20., 20., "three"), None);
        assert_eq!(labels.hidden, ["two", "three"]);
        // Nor does a state word ride after the count.
        assert!(!labels.fits(10.));
    }

    #[test]
    fn the_count_keeps_its_seat_while_refs_remain() {
        let mut labels = RowLabels::new(100., 2);
        // The first of two leaves room for "+1" beside it.
        assert_eq!(labels.grant(140., 56., "one"), Some(100. - RowLabels::MORE));
        assert_eq!(labels.grant(60., 56., "two"), None);
        assert_eq!(labels.hidden, ["two"]);
    }

    #[test]
    fn a_state_word_leaves_without_a_trace() {
        let mut labels = RowLabels::new(40., 0);
        assert!(!labels.fits(54.));
        assert!(labels.fits(34.));
        assert!(labels.hidden.is_empty());
    }

    #[test]
    fn saving_appends_or_replaces_by_name() {
        let views = vec![view("wip", "description(wip)")];
        let added = upsert_saved_view(&views, " mine ", " mine() ").unwrap();
        assert_eq!(
            added,
            vec![view("wip", "description(wip)"), view("mine", "mine()")]
        );
        let replaced = upsert_saved_view(&added, "wip", "all()").unwrap();
        assert_eq!(replaced[0], view("wip", "all()"));
        assert_eq!(replaced.len(), 2);
        assert!(upsert_saved_view(&views, "  ", "x").is_none());
        assert!(upsert_saved_view(&views, "x", "").is_none());
    }

    #[test]
    fn renaming_refuses_collisions_and_missing_targets() {
        let views = vec![view("a", "1"), view("b", "2")];
        assert_eq!(
            rename_saved_view(&views, "a", " c ").unwrap(),
            vec![view("c", "1"), view("b", "2")]
        );
        assert!(rename_saved_view(&views, "a", "b").is_none());
        assert!(rename_saved_view(&views, "gone", "c").is_none());
        assert!(rename_saved_view(&views, "a", " ").is_none());
        // Renaming to the same name is a no-op, not a collision.
        assert_eq!(rename_saved_view(&views, "a", "a").unwrap(), views);
    }

    #[test]
    fn lenses_label_themselves() {
        assert_eq!(view_label(&SnapshotView::Trunk), "Trunk history");
        for (preset, label, _) in VIEW_PRESETS.iter() {
            assert_eq!(view_label(preset), *label);
        }
        assert_eq!(
            view_label(&SnapshotView::Custom { revset: "x".into() }),
            "Custom revset"
        );
    }

    #[test]
    fn deleting_a_view_leaves_the_rest_in_order() {
        let views = vec![view("WIP", "description(wip)"), view("Mine", "mine()")];
        assert_eq!(delete_saved_view(&views, "WIP"), [view("Mine", "mine()")]);
        assert_eq!(delete_saved_view(&views, "gone"), views);
    }

    #[test]
    fn a_row_says_what_a_glance_at_it_takes_in() {
        let snapshot = jiji_core::mock::mock_snapshot(std::path::Path::new("/repo"));
        let node = |id: &str| snapshot.nodes.iter().find(|node| node.id == id).unwrap();
        let working = node(&snapshot.working_copy);
        assert_eq!(
            row_name(working, "", true, false),
            format!("No description yet, {}, working copy", &working.id[..8])
        );
        let conflicted = snapshot
            .nodes
            .iter()
            .find(|node| node.has_conflict && !node.bookmarks.is_empty())
            .expect("the mock has a bookmarked conflict");
        let name = row_name(conflicted, "the title", false, false);
        assert!(name.starts_with("the title, "), "{name}");
        assert!(name.contains(", conflicted"), "{name}");
        assert!(name.ends_with(&conflicted.bookmarks.join(", ")), "{name}");
    }
}
