//! The graph pane: the lens picker, the view toggle, and the rows — one
//! continuous jj-native tree, or one workstream's lane at a time.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::{
    canvas, div, fill, point, prelude::*, px, quad, AnyElement, App, BorderStyle, Bounds, Context,
    Corner, Div, FontWeight, Hsla, MouseButton, PathBuilder, Pixels, SharedString, Stateful,
    TextRun, Window,
};
use jiji_core::snapshot::{NodeKind, RepoSnapshot, SnapshotView, WorkstreamSummary};
use jiji_forge::RepoPrState;

use super::{ShownDiff, WorkbenchView};
use crate::app::WorkbenchMode;
use crate::logic::graph::{
    emphasized_stream, full_lanes_for, gutter_width, lane_of, lane_tier, rail_x,
    resolve_focused_workstream, row_bookmarks, sync_glyph, ElisionRow, GraphModel, GraphRow,
    LaneTier, NodeRow, Rail, RowDensity, ELISION_ROW_HEIGHT, RAIL_CORNER,
};
use crate::logic::inspect::summary_of;
use crate::logic::pr::{pr_badge, pr_for_bookmark, PrTone};
use crate::logic::text::counted;
use crate::store::SavedView;
use crate::theme::{
    theme, Theme, RADIUS_L, RADIUS_M, RADIUS_S, TEXT_L, TEXT_M, TEXT_S, TEXT_XS, TOOLBAR_H,
};
use crate::ui::ScrollY as _;
use crate::ui::{
    absolute_time, btn, empty_state, h_flex, icon, icon_btn, input::field, menu_label, popover,
    popover_layer, ref_label, scrollbar, segmented, short_age, tooltip, truncated, v_flex,
    IconName,
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

/// Panes this wide or narrower leave authors out of the rows.
const AUTHOR_MIN_PANE: f32 = 560.;

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
}

impl RowStyle<'_> {
    fn rail_color(&self, rail: &Rail, hot_lane: Option<usize>) -> Hsla {
        let t = &self.theme;
        match rail.lane {
            None => Theme::blend(t.text3, 0.5, t.bg1),
            Some(lane) if Some(lane) == hot_lane => Theme::blend(t.lane(lane), 0.8, t.bg1),
            Some(lane) => Theme::blend(t.lane(lane), 0.38, t.bg1),
        }
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
    mono: SharedString,
}

impl GutterPaint {
    fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let (left, top, bottom) = (bounds.left(), bounds.top(), bounds.bottom());
        let height: f32 = bounds.size.height.into();
        let cy = top + px(height / 2.);
        let nx = left + px(rail_x(self.column, self.full));
        let clear = px(self.clear);

        for (role, rail, color) in &self.rails {
            let tier = lane_tier(rail.col, self.full);
            let width = if tier == LaneTier::Full { 1.5 } else { 1. };
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
                    // Down the rail's own column, round the corner, then
                    // across to the node.
                    let s = px(dx.signum());
                    let r = RAIL_CORNER.min(dx.abs());
                    let reach = self.clear.min(dx.abs() - r);
                    stroke(window, color, width, rail.elided, |path| {
                        path.move_to(point(x, top));
                        path.line_to(point(x, cy - px(r)));
                        path.curve_to(point(x + s * r, cy), point(x, cy));
                        path.line_to(point(nx - s * reach, cy));
                    });
                }
                RailRole::Out => {
                    let away: f32 = (x - nx).into();
                    let s = px(away.signum());
                    let r = RAIL_CORNER.min(away.abs());
                    let reach = self.clear.min(away.abs() - r);
                    stroke(window, color, width, rail.elided, |path| {
                        path.move_to(point(nx + s * reach, cy));
                        path.line_to(point(x - s * r, cy));
                        path.curve_to(point(x, cy + px(r)), point(x, cy));
                        path.line_to(point(x, bottom));
                    });
                }
            }
        }
        if let Some(color) = self.stub_below {
            let width = if self.narrow { 1. } else { 1.5 };
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
                let font_size = if self.narrow {
                    10.
                } else if self.classic {
                    15.
                } else {
                    13.
                };
                let run = TextRun {
                    len: 1,
                    font: gpui::Font {
                        family: self.mono.clone(),
                        features: gpui::FontFeatures::default(),
                        fallbacks: None,
                        weight: FontWeight::BOLD,
                        style: gpui::FontStyle::Normal,
                    },
                    color: self.working_copy,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let line = window
                    .text_system()
                    .shape_line("@".into(), px(font_size), &[run], None);
                let line_height = px(font_size * 1.3);
                let origin = point(nx - line.width / 2., cy - line_height / 2. + px(0.5));
                let _ = line.paint(origin, line_height, window, cx);
            }
            Marker::Conflict => {
                let xr = px(self.radius - 0.8);
                let width = if self.narrow { 1.4 } else { 1.8 };
                stroke(window, self.danger, width, false, |path| {
                    path.move_to(point(nx - xr, cy - xr));
                    path.line_to(point(nx + xr, cy + xr));
                });
                stroke(window, self.danger, width, false, |path| {
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
                let width = if self.narrow { 1.2 } else { 1.5 };
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

fn gutter_canvas(paint: GutterPaint, width: f32, height: f32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, cx| paint.paint(bounds, window, cx),
    )
    .w(px(width))
    .h(px(height))
    .flex_shrink_0()
}

/// A small ref-shaped chip in a custom tone.
fn chip(
    text: impl Into<SharedString>,
    fg: Hsla,
    bg: Option<Hsla>,
    mono: Option<SharedString>,
) -> Div {
    let mut el = h_flex()
        .flex_shrink_0()
        .gap(px(3.))
        .h(px(17.))
        .px(px(5.))
        .rounded(px(RADIUS_S))
        .text_size(px(TEXT_XS))
        .text_color(fg)
        .whitespace_nowrap();
    if let Some(bg) = bg {
        el = el.bg(bg);
    }
    if let Some(mono) = mono {
        el = el.font_family(mono);
    }
    el.child(text.into())
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
                vec![
                    (WorkbenchMode::Graph, "Graph".into(), None),
                    (WorkbenchMode::Focus, "Focus".into(), None),
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
                .tooltip(if classic {
                    "Classic rows — switch to compact, jj-dense rows"
                } else {
                    "Compact rows — switch to larger, classic rows"
                })
                .on_click(cx.listener(|this, _, _, cx| this.toggle_density(cx))),
            );

        let body = match mode {
            WorkbenchMode::Graph => self.render_graph(width, cx).into_any_element(),
            WorkbenchMode::Focus => self.render_focus(width, cx).into_any_element(),
        };
        v_flex().size_full().child(head).child(
            div()
                .id("graph-pane")
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
                        .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()))
                        .child(body),
                )
                .child(scrollbar("graph-scrollbar", &self.scroll)),
        )
    }

    // ── The lens picker ──

    fn trigger_label(&self) -> String {
        let count = self.snapshot.workstreams.len();
        match &self.snapshot.view {
            // The trigger reads as the pane label: the workstream count on
            // the default lens, the lens name on any other — a custom lens
            // wears its saved name when it has one.
            SnapshotView::Workstreams => match count {
                0 => "No workstreams".into(),
                1 => "1 workstream".into(),
                n => format!("{n} workstreams"),
            },
            SnapshotView::Custom { revset } => self
                .view_menu
                .saved
                .iter()
                .find(|view| &view.revset == revset)
                .map_or_else(|| "Custom revset".to_owned(), |view| view.name.clone()),
            other => view_label(other).to_owned(),
        }
    }

    fn toggle_view_menu(&mut self, cx: &mut Context<Self>) {
        self.view_menu.popover.toggle();
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

    fn apply_view(&mut self, view: SnapshotView, cx: &mut Context<Self>) {
        if self.view_menu.busy {
            return;
        }
        self.view_menu.busy = true;
        self.view_menu.error = None;
        cx.notify();
        let task = self
            .app
            .update(cx, |model, cx| model.set_snapshot_view(view, true, cx));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.view_menu.busy = false;
                match result {
                    Ok(()) => this.view_menu.popover.close(),
                    // The backend's refusal, inline: the graph keeps
                    // rendering what it was.
                    Err(error) => this.view_menu.error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn apply_revset(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let revset = self.view_menu.revset.read(cx).text().trim().to_owned();
        if !revset.is_empty() {
            self.apply_view(SnapshotView::Custom { revset }, cx);
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
        let mut trigger = h_flex()
            .id("view-menu")
            .gap(px(4.))
            .h(px(24.))
            .px(px(6.))
            .min_w_0()
            .rounded(px(RADIUS_M))
            .text_size(px(TEXT_S))
            .font_weight(FontWeight::MEDIUM)
            .text_color(if lensed { t.accent_strong } else { t.text2 })
            .cursor_pointer()
            .hover(|style| style.bg(t.bg_hover).text_color(t.text1))
            .tooltip(tooltip("Change what the graph shows"))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_view_menu(cx)))
            .child(div().min_w_0().child(truncated(self.trigger_label())))
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
        let preset_row =
            |id: gpui::ElementId, label: String, desc: String, mono: bool, on: bool| {
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
                h_flex()
                    .id(id)
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
        for (ix, (view, label, desc)) in VIEW_PRESETS.iter().enumerate() {
            let target = view.clone();
            panel = panel.child(
                preset_row(
                    ("lens", ix).into(),
                    (*label).to_owned(),
                    (*desc).to_owned(),
                    false,
                    *view == current,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.apply_view(target.clone(), cx))),
            );
        }

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
                                move |this, _, _, cx| this.apply_view(target.clone(), cx),
                            )),
                        )
                        .child(
                            icon_btn(("rename-saved", ix), IconName::Edit)
                                .size(22., 12.)
                                .tooltip("Rename view")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.view_menu.error = None;
                                    this.view_menu.renaming = Some(rename_name.clone());
                                    let name = rename_name.clone();
                                    this.view_menu.rename.update(cx, |input, cx| {
                                        input.set_text(name, cx);
                                        input.focus(window);
                                    });
                                    cx.notify();
                                })),
                        )
                        .child(
                            icon_btn(("delete-saved", ix), IconName::Trash)
                                .size(22., 12.)
                                .tooltip("Delete view")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this.view_menu.renaming.as_deref() == Some(&delete_name) {
                                        this.view_menu.renaming = None;
                                    }
                                    let next: Vec<SavedView> = this
                                        .view_menu
                                        .saved
                                        .iter()
                                        .filter(|view| view.name != delete_name)
                                        .cloned()
                                        .collect();
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
                            .update(cx, |input, _| input.focus(window));
                        cx.notify();
                    }))
                    .child("Save as view…"),
            );
        }
        if let Some(error) = &self.view_menu.error {
            panel = panel.child(
                div()
                    .px(px(8.))
                    .pb(px(6.))
                    .text_size(px(TEXT_XS))
                    .line_height(gpui::relative(1.5))
                    .text_color(t.danger)
                    .child(error.clone()),
            );
        }
        anchor.child(popover_layer(
            Corner::TopLeft,
            28.,
            panel,
            cx.listener(|this, _, _, cx| {
                this.view_menu.popover.close();
                cx.notify();
            }),
        ))
    }

    // ── Rows ──

    fn row_style<'a>(
        &'a self,
        model: &GraphModel,
        width: f32,
        selected: &'a HashSet<String>,
        emphasized: Option<String>,
        cx: &App,
    ) -> RowStyle<'a> {
        let full = full_lanes_for(width, model.column_count);
        RowStyle {
            snapshot: &self.snapshot,
            theme: theme(cx),
            emphasized,
            selected,
            full,
            gutter: gutter_width(model.column_count, full),
            density: self.density,
            prs: self.app.read(cx).forge.prs.clone(),
            comparison: self.shown.as_ref().filter(|shown| shown.from.is_some()),
            authors: width > AUTHOR_MIN_PANE,
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
            (false, true) => 5.,
            (false, false) => 4.,
        };
        // Clearance around the marker so rails do not pierce hollow glyphs.
        let clear = if narrow {
            radius + 2.
        } else {
            (if row.is_working_copy { 7. } else { 6. }) + if classic { 1. } else { 0. }
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
                ring: if hot {
                    lane_color
                } else {
                    Theme::blend(lane_color, 0.55, t.text3)
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
                mono: t.mono.clone(),
            },
            style.gutter,
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
            h_flex()
                .flex_shrink_0()
                .font_family(t.mono.clone())
                .text_size(px(TEXT_S))
                .text_color(t.text3)
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if is_base { t.text2 } else { t.text1 })
                        .child(short(&node.id, 0, 2)),
                )
                .child(short(&node.id, 2, 8))
                .into_any_element()
        };

        let title = summary_of(&node.description);
        let mut desc =
            div()
                .min_w(px(40.))
                .flex_shrink()
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
        let is_compare_to = style.comparison.is_some_and(|shown| shown.id == node.id);

        let mut tags = h_flex()
            .flex_shrink_0()
            .min_w_0()
            .gap(px(4.))
            .overflow_hidden();
        if is_compare_base {
            tags = tags.child(
                chip("diff base", t.accent_strong, Some(t.accent_dim), None)
                    .id(("diff-base", key))
                    .tooltip(tooltip(
                        "The diff pane measures the selection against this change",
                    )),
            );
        }
        if node.has_conflict {
            tags = tags.child(chip("conflict", t.danger, Some(t.danger_dim), None));
        }
        for (bx, bookmark) in row_bookmarks(style.snapshot, row).into_iter().enumerate() {
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
            let (fg, bg) = if !bookmark.is_local {
                (t.text3, None)
            } else if bookmark.is_trunk {
                // Trunk is the one ref the whole graph measures against.
                (t.accent_strong, Some(t.accent_dim))
            } else {
                (t.text2, Some(t.ref_bg))
            };
            let mut label = h_flex()
                .id(("bm", key * 64 + bx))
                .flex_shrink_0()
                .gap(px(3.))
                .h(px(17.))
                .px(px(if bookmark.is_local { 5. } else { 2. }))
                .rounded(px(RADIUS_S))
                .font_family(t.mono.clone())
                .text_size(px(TEXT_XS))
                .text_color(fg)
                .tooltip(tooltip(format!("{full_name} — {sync_label}")))
                .on_mouse_down(
                    MouseButton::Right,
                    menu::bookmark_menu(bookmark.name.clone(), bookmark.remote.clone()),
                )
                .child(div().max_w(px(104.)).child(truncated(shown)));
            if let Some(bg) = bg {
                label = label.bg(bg);
            }
            if !glyph.is_empty() {
                label = label.child(
                    div()
                        .text_size(px(10.))
                        .text_color(t.tone(tone))
                        .child(glyph),
                );
            }
            tags = tags.child(label);
            if let Some(pr) = pr_for_bookmark(bookmark, style.prs.as_deref()) {
                // The bookmark's pull request, right beside its label.
                // Tones read like GitHub's states: open green, draft muted,
                // merged accent, closed danger.
                let badge = pr_badge(pr);
                let (fg, bg) = match badge.tone {
                    PrTone::Open => (t.ok, Some(t.ok_dim)),
                    PrTone::Draft => (t.text3, Some(t.ref_bg)),
                    PrTone::Merged => (t.accent_strong, Some(t.accent_dim)),
                    PrTone::Closed => (t.danger, Some(t.danger_dim)),
                };
                let mut el = chip(badge.label, fg, bg, Some(t.mono.clone()))
                    .id(("pr", key * 64 + bx))
                    .on_mouse_down(MouseButton::Right, menu::pr_menu(pr.number))
                    .tooltip(tooltip(badge.title));
                for glyph in [badge.review, badge.checks].into_iter().flatten() {
                    el = el.child(
                        div()
                            .text_size(px(10.))
                            .text_color(t.tone(glyph.tone))
                            .child(glyph.glyph),
                    );
                }
                tags = tags.child(el);
            }
        }
        // Colocated git tags, read-only: markers only, no tag management.
        for tag in &node.tags {
            tags = tags.child(ref_label(Some(IconName::Tag), tag.clone(), cx).max_w(px(120.)));
        }
        if row.is_stack_head && row.behind_trunk > 0 {
            tags = tags.child(
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
                    .child(format!("↓{}", row.behind_trunk)),
            );
        }
        if node.is_empty {
            tags = tags.child(
                div()
                    .flex_shrink_0()
                    .text_size(px(TEXT_XS))
                    .text_color(t.text3)
                    .child("empty"),
            );
        }

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
                    .child(short_age(&node.timestamp)),
            );
        // What pushes the meta to the row's far edge. (A spacer, not an
        // auto margin: the layout engine drops a row's gaps when an auto
        // margin absorbs free space.)
        let spacer = || div().flex_1().min_w_0();

        let id = node.id.clone();
        let focus = self.graph_focus.clone();
        // Row states paint edge to edge behind the content; the tree's
        // rails run straight through them, like a table row, not a
        // picked-up card.
        let mut el = h_flex()
            .id(("node", key))
            .relative()
            .w_full()
            .h(px(height))
            .gap(px(8.))
            .pr(px(12.))
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    window.focus(&focus);
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
        el = if selected {
            el.bg(t.accent_dim)
        } else if is_compare_base {
            el.bg(Theme::fade(t.accent, 0.05))
        } else {
            el.hover(|style| style.bg(t.bg_hover))
        };
        // An active comparison marks both endpoints on the row's leading
        // edge, so the pair reads at a glance.
        if is_compare_base || (is_compare_to && selected) {
            el = el.child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(2.))
                    .bg(t.accent),
            );
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
                        if tier == LaneTier::Full { 1.5 } else { 1. },
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
                    let _ = line.paint(origin, line_height, window, cx);
                }
            },
        )
        .w(px(style.gutter))
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
    fn render_graph(&mut self, width: f32, cx: &mut Context<Self>) -> impl IntoElement {
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

        let mut tops = Vec::with_capacity(model.rows.len() + 1);
        let mut total = 0.;
        for row in &model.rows {
            tops.push(total);
            total += self.row_height(row);
        }
        tops.push(total);
        let scroll_top: f32 = (-self.scroll.offset().y).into();
        let viewport: f32 = self.scroll.bounds().size.height.into();
        let viewport = if viewport > 0. { viewport } else { 900. };
        // A screenful stays built above and below, so a fling reveals
        // rows that already exist.
        let first = tops
            .partition_point(|top| *top <= scroll_top - viewport)
            .saturating_sub(1);
        let last = tops
            .partition_point(|top| *top < scroll_top + 2. * viewport)
            .min(model.rows.len());

        let style = self.row_style(&model, width, &selected, emphasized, cx);
        let rows = self.rows(&model.rows[first..last], first, &style, cx);
        let mut list = div().relative().w_full().h(px(total));
        for (ix, row) in rows.into_iter().enumerate() {
            list = list.child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(tops[first + ix]))
                    .child(row),
            );
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

    /// One workstream at a time: the chain in a card, the immutable base
    /// it sits on in a recessed zone below, the other lanes as a list.
    fn render_focus(&mut self, width: f32, cx: &mut Context<Self>) -> impl IntoElement {
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
        let inner = (width - 2. * 12.).max(240.);
        let style = self.row_style(&focus.graph, inner, &selected, Some(lane.id.clone()), cx);
        // The chain renders inside the card body; the immutable base(s)
        // and their terminal `~` rows sit in a recessed zone below.
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
            .px(px(12.))
            .border_b_1()
            .border_color(t.border2)
            .child(crate::ui::dot(t.lane(slot), 8.))
            .child(
                div()
                    .min_w_0()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(truncated(lane.title.clone())),
            );
        if let Some(bookmark) = &lane.bookmark {
            head = head.child(
                ref_label(None, bookmark.clone(), cx)
                    .flex_shrink()
                    .min_w_0(),
            );
        }
        head = head.child(div().flex_1());
        let mut card = v_flex()
            .w_full()
            .max_w(px(760.))
            .rounded(px(RADIUS_L))
            .border_1()
            .border_color(t.border2)
            .overflow_hidden()
            .child(head.child(meta))
            .child(v_flex().py(px(4.)).children(chain));
        if !base.is_empty() {
            let mut zone = v_flex()
                .border_t_1()
                .border_color(t.border2)
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
            card = card.child(zone);
        }

        let mut body = v_flex().w_full().p(px(12.)).child(card);
        let siblings: Vec<&WorkstreamSummary> = snapshot
            .workstreams
            .iter()
            .filter(|w| w.id != lane.id)
            .collect();
        if !siblings.is_empty() {
            body = body.child(
                div()
                    .mt(px(20.))
                    .mb(px(4.))
                    .px(px(8.))
                    .text_size(px(TEXT_XS))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text3)
                    .child("Other workstreams"),
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
        let mut mini = v_flex()
            .w(px(12.))
            .flex_shrink_0()
            .items_center()
            .gap(px(2.));
        for ix in 0..count.min(4) {
            let is_wc = workstream.is_active && ix == 0;
            mini = mini.child(crate::ui::dot(
                if is_wc { t.working_copy } else { color },
                if is_wc { 6. } else { 5. },
            ));
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
                    .flex_shrink()
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
            .max_w(px(760.))
            .gap(px(10.))
            .px(px(8.))
            .py(px(6.))
            .rounded(px(RADIUS_M))
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
        assert_eq!(
            view_label(&SnapshotView::Custom { revset: "x".into() }),
            "Custom revset"
        );
    }
}
