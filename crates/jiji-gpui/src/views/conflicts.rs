//! The conflict inbox: everything that needs attention, in plain language.
//! jj never blocks on a conflict — rewrites and syncs always complete, what
//! collided is recorded first-class, and other work continues meanwhile.

use gpui::{
    div, prelude::*, px, AnyElement, Context, Entity, FocusHandle, Focusable, FontWeight,
    MouseButton, MouseDownEvent, ScrollHandle, SharedString, Window,
};
use jiji_core::snapshot::{ConflictItem, ConflictKind, ConflictSide, RepoSnapshot};

use crate::app::{app, AppModel};
use crate::logic::conflicts::{
    can_recover_workspace, can_resolve, can_take_side, group_conflicts, merge_tool_label,
};
use crate::logic::inspect::{find_node, split_path, summary_of};
use crate::theme::{theme, Theme, RADIUS_S, TEXT_M, TEXT_S, TEXT_XS};
use crate::ui::pinned;
use crate::ui::ScrollY as _;
use crate::ui::{
    btn, empty_state, h_flex, icon, scrollbar, section_header, tooltip, truncated, v_flex, IconName,
};

/// The rows' shared left inset: the glyph column plus its gap.
const INSET: f32 = 24.;

pub struct ConflictsView {
    app: Entity<AppModel>,
    focus: FocusHandle,
    /// The side-pick in flight: change, path, side.
    taking: Option<(String, String, ConflictSide)>,
    /// The last refusal, shown under the item it belongs to.
    error: Option<(String, String)>,
    updating: bool,
    scroll: ScrollHandle,
}

impl Focusable for ConflictsView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ConflictsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let app = app(cx);
        cx.observe(&app, |_, _, cx| cx.notify()).detach();
        Self {
            app,
            focus: cx.focus_handle(),
            taking: None,
            error: None,
            updating: false,
            scroll: ScrollHandle::new(),
        }
    }

    /// Hand the file to the external merge tool. One tool window at a
    /// time: while any resolve is in flight (from here or the diff
    /// surface), every button waits.
    fn resolve(&mut self, item: String, change: String, path: String, cx: &mut Context<Self>) {
        self.error = None;
        let task = self
            .app
            .update(cx, |model, cx| model.resolve_conflict(change, path, cx));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.error = Some((item, error.message));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Keep one side wholesale — instant, no tool window to wait on.
    fn take(
        &mut self,
        item: String,
        change: String,
        path: String,
        side: ConflictSide,
        cx: &mut Context<Self>,
    ) {
        self.error = None;
        self.taking = Some((change.clone(), path.clone(), side));
        let task = self.app.update(cx, |model, cx| {
            model.mutate(cx, move |backend, repo| {
                backend.resolve_conflict_take(repo, &change, &path, side)
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.taking = None;
                if let Err(error) = result {
                    this.error = Some((item, error.message));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// `jj workspace update-stale` for the workspace Jiji has open.
    fn update_workspace(&mut self, item: String, cx: &mut Context<Self>) {
        self.error = None;
        self.updating = true;
        let task = self.app.update(cx, |model, cx| {
            model.mutate(cx, |backend, repo| backend.update_stale_workspace(repo))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.updating = false;
                if let Err(error) = result {
                    this.error = Some((item, error.message));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The head line: what happened, and — when the change is drawn — the
    /// jump into the workbench.
    fn render_head(
        &self,
        item: &ConflictItem,
        snapshot: &RepoSnapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let node = item
            .node_id
            .as_deref()
            .and_then(|id| find_node(snapshot, id));
        let group = SharedString::from(format!("conflict-head-{}", item.id));
        let (glyph, color) = match item.kind {
            ConflictKind::File => ("×", t.danger),
            ConflictKind::Bookmark => ("??", t.danger),
            ConflictKind::StaleWorkspace => ("!", t.warn),
        };
        let mut summary = div()
            .flex_1()
            .min_w_0()
            .text_size(px(TEXT_M))
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.text1)
            .child(item.summary.clone());
        let mut head = h_flex()
            .id(SharedString::from(format!("conflict-{}", item.id)))
            .group(group.clone())
            .gap(px(8.))
            .min_w_0()
            .rounded(px(RADIUS_S))
            .child(
                div()
                    .w(px(16.))
                    .flex_shrink_0()
                    .flex()
                    .justify_center()
                    .font_family(t.mono.clone())
                    .text_size(px(TEXT_M))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(color)
                    .child(glyph),
            );
        if let (Some(node), false) = (node, item.kind == ConflictKind::Bookmark) {
            summary = summary.group_hover(group.clone(), |style| style.text_color(t.accent_strong));
            let (app, id) = (self.app.clone(), node.id.clone());
            head = head
                .cursor_pointer()
                .tooltip(tooltip(format!("Open {} in the workbench", node.id)))
                .on_click(move |_, _, cx| app.update(cx, |model, cx| model.jump_to_change(&id, cx)))
                .child(summary)
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .gap(px(2.))
                        .font_family(t.mono.clone())
                        .text_size(px(TEXT_XS))
                        .text_color(t.text3)
                        .group_hover(group, |style| style.text_color(t.accent_strong))
                        .child(node.id.clone())
                        .child(icon(IconName::ChevronRight, 11., t.text3)),
                );
        } else {
            head = head.child(summary);
        }
        head.into_any_element()
    }

    fn render_item(
        &self,
        item: &ConflictItem,
        snapshot: &RepoSnapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let node = item
            .node_id
            .as_deref()
            .and_then(|id| find_node(snapshot, id));
        let mut row = v_flex()
            .gap(px(8.))
            .py(px(12.))
            .border_t_1()
            .border_color(t.border2)
            .child(self.render_head(item, snapshot, cx));
        match item.kind {
            ConflictKind::File => {
                let tool = snapshot.resolve_tool.as_deref().map(merge_tool_label);
                let resolvable = can_resolve(snapshot, item.node_id.as_deref());
                // Taking a side needs no merge tool, so it can render when
                // Resolve cannot.
                let takeable = can_take_side(snapshot, item.node_id.as_deref());
                let resolving = self.app.read(cx).ui().resolving_conflict.clone();
                let busy = resolving.is_some() || self.taking.is_some();
                let mut paths = v_flex()
                    .gap(px(2.))
                    .pl(px(INSET))
                    .font_family(t.mono.clone())
                    .text_size(px(TEXT_S));
                for (index, path) in item.paths.iter().enumerate() {
                    let (dir, name) = split_path(path);
                    let waiting = resolving.as_ref().is_some_and(|(change, file)| {
                        Some(change) == item.node_id.as_ref() && file == path
                    });
                    let menu_node = item.node_id.clone();
                    let menu_path = path.clone();
                    let mut line = h_flex()
                        .id(SharedString::from(format!(
                            "conflict-path-{}-{index}",
                            item.id
                        )))
                        .gap(px(8.))
                        .min_w_0()
                        .min_h(px(26.))
                        .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            app(cx).update(cx, |model, cx| {
                                model.open_conflict_menu(
                                    menu_node.as_deref(),
                                    &menu_path,
                                    event.position,
                                    cx,
                                )
                            });
                        })
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(Theme::fade(t.danger, 0.7))
                                .child("×"),
                        )
                        .child(
                            h_flex()
                                .min_w(px(40.))
                                .flex_shrink()
                                .overflow_hidden()
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_color(t.text3)
                                        .child(dir.to_owned()),
                                )
                                .child(
                                    div()
                                        .min_w(px(20.))
                                        .flex_shrink()
                                        .text_color(t.text1)
                                        .child(truncated(name.to_owned())),
                                ),
                        )
                        .child(div().flex_1());
                    let Some(change) = node.map(|node| node.id.clone()) else {
                        paths = paths.child(line);
                        continue;
                    };
                    // Per-path verbs sit together on the row's right edge:
                    // the instant side-picks first, the tool hand-off last.
                    let mut verbs = h_flex()
                        .flex_shrink_0()
                        .gap(px(4.))
                        .font_family(crate::theme::FONT_UI);
                    if takeable {
                        for (side, label, tip) in [
                            (
                                ConflictSide::Ours,
                                "Take ours",
                                "Keep side #1 of the conflict — usually the rebase destination's \
                                 version (jj resolve --tool :ours)",
                            ),
                            (
                                ConflictSide::Theirs,
                                "Take theirs",
                                "Keep side #2 of the conflict — usually this change's own version \
                                 (jj resolve --tool :theirs)",
                            ),
                        ] {
                            let (item_id, change, path) =
                                (item.id.clone(), change.clone(), path.clone());
                            verbs = verbs.child(
                                btn(
                                    SharedString::from(format!("take-{}-{index}-{label}", item.id)),
                                    label,
                                )
                                .small()
                                .disabled(busy)
                                .tooltip(tip)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.take(
                                            item_id.clone(),
                                            change.clone(),
                                            path.clone(),
                                            side,
                                            cx,
                                        )
                                    },
                                )),
                            );
                        }
                    }
                    if let (true, Some(tool)) = (resolvable, tool) {
                        let (item_id, change, path) =
                            (item.id.clone(), change.clone(), path.clone());
                        verbs = verbs.child(
                            btn(
                                SharedString::from(format!("resolve-{}-{index}", item.id)),
                                if waiting {
                                    format!("Waiting for {tool}…")
                                } else {
                                    format!("Resolve in {tool}")
                                },
                            )
                            .small()
                            .armed(true)
                            .disabled(busy)
                            .tooltip(format!(
                                "Open the 3-way merge in {tool} and record the saved result"
                            ))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.resolve(item_id.clone(), change.clone(), path.clone(), cx)
                                },
                            )),
                        );
                    }
                    line = line.child(verbs);
                    paths = paths.child(line);
                }
                if item.more_paths > 0 {
                    paths = paths.child(
                        div()
                            .min_h(px(26.))
                            .flex()
                            .items_center()
                            .text_color(t.text3)
                            .child(format!("+ {} more conflicted files", item.more_paths)),
                    );
                }
                row = row.child(paths);
            }
            ConflictKind::Bookmark => {
                // Candidates render with their change's title so picking
                // the right target is a read, not a lookup. Undrawn
                // candidates stay inert.
                let mut targets = h_flex().gap(px(8.)).pl(px(INSET)).overflow_hidden().child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(TEXT_XS))
                        .text_color(t.text3)
                        .child("Could point to"),
                );
                for (index, id) in item.targets.iter().enumerate() {
                    let target = find_node(snapshot, id);
                    let mut chip = h_flex()
                        .id(SharedString::from(format!("target-{}-{index}", item.id)))
                        .gap(px(6.))
                        .min_w_0()
                        .max_w(px(300.))
                        .h(px(22.))
                        .px(px(7.))
                        .rounded(px(RADIUS_S))
                        .bg(Theme::fade(t.text1, 0.06))
                        .text_size(px(TEXT_XS))
                        .text_color(t.text2)
                        .child(
                            div()
                                .flex_shrink_0()
                                .font_family(t.mono.clone())
                                .child(id.clone()),
                        );
                    match target {
                        Some(target) => {
                            let title = summary_of(&target.description);
                            if !title.is_empty() {
                                chip = chip.child(
                                    div()
                                        .min_w(px(20.))
                                        .flex_shrink()
                                        .child(truncated(title.to_owned())),
                                );
                            }
                            let (app, id) = (self.app.clone(), id.clone());
                            chip = chip
                                .cursor_pointer()
                                .hover(|style| style.bg(t.accent_dim).text_color(t.accent_strong))
                                .tooltip(tooltip(format!("Go to {id}")))
                                .on_click(move |_, _, cx| {
                                    app.update(cx, |model, cx| model.jump_to_change(&id, cx))
                                });
                        }
                        None => chip = chip.opacity(0.7),
                    }
                    targets = targets.child(chip);
                }
                row = row.child(targets);
            }
            ConflictKind::StaleWorkspace => {
                // The recovery only reaches the workspace Jiji has open;
                // sibling cards explain the CLI route.
                let note = div().text_size(px(TEXT_XS)).text_color(t.text3);
                if can_recover_workspace(snapshot, item) {
                    let item_id = item.id.clone();
                    row = row.child(
                        h_flex()
                            .gap(px(12.))
                            .pl(px(INSET))
                            .min_h(px(26.))
                            .child(note.flex_1().min_w_0().child(
                                "Local edits are saved first, then the working copy catches up \
                                 to where the repo moved.",
                            ))
                            .child(
                                btn(
                                    SharedString::from(format!("update-{}", item.id)),
                                    if self.updating {
                                        "Updating…"
                                    } else {
                                        "Update workspace"
                                    },
                                )
                                .small()
                                .armed(true)
                                .disabled(self.updating)
                                .tooltip(
                                    "Record on-disk edits, then check out the repo's current \
                                     position (jj workspace update-stale)",
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.update_workspace(item_id.clone(), cx)
                                    },
                                )),
                            ),
                    );
                } else {
                    row = row.child(note.pl(px(INSET)).child(crate::ui::rich(
                        "Jiji can only update the workspace it has open — run **jj workspace \
                         update-stale** inside this one.",
                        cx,
                    )));
                }
            }
        }
        if let Some((_, message)) = self.error.as_ref().filter(|(of, _)| *of == item.id) {
            row = row.child(
                div()
                    .pl(px(INSET))
                    .text_size(px(TEXT_XS))
                    .text_color(t.danger)
                    .child(message.clone()),
            );
        }
        row.into_any_element()
    }
}

impl Render for ConflictsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let Some(snapshot) = self.app.read(cx).snapshot().cloned() else {
            return div().into_any_element();
        };
        let root = div()
            .id("conflicts")
            .key_context("Conflicts")
            .track_focus(&self.focus)
            .relative()
            .size_full();
        let groups = group_conflicts(&snapshot);
        if groups.is_empty() {
            return root
                .child(empty_state(
                    IconName::Conflicts,
                    "No conflicts",
                    "Rewrites and syncs that collide land here as first-class items instead of \
                     blocking your work. Nothing needs attention right now.",
                    Some("everything applies cleanly".into()),
                    cx,
                ))
                .into_any_element();
        }
        // Block flow: see the Operations page's column.
        let mut column = div()
            .w_full()
            .max_w(px(800.))
            .mx_auto()
            .pt(px(24.))
            .px(px(24.))
            .pb(px(32.))
            .child(section_header(
                IconName::Conflicts,
                "Conflicts",
                "Everything that needs attention, in plain language. jj never blocks on a \
                 conflict: rewrites and syncs always complete, what collided is recorded \
                 first-class, and other work can continue in the meantime.",
                cx,
            ));
        for group in &groups {
            // The group's name stays in view while its items scroll by.
            let heading = h_flex()
                .gap(px(8.))
                .pt(px(12.))
                .pb(px(4.))
                .bg(t.bg1)
                .child(
                    div()
                        .text_size(px(TEXT_S))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.text2)
                        .child(group.key.title()),
                )
                .child(
                    div()
                        .font_family(t.mono.clone())
                        .text_size(px(TEXT_XS))
                        .text_color(t.text3)
                        .child(group.items.len().to_string()),
                );
            let mut section = div().child(
                div()
                    .pb(px(8.))
                    .text_size(px(TEXT_S))
                    .text_color(t.text3)
                    .child(group.key.blurb()),
            );
            for item in &group.items {
                section = section.child(self.render_item(item, &snapshot, cx));
            }
            column = column.child(div().mb(px(24.)).child(pinned(
                heading,
                section.child(div().h(px(1.)).bg(t.border2)),
            )));
        }
        root.child(
            div()
                .id("conflicts-scroll")
                .size_full()
                .scroll_y()
                .track_scroll(&self.scroll)
                .child(column),
        )
        .child(scrollbar("conflicts-scrollbar", &self.scroll))
        .into_any_element()
    }
}
