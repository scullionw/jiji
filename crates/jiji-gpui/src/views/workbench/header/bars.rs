//! The header's two bars: the title with its verbs, then the facts about
//! the change and the controls for how its diff is shown.

use gpui::{div, prelude::*, px, AnyElement, Context, Corner, FontWeight, Hsla, Window};
use jiji_core::snapshot::{BookmarkState, GraphNode, NodeKind, RepoSnapshot};

use super::{title_or_none, Confirm, Tray};
use crate::logic::graph::lane_of;
use crate::logic::inspect::{
    bookmarks_at, children_of, find_node, move_direction, resolve_compare_from, split_path,
    stack_position, summary_of, sync_label, ChangeActions, CompareMode, MoveDirection,
};
use crate::logic::pr::{pr_badge, pr_for_bookmark, PrTone};
use crate::logic::text::{counted, short_id};
use crate::theme::{theme, Theme, RADIUS_S, TEXT_HEAD, TEXT_M, TEXT_S, TEXT_XS, TOOLBAR_H};
use crate::ui::ScrollY as _;
use crate::ui::{
    btn, diffstat, from_now, h_flex, icon, icon_btn, label_box, middot, node_mark, popover,
    popover_layer, scrollbar, tag, text_width, tooltip, truncated, v_flex, IconName, Mark,
    Tabular as _, LABEL_H,
};
use crate::views::workbench::WorkbenchView;

/// The header gives way by the diff pane's width. At or under each of
/// these the meta bar drops, in turn: the parent and child jump links, the
/// stack note, then the commit id and author.
const RELATIONS_PANE: f32 = 1000.;
const STACK_NOTE_PANE: f32 = 880.;
const IDENTITY_PANE: f32 = 720.;
/// Panes wider than this always seat the verbs' labels; narrower ones only
/// while the title leaves room for them.
const VERB_LABELS_PANE: f32 = 880.;
/// The title bar's labelled verbs plus everything else that is not the
/// title: padding, gaps, the pencil, the close button.
const VERBS_AND_CHROME: f32 = 440.;
const META_BAR_H: f32 = 32.;
/// The change's title: the largest type on the workbench.
const TITLE_SIZE: f32 = TEXT_HEAD + 1.;

impl WorkbenchView {
    pub(super) fn render_title_bar(
        &mut self,
        node: &GraphNode,
        avail: &ChangeActions,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let editing = self.header.tray == Tray::Edit;
        let acting = self.header.acting;
        let title = summary_of(&node.description).to_owned();
        // The verbs keep their names while the title fits beside them; in
        // a pane too narrow for both, the title wins and the verbs fall
        // back to their icons.
        let labelled = self.diff_width > VERB_LABELS_PANE
            || text_width(&title, TITLE_SIZE, FontWeight::SEMIBOLD, window) + VERBS_AND_CHROME
                <= self.diff_width;
        let has_body = node
            .description
            .split_once('\n')
            .is_some_and(|(_, rest)| !rest.trim().is_empty());
        let has_draft = self
            .app
            .read(cx)
            .draft_for(&self.snapshot.repo_path, &node.id, &node.description)
            .is_some();
        let mut bar = h_flex()
            .h(px(TOOLBAR_H))
            .min_w_0()
            .gap(px(8.))
            .pl(px(13.))
            .pr(px(8.))
            .border_b_1()
            .border_color(t.border2);
        bar = bar.child(self.title_mark(node, cx));
        if title.is_empty() {
            let mut undescribed = h_flex()
                .min_w_0()
                .gap(px(8.))
                .text_size(px(TEXT_HEAD))
                .italic()
                .text_color(t.text3)
                .child("No description yet");
            if avail.describe {
                undescribed = undescribed.child(
                    div().not_italic().child(
                        btn(
                            "describe",
                            if has_draft {
                                "Resume draft"
                            } else {
                                "Describe"
                            },
                        )
                        .small()
                        .icon(IconName::Edit)
                        .on_click(cx.listener(|this, _, window, cx| this.open_editor(window, cx))),
                    ),
                );
            } else {
                undescribed = undescribed.child(
                    div()
                        .not_italic()
                        .font_family(t.mono.clone())
                        .text_size(px(TEXT_S))
                        .child("jj describe"),
                );
            }
            bar = bar.child(undescribed);
        } else {
            bar = bar.child(
                div()
                    .id("change-title")
                    .min_w_0()
                    .text_size(px(TITLE_SIZE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .tooltip(tooltip(title.clone()))
                    .child(truncated(title)),
            );
            if has_body {
                let open = self.header.body_open;
                bar = bar.child(
                    icon_btn(
                        "disclose-body",
                        if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        },
                    )
                    .size(22., 13.)
                    .tooltip(if open {
                        "Hide full description"
                    } else {
                        "Show full description"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.header.body_open = !this.header.body_open;
                        cx.notify();
                    })),
                );
            }
            if avail.describe && !editing {
                bar = bar.child(
                    icon_btn("edit-description", IconName::Edit)
                        .size(22., 12.)
                        .color(if has_draft { t.warn } else { t.text3 })
                        .tooltip(if has_draft {
                            "Edit description (unsaved draft)"
                        } else {
                            "Edit description"
                        })
                        .on_click(cx.listener(|this, _, window, cx| this.open_editor(window, cx))),
                );
            }
        }
        bar = bar.child(div().flex_1());
        if !editing {
            let action =
                |id: &'static str, label: &'static str, glyph: IconName, tip: &'static str| {
                    btn(id, if labelled { label } else { "" })
                        .ghost()
                        .icon(glyph)
                        .disabled(acting)
                        .tooltip(tip)
                };
            let mut actions = h_flex().flex_shrink_0().gap(px(1.)).child(
                action(
                    "action-new",
                    "New",
                    IconName::Plus,
                    "Start a new change on top of this one (jj new)",
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.mutate_node(window, cx, |backend, path, id| backend.new_change(path, id))
                })),
            );
            if avail.edit {
                actions = actions.child(
                    action(
                        "action-edit",
                        "Edit",
                        IconName::AtSign,
                        "Make this the working copy (jj edit)",
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.mutate_node(window, cx, |backend, path, id| {
                            backend.edit_change(path, id)
                        })
                    })),
                );
            }
            actions = actions
                .child(
                    action(
                        "action-bookmark",
                        "Bookmark",
                        IconName::Bookmark,
                        "Create or move a bookmark onto this change (jj bookmark)",
                    )
                    .on(self.header.tray == Tray::Bookmark)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_tray(Tray::Bookmark, window, cx)
                    })),
                )
                .child(self.render_more_menu(avail, labelled, cx));
            bar = bar.child(actions);
        }
        bar.child(
            icon_btn("close-change", IconName::Close)
                .size(24., 13.)
                .tooltip("Close (esc)")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.app.update(cx, |model, cx| model.select_node(None, cx));
                })),
        )
    }

    /// The change's own mark from the graph, filled in as it is there:
    /// the header names the station the map has selected.
    fn title_mark(&self, node: &GraphNode, cx: &Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let (kind, color) = match crate::ui::mark_for(&self.snapshot, &node.id, &t) {
            Some((Mark::Station { .. }, hue)) => (Mark::Station { filled: true }, hue),
            Some(mark) => mark,
            None => (Mark::Station { filled: true }, t.accent),
        };
        div()
            .flex_shrink_0()
            .w(px(14.))
            .flex()
            .justify_center()
            .child(node_mark(kind, color, 12.))
    }

    pub(super) fn render_more_menu(
        &mut self,
        avail: &ChangeActions,
        labelled: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let open = self.header.more.is_open();
        let acting = self.header.acting;
        let anchor = div().relative().child(
            btn("action-more", if labelled { "More" } else { "" })
                .ghost()
                .on(open)
                .disabled(acting)
                .tooltip("More change actions")
                .trailing(icon(IconName::ChevronDown, 12., t.text3))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.header.more.toggle();
                    cx.notify();
                })),
        );
        if !open {
            return anchor;
        }
        let item = |id: &'static str,
                    glyph: IconName,
                    label: &'static str,
                    command: &'static str,
                    armed: bool,
                    danger: bool| {
            let fg = if danger { t.danger } else { t.text1 };
            let hover = if danger { t.danger_dim } else { t.bg_hover };
            let mut row = h_flex()
                .id(id)
                .h(px(28.))
                .gap(px(8.))
                .px(px(8.))
                .rounded(px(RADIUS_S))
                .text_size(px(TEXT_M))
                .text_color(fg)
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .child(icon(glyph, 14., if danger { t.danger } else { t.text3 }))
                .child(div().flex_1().child(label))
                .child(
                    div()
                        .flex_shrink_0()
                        .font_family(t.mono.clone())
                        .text_size(px(TEXT_XS))
                        .text_color(t.text3)
                        .child(command),
                );
            if armed {
                row = row.bg(hover);
            }
            row
        };
        let tray = self.header.tray.clone();
        let evolution_open = self
            .app
            .read(cx)
            .ui()
            .evolution
            .as_deref()
            .is_some_and(|id| Some(id) == self.header.node.as_deref());
        let mut panel = popover(cx).w(px(248.)).p(px(4.)).flex().flex_col();
        let open_tray = |tray: Tray| {
            move |this: &mut Self,
                  _: &gpui::ClickEvent,
                  window: &mut Window,
                  cx: &mut Context<Self>| {
                this.header.more.close();
                this.toggle_tray(tray.clone(), window, cx);
            }
        };
        if avail.rebase {
            panel = panel.child(
                item(
                    "more-rebase",
                    IconName::Rebase,
                    "Rebase…",
                    "jj rebase",
                    tray == Tray::Rebase,
                    false,
                )
                .tooltip(tooltip(
                    "Move this change onto a different parent (jj rebase)",
                ))
                .on_click(cx.listener(open_tray(Tray::Rebase))),
            );
        }
        if avail.split {
            panel = panel.child(
                item("more-split", IconName::Split, "Split…", "jj split", tray == Tray::Split, false)
                    .tooltip(tooltip(
                        "Split this change in two, or move files into another change (jj split / jj squash --into)",
                    ))
                    .on_click(cx.listener(open_tray(Tray::Split))),
            );
        }
        if avail.squash {
            panel = panel.child(
                item(
                    "more-squash",
                    IconName::Squash,
                    "Squash into parent…",
                    "jj squash",
                    tray == Tray::Confirm(Confirm::Squash),
                    false,
                )
                .tooltip(tooltip(
                    "Move this change's content into its parent (jj squash)",
                ))
                .on_click(cx.listener(open_tray(Tray::Confirm(Confirm::Squash)))),
            );
        }
        panel = panel.child(
            item("more-evolog", IconName::Operations, "Evolution", "jj evolog", evolution_open, false)
                .tooltip(tooltip(
                    "How this change was rewritten over time (jj evolog) — a scrubbable timeline of its versions",
                ))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.header.more.close();
                    let Some(id) = this.header.node.clone() else {
                        return;
                    };
                    this.app.update(cx, |model, cx| {
                        if evolution_open {
                            model.close_evolution(cx)
                        } else {
                            model.open_evolution(id, cx)
                        }
                    });
                    cx.notify();
                })),
        );
        if avail.abandon {
            panel = panel.child(crate::ui::separator(cx)).child(
                item(
                    "more-abandon",
                    IconName::Trash,
                    "Abandon…",
                    "jj abandon",
                    tray == Tray::Confirm(Confirm::Abandon),
                    true,
                )
                .tooltip(tooltip("Abandon this change (jj abandon)"))
                .on_click(cx.listener(open_tray(Tray::Confirm(Confirm::Abandon)))),
            );
        }
        anchor.child(popover_layer(
            Corner::TopRight,
            30.,
            panel,
            cx.listener(|this, _, _, cx| {
                this.header.more.close();
                cx.notify();
            }),
        ))
    }

    pub(super) fn compare_label(&self, snapshot: &RepoSnapshot, from: Option<&str>) -> String {
        match (from, &self.compare) {
            (None, _) => "vs parent".into(),
            (Some(_), CompareMode::Trunk) => format!(
                "vs {}",
                if snapshot.trunk_bookmark.is_empty() {
                    "trunk"
                } else {
                    &snapshot.trunk_bookmark
                }
            ),
            (Some(_), CompareMode::Base) => "vs stack base".into(),
            (Some(from), _) => format!("vs {}", short_id(from, 4)),
        }
    }

    pub(super) fn compare_note(&self, snapshot: &RepoSnapshot, node: &GraphNode) -> Option<String> {
        let from = resolve_compare_from(snapshot, &node.id, &self.compare)?;
        match move_direction(snapshot, &from, &node.id)? {
            MoveDirection::Backwards => Some(format!(
                "Comparing against a descendant — the diff reads in reverse: what going back to {} would undo.",
                short_id(&node.id, 4)
            )),
            MoveDirection::Sideways => Some(
                "Comparing across branches — everything that differs between the two trees.".into(),
            ),
            MoveDirection::Forward => None,
        }
    }

    pub(super) fn render_meta_bar(
        &mut self,
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let mono = t.mono.clone();
        // One line, always: the diff never shifts as the selection moves.
        // Facts leave in order of how much they are missed as the pane
        // narrows; whatever still overruns clips at the view controls.
        // A state tag (working copy, immutable) takes its room from the
        // facts after it, so they leave that much sooner.
        let tagged = if node.kind == NodeKind::Mutable {
            0.
        } else {
            96.
        };
        let pane = self.diff_width - tagged;
        let (relations, stack_note, identity) = (
            pane > RELATIONS_PANE,
            pane > STACK_NOTE_PANE,
            pane > IDENTITY_PANE,
        );
        // The change's line, when it rides one: its id and its stack note
        // wear the hue the graph draws it in.
        let line = stack_position(snapshot, &node.id)
            .map(|position| t.lane(lane_of(snapshot, &position.workstream.id).unwrap_or(0)))
            .filter(|_| node.kind != NodeKind::Immutable);
        // The facts read in groups — who it is, who made it when, what
        // state it is in, where it sits — a quiet dot between groups.
        let mut who: Vec<AnyElement> = Vec::new();
        let mut when: Vec<AnyElement> = Vec::new();
        let mut state: Vec<AnyElement> = Vec::new();
        let mut place: Vec<AnyElement> = Vec::new();
        if node.is_divergent {
            who.push(
                h_flex()
                    .id("fact-ids")
                    .flex_shrink_0()
                    .font_family(mono.clone())
                    .text_color(t.danger)
                    .tooltip(tooltip(
                        "Divergent change: several visible commits share this change id",
                    ))
                    .child(node.change_id.clone())
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("??"))
                    .into_any_element(),
            );
            // A divergent copy is told apart by its commit id, so this one
            // stays at every width.
            who.push(
                div()
                    .id("fact-commit")
                    .flex_shrink_0()
                    .font_family(mono.clone())
                    .tooltip(tooltip("This copy's commit id"))
                    .child(node.commit_id.clone())
                    .into_any_element(),
            );
        } else {
            who.push(
                h_flex()
                    .id("fact-ids")
                    .flex_shrink_0()
                    .font_family(mono.clone())
                    .text_color(t.text2)
                    .tooltip(tooltip("Change id"))
                    .child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .text_color(line.unwrap_or(t.text1))
                            .child(short_id(&node.id, 2).to_owned()),
                    )
                    .child(node.id.chars().skip(2).collect::<String>())
                    .into_any_element(),
            );
            if identity {
                who.push(
                    div()
                        .id("fact-commit")
                        .flex_shrink_0()
                        .font_family(mono.clone())
                        .tooltip(tooltip(format!("Commit id {}", node.commit_id)))
                        .child(short_id(&node.commit_id, 8).to_owned())
                        .into_any_element(),
                );
            }
        }
        match node.kind {
            NodeKind::WorkingCopy => {
                who.push(tag("working copy", t.working_copy).into_any_element())
            }
            NodeKind::Immutable => who.push(tag("immutable", t.text3).into_any_element()),
            NodeKind::Mutable => {}
        }
        if identity {
            when.push(
                h_flex()
                    .flex_shrink_0()
                    .gap(px(6.))
                    .child(crate::ui::monogram(
                        &node.author,
                        &crate::ui::initials(&node.author),
                        18.,
                        true,
                        cx,
                    ))
                    .child(
                        div()
                            .max_w(px(160.))
                            .text_color(t.text2)
                            .child(truncated(node.author.clone())),
                    )
                    .into_any_element(),
            );
        }
        when.push(
            div()
                .id("fact-age")
                .flex_shrink_0()
                .tooltip(tooltip(format!(
                    "{} · {}",
                    node.author,
                    crate::ui::absolute_time(&node.timestamp)
                )))
                .child(from_now(&node.timestamp))
                .into_any_element(),
        );
        if node.is_empty {
            state.push(div().flex_shrink_0().child("empty").into_any_element());
        }
        if node.has_conflict {
            state.push(
                label_box(t.danger, Theme::fade(t.danger, 0.42))
                    .gap(px(4.))
                    .child(icon(IconName::Conflicts, 11., t.danger))
                    .child("conflicts")
                    .into_any_element(),
            );
        }
        let prs = self.app.read(cx).forge.prs.clone();
        for (ix, mark) in bookmarks_at(snapshot, &node.id).into_iter().enumerate() {
            state.extend(self.bookmark_facts(mark, ix, prs.as_deref(), cx));
        }
        if let Some(position) = stack_position(snapshot, &node.id).filter(|_| stack_note) {
            let (at, of) = (position.index + 1, position.workstream.node_ids.len());
            let title = position.workstream.title.clone();
            // The workstream's lane hue, so the note reads as the lane it
            // names in the graph.
            let hue = t.lane(lane_of(snapshot, &position.workstream.id).unwrap_or(0));
            place.push(
                h_flex()
                    .id("fact-stack")
                    .flex_shrink_0()
                    .gap(px(5.))
                    .max_w(px(300.))
                    .tooltip(tooltip(format!("{at} of {of} in \u{201c}{title}\u{201d}")))
                    .child(crate::ui::station(hue, 9.))
                    .child(
                        div()
                            .flex_shrink_0()
                            .tabular()
                            .child(format!("{at} of {of} in")),
                    )
                    .child(div().min_w_0().text_color(t.text2).child(truncated(title)))
                    .into_any_element(),
            );
        }
        let relation = |key: usize, id: &str, role: &'static str, glyph: IconName| {
            let description = find_node(snapshot, id)
                .map(|n| summary_of(&n.description).to_owned())
                .filter(|title| !title.is_empty());
            let tip = match description {
                Some(description) => format!("Go to {role} {id} — {description}"),
                None => format!("Go to {role} {id}"),
            };
            let target = id.to_owned();
            h_flex()
                .id(("relation", key))
                .flex_shrink_0()
                .gap(px(2.))
                .h(px(20.))
                .px(px(4.))
                .rounded(px(RADIUS_S))
                .text_size(px(TEXT_XS))
                .cursor_pointer()
                .hover(|style| style.bg(t.bg_hover))
                .tooltip(tooltip(tip))
                .on_click(cx.listener(move |this, _, _, cx| this.jump_to(target.clone(), cx)))
                .child(icon(glyph, 10., t.text3))
                .child(
                    div()
                        .font_family(mono.clone())
                        .text_color(t.text2)
                        .child(short_id(id, 4).to_owned()),
                )
                .into_any_element()
        };
        if relations {
            let mut key = 0;
            for child in children_of(snapshot, &node.id) {
                place.push(relation(key, &child.id, "child", IconName::ArrowUp));
                key += 1;
            }
            for parent in &node.parents {
                place.push(relation(key, parent, "parent", IconName::ArrowDown));
                key += 1;
            }
        }

        let mut facts = h_flex().flex_1().min_w_0().overflow_hidden().gap(px(8.));
        let mut first = true;
        for group in [who, when, state, place] {
            if group.is_empty() {
                continue;
            }
            if !first {
                facts = facts.child(middot(cx));
            }
            first = false;
            facts = facts.children(group);
        }

        h_flex()
            .h(px(META_BAR_H))
            .min_w_0()
            .gap(px(8.))
            .pl(px(16.))
            .pr(px(8.))
            .text_size(px(TEXT_S))
            .text_color(t.text3)
            .child(facts)
            .child(self.render_view_controls(snapshot, node, cx))
    }

    /// A bookmark on this change: its chip (click to rename or delete a
    /// local one), then its pull request and the CI re-run when one
    /// applies.
    pub(super) fn bookmark_facts(
        &mut self,
        mark: &BookmarkState,
        key: usize,
        prs: Option<&jiji_forge::RepoPrState>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let t = theme(cx);
        let (sync_text, tone) = sync_label(mark.sync);
        let full = match &mark.remote {
            Some(remote) => format!("{}@{remote}", mark.name),
            None => mark.name.clone(),
        };
        let manageable = mark.is_local && !mark.is_trunk;
        let armed = self.header.tray == Tray::Manage(mark.name.clone());
        let (ink, line) = if mark.is_trunk {
            (t.accent_strong, Theme::fade(t.accent, 0.5))
        } else if armed {
            (t.text1, t.accent)
        } else {
            (t.text2, t.border1)
        };
        let mut chip = label_box(ink, line)
            .id(("bookmark-chip", key))
            .gap(px(5.))
            .font_family(t.mono.clone())
            .on_mouse_down(
                gpui::MouseButton::Right,
                crate::views::menu::bookmark_menu(mark.name.clone(), mark.remote.clone()),
            )
            .child(div().max_w(px(180.)).child(truncated(mark.name.clone())))
            .child(
                div()
                    .font_family(crate::theme::FONT_UI)
                    .text_color(t.tone(tone))
                    .child(sync_text),
            );
        chip = if manageable {
            let name = mark.name.clone();
            chip.cursor_pointer()
                .hover(|style| style.bg(t.bg_hover))
                .tooltip(tooltip(format!("{full} — {sync_text} · rename or delete")))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.toggle_tray(Tray::Manage(name.clone()), window, cx)
                }))
        } else {
            chip.tooltip(tooltip(format!("{full} — {sync_text}")))
        };
        let mut out = vec![chip.into_any_element()];
        let Some(pr) = pr_for_bookmark(mark, prs) else {
            return out;
        };
        // The bookmark's pull request: a one-click route to GitHub, wearing
        // the same state the graph badge shows.
        let badge = pr_badge(pr);
        let fg = match badge.tone {
            PrTone::Open => t.ok,
            PrTone::Draft => t.text3,
            PrTone::Merged => t.accent_strong,
            PrTone::Closed => t.danger,
        };
        let url = pr.url.clone();
        let mut pr_chip = label_box(fg, Theme::fade(fg, 0.42))
            .id(("pr-chip", key))
            .gap(px(4.))
            .cursor_pointer()
            .hover(|style| style.bg(t.bg_hover))
            .tooltip(tooltip(format!("{} · open on GitHub", badge.title)))
            .on_click(move |_, _, cx| cx.open_url(&url))
            .on_mouse_down(
                gpui::MouseButton::Right,
                crate::views::menu::pr_menu(pr.number),
            )
            .child(div().font_family(t.mono.clone()).child(badge.label.clone()))
            .child(badge.tone.word());
        for glyph in [badge.review, badge.checks].into_iter().flatten() {
            pr_chip = pr_chip.child(div().text_color(t.tone(glyph.tone)).child(glyph.glyph));
        }
        out.push(pr_chip.child("↗").into_any_element());
        if pr.state == jiji_forge::PrState::Open && pr.checks == jiji_forge::ChecksRollup::Failing {
            // A failing PR offers the way back to green: ask GitHub
            // Actions to re-run only the failed jobs.
            let number = pr.number;
            let mine = self
                .header
                .rerun
                .as_ref()
                .filter(|(asked, _, _)| *asked == number);
            let busy = mine.is_some_and(|(_, busy, _)| *busy);
            let label = match mine {
                Some((_, true, _)) => "Re-running…",
                Some(_) => "CI re-run requested",
                None => "Re-run failed CI",
            };
            let tip = mine
                .and_then(|(_, _, note)| note.clone())
                .unwrap_or_else(|| format!("Re-run the failed GitHub Actions jobs on #{number}"));
            let mut rerun = div()
                .id(("rerun-ci", key))
                .flex_shrink_0()
                .h(px(LABEL_H))
                .px(px(5.))
                .rounded(px(RADIUS_S))
                .text_size(px(TEXT_XS))
                .text_color(t.accent_strong)
                .tooltip(tooltip(tip))
                .child(label);
            if !busy {
                rerun = rerun
                    .cursor_pointer()
                    .hover(|style| style.bg(t.accent_dim))
                    .on_click(cx.listener(move |this, _, _, cx| this.rerun_ci(number, cx)));
            }
            out.push(rerun.into_any_element());
        }
        out
    }

    pub(super) fn render_view_controls(
        &mut self,
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let from = resolve_compare_from(snapshot, &node.id, &self.compare);
        let comparing = from.is_some();
        let tip = match &from {
            Some(from) => format!(
                "Comparing against {from} \u{201c}{}\u{201d} — click to change",
                find_node(snapshot, from).map_or("no description", |n| title_or_none(&n.description))
            ),
            None => "Compare against trunk, the stack base, or any change — or ⌘-click a row in the graph"
                .to_owned(),
        };
        let fg = if comparing { t.accent_strong } else { t.text2 };
        let mut compare = h_flex().flex_shrink_0().child({
            let mut chip = h_flex()
                .id("compare-chip")
                .gap(px(5.))
                .h(px(22.))
                .px(px(7.))
                .rounded(px(RADIUS_S))
                .text_color(fg)
                .cursor_pointer()
                .tooltip(tooltip(tip))
                .on_click(
                    cx.listener(|this, _, window, cx| this.toggle_tray(Tray::Compare, window, cx)),
                )
                .child(icon(
                    IconName::Compare,
                    12.,
                    if comparing { fg } else { t.text3 },
                ))
                .child(self.compare_label(snapshot, from.as_deref()));
            chip = if comparing {
                chip.bg(t.accent_dim)
            } else {
                if self.header.tray == Tray::Compare {
                    chip = chip.bg(t.bg_hover);
                }
                chip.hover(|style| style.bg(t.bg_hover).text_color(t.text1))
            };
            chip
        });
        if comparing {
            compare = compare.child(
                div()
                    .id("compare-reset")
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(18.))
                    .h(px(22.))
                    .rounded_r(px(RADIUS_S))
                    .bg(t.accent_dim)
                    .cursor_pointer()
                    .hover(|style| style.bg(Theme::fade(t.accent, 0.3)))
                    .tooltip(tooltip("Back to the parent diff"))
                    .on_click(
                        cx.listener(|this, _, _, cx| this.set_compare(CompareMode::Parent, cx)),
                    )
                    .child(icon(IconName::Close, 10., t.accent_strong)),
            );
        }
        h_flex()
            .flex_shrink_0()
            .gap(px(4.))
            .min_h(px(24.))
            .child(compare)
            .children(self.render_files_menu(cx))
            .child(self.tree_toggle(cx))
            .child(self.layout_toggle(cx))
    }

    /// "N files +a −r": the jump-to-a-file menu.
    pub(super) fn render_files_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = theme(cx);
        let files = self.surface.read(cx).files();
        self.diff.current()?;
        let count = files.len();
        let (added, removed) = files
            .iter()
            .fold((0, 0), |(a, r), file| (a + file.added, r + file.removed));
        let open = self.header.files.is_open();
        let stat = |sign: &str, n: u32, color: Hsla| {
            (n > 0).then(|| {
                div()
                    .font_family(t.mono.clone())
                    .text_color(color)
                    .child(format!("{sign}{n}"))
            })
        };
        let mut button = h_flex()
            .id("files-button")
            .gap(px(6.))
            .h(px(24.))
            .px(px(7.))
            .rounded(px(RADIUS_S))
            .text_color(t.text2)
            .tabular()
            .child(counted(count, "file"))
            .children(stat("+", added, t.add_fg))
            .children(stat("−", removed, t.del_fg))
            // The blocks are the first thing a narrow pane gives up: the
            // change's own facts need the room more.
            .when(
                added + removed > 0 && self.diff_width > IDENTITY_PANE,
                |el| el.child(diffstat(added, removed, cx)),
            );
        if count == 0 {
            button = button.opacity(0.6);
        } else {
            if open {
                button = button.bg(t.bg_hover);
            }
            button = button
                .cursor_pointer()
                .hover(|style| style.bg(t.bg_hover).text_color(t.text1))
                .tooltip(tooltip("Jump to a file"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.header.files.toggle();
                    cx.notify();
                }));
        }
        let mut anchor = div().relative().child(button);
        if open {
            let mut list = v_flex().p(px(4.));
            for (index, file) in files.iter().enumerate() {
                let (glyph, hue) = match file.status {
                    jiji_core::snapshot::FileStatus::Added => ("A", t.add_fg),
                    jiji_core::snapshot::FileStatus::Modified => ("M", t.text3),
                    jiji_core::snapshot::FileStatus::Removed => ("D", t.del_fg),
                    _ => ("R", t.accent_strong),
                };
                let (dir, name) = split_path(&file.path);
                list = list.child(
                    h_flex()
                        .id(("jump-file", index))
                        .w_full()
                        .h(px(26.))
                        .gap(px(8.))
                        .px(px(8.))
                        .rounded(px(RADIUS_S))
                        .font_family(t.mono.clone())
                        .text_size(px(TEXT_S))
                        .cursor_pointer()
                        .hover(|style| style.bg(t.bg_hover))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.header.files.close();
                            this.jump_to_file(index, cx);
                            cx.notify();
                        }))
                        .child(
                            div()
                                .w(px(12.))
                                .flex_shrink_0()
                                .text_size(px(TEXT_XS))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(hue)
                                .child(glyph),
                        )
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .min_w_0()
                                        .text_color(t.text3)
                                        .child(truncated(dir.to_owned())),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_color(t.text1)
                                        .child(name.to_owned()),
                                ),
                        )
                        .child(
                            h_flex()
                                .flex_shrink_0()
                                .gap(px(6.))
                                .text_size(px(TEXT_XS))
                                .children(stat("+", file.added, t.add_fg))
                                .children(stat("−", file.removed, t.del_fg)),
                        ),
                );
            }
            let panel = popover(cx)
                .relative()
                .w(px(420.))
                .child(
                    div()
                        .id("files-menu")
                        .max_h(px(360.))
                        .scroll_y()
                        .track_scroll(&self.header.files_scroll)
                        .child(list),
                )
                .child(scrollbar("files-menu-scrollbar", &self.header.files_scroll));
            anchor = anchor.child(popover_layer(
                Corner::TopRight,
                26.,
                panel,
                cx.listener(|this, _, _, cx| {
                    this.header.files.close();
                    cx.notify();
                }),
            ));
        }
        Some(anchor.into_any_element())
    }
}
