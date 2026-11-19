//! The trays that open under the header: the describe editor, the
//! divergence notice, compare, squash and abandon confirmations, rebase,
//! and bookmark create and manage.

use gpui::{div, prelude::*, px, AnyElement, Context, FontWeight, Hsla, Role, Window};
use jiji_core::snapshot::{BookmarkState, GraphNode, NodeKind, RepoSnapshot};

use super::{kind_glyph, title_or_none, Confirm};
use crate::logic::inspect::{
    bookmarks_at, combined_description, descendants_of, divergent_siblings, find_node,
    move_direction, rebase_destinations, resolve_compare_from, sync_label, CompareMode,
    MoveDirection,
};
use crate::logic::text::{counted, short_id};
use crate::theme::{theme, Theme, RADIUS_M, RADIUS_S, TEXT_M, TEXT_S, TEXT_XS};
use crate::ui::ScrollY as _;
use crate::ui::{
    btn, h_flex, icon, input::field, rich, segmented, tooltip, truncated, trunk_label, v_flex,
    IconName, NodeExt as _, Segment, Tabbable as _,
};
use crate::views::workbench::WorkbenchView;

impl WorkbenchView {
    pub(super) fn render_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let saving = self.header.acting;
        let mut hint = h_flex()
            .flex_1()
            .min_w_0()
            .gap(px(4.))
            .text_size(px(TEXT_XS))
            .text_color(t.text3);
        if self.header.draft_restored {
            hint = hint.child("Unsaved draft restored ·").child(
                div()
                    .id("discard-draft")
                    .role(Role::Button)
                    .aria_label("Discard draft")
                    .tabbable(cx)
                    .text_color(t.accent_strong)
                    .cursor_pointer()
                    .hover(|style| style.text_color(t.text1))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.discard_editor_draft(window, cx)),
                    )
                    .child("Discard"),
            );
        } else {
            hint = hint.child("First line becomes the summary · ⌘↵ to save");
        }
        let mut row = h_flex().mt(px(8.)).gap(px(8.)).child(hint);
        if let Some(error) = &self.header.error {
            row = row.child(
                div()
                    .id("editor-error")
                    .alert(error.clone())
                    .min_w_0()
                    .max_w(px(360.))
                    .text_size(px(TEXT_XS))
                    .text_color(t.danger)
                    .child(truncated(error.clone())),
            );
        }
        row = row
            .child(
                btn("editor-cancel", "Cancel")
                    .disabled(saving)
                    .tooltip("Close, keeping the draft for later (esc)")
                    .on_click(cx.listener(|this, _, window, cx| this.close_tray(window, cx))),
            )
            .child(
                btn("editor-save", if saving { "Saving…" } else { "Save" })
                    .primary()
                    .disabled(saving)
                    .on_click(cx.listener(|this, _, window, cx| this.save_description(window, cx))),
            );
        let focused = self.header.editor.read(cx).is_focused(window);
        self.tray(false, "Describe this change", cx)
            .child(
                div()
                    .flex()
                    .min_h(px(84.))
                    .px(px(12.))
                    .py(px(8.))
                    .rounded(px(RADIUS_M))
                    .border_1()
                    .border_color(if focused { t.accent } else { t.border1 })
                    .bg(t.bg1)
                    .text_size(px(TEXT_M))
                    .line_height(px(20.))
                    .when(saving, |el| el.opacity(0.6))
                    .child(self.header.editor.clone()),
            )
            .child(row)
            .into_any_element()
    }

    pub(super) fn render_divergence(
        &mut self,
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let t = theme(cx);
        let siblings = divergent_siblings(snapshot, node);
        let acting = self.header.acting;
        let mut note = h_flex()
            .flex_wrap()
            .gap_x(px(8.))
            .gap_y(px(4.))
            .px(px(16.))
            .py(px(6.))
            .border_t_1()
            .border_color(t.border2)
            .bg(Theme::fade(t.danger, 0.06))
            .text_size(px(TEXT_S))
            .text_color(t.text2)
            .child(div().child(rich(
                &format!(
                    "Divergent change — {} visible commits share **{}**, so each copy goes by its commit id. Keep one and abandon the other{} to resolve.",
                    siblings.len() + 1,
                    node.change_id,
                    if siblings.len() == 1 { "" } else { "s" }
                ),
                cx,
            )));
        let small = |id: gpui::ElementId, label: &'static str, name: String, color: Hsla| {
            div()
                .id(id)
                .role(Role::Button)
                .aria_label(name)
                .tabbable_in(t.accent)
                .h(px(18.))
                .px(px(5.))
                .rounded(px(RADIUS_S))
                .text_size(px(TEXT_XS))
                .text_color(color)
                .cursor_pointer()
                .hover(|style| style.bg(t.bg_hover))
                .child(label)
        };
        for (ix, sibling) in siblings.iter().enumerate() {
            let commit = short_id(&sibling.commit_id, 8).to_owned();
            let (jump_id, compare_id, confirm_id) =
                (sibling.id.clone(), sibling.id.clone(), sibling.id.clone());
            let comparing = self.compare == CompareMode::Change(sibling.id.clone());
            let mut group = h_flex().gap(px(2.)).child(
                div()
                    .id(("copy", ix))
                    .role(Role::Button)
                    .aria_label(format!("Go to copy {commit}"))
                    .tabbable(cx)
                    .h(px(18.))
                    .px(px(5.))
                    .rounded(px(RADIUS_S))
                    .border_1()
                    .border_color(t.border1)
                    .font_family(t.mono.clone())
                    .text_size(px(TEXT_XS))
                    .cursor_pointer()
                    .hover(|style| style.bg(t.bg_hover))
                    .on_click(cx.listener(move |this, _, _, cx| this.jump_to(jump_id.clone(), cx)))
                    .child(commit.clone()),
            );
            group = group.child(
                small(
                    ("copy-compare", ix).into(),
                    "Compare",
                    format!("Compare with copy {commit}"),
                    if comparing { t.accent_strong } else { t.text2 },
                )
                .tooltip(tooltip(format!(
                    "Everything that differs between copy {commit} and this one"
                )))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_compare(CompareMode::Change(compare_id.clone()), cx)
                })),
            );
            if sibling.kind != NodeKind::Immutable && !acting {
                group = group.child(
                    small(
                        ("copy-abandon", ix).into(),
                        "Abandon",
                        format!("Abandon copy {commit}"),
                        t.danger,
                    )
                    .aria_expanded(self.header.diverge_confirm.as_ref() == Some(&sibling.id))
                    .tooltip(tooltip(format!("Abandon copy {commit} and keep this one")))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.header.diverge_confirm =
                            if this.header.diverge_confirm.as_ref() == Some(&confirm_id) {
                                None
                            } else {
                                Some(confirm_id.clone())
                            };
                        cx.notify();
                    })),
                );
            }
            note = note.child(group);
        }
        if let Some(error) = &self.header.diverge_error {
            note = note.child(
                div()
                    .id("diverge-error")
                    .alert(error.clone())
                    .min_w_0()
                    .text_color(t.danger)
                    .child(truncated(error.clone())),
            );
        }
        let mut out = vec![note.into_any_element()];
        let confirming = self.header.diverge_confirm.clone();
        if let Some(sibling) = siblings
            .iter()
            .find(|sibling| Some(&sibling.id) == confirming.as_ref())
        {
            let id = sibling.id.clone();
            let text = format!(
                "Abandon copy **{}** \u{201c}{}\u{201d} and keep this one. Bookmarks on it are deleted and its descendants rebase onto its parents{}. Undoable from the op log.",
                short_id(&sibling.commit_id, 8),
                title_or_none(&sibling.description),
                if siblings.len() == 1 {
                    format!("; {} stops being divergent", node.change_id)
                } else {
                    String::new()
                }
            );
            out.push(
                h_flex()
                    .gap(px(8.))
                    .px(px(16.))
                    .py(px(10.))
                    .border_t_1()
                    .border_color(t.border2)
                    .bg(t.bg_well)
                    .text_size(px(TEXT_S))
                    .text_color(t.text2)
                    .child(div().flex_1().min_w_0().child(rich(&text, cx)))
                    .child(
                        btn("copy-cancel", "Cancel")
                            .disabled(acting)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.header.diverge_confirm = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        btn(
                            "copy-confirm",
                            if acting {
                                "Abandoning…"
                            } else {
                                "Abandon copy"
                            },
                        )
                        .danger_solid()
                        .disabled(acting)
                        .on_click(cx.listener(
                            move |this, _, window, cx| this.abandon_copy(id.clone(), window, cx),
                        )),
                    )
                    .into_any_element(),
            );
        }
        out
    }

    pub(super) fn render_compare_tray(
        &mut self,
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let from = resolve_compare_from(snapshot, &node.id, &self.compare);
        let trunk_from = resolve_compare_from(snapshot, &node.id, &CompareMode::Trunk);
        let base_from = resolve_compare_from(snapshot, &node.id, &CompareMode::Base);
        let preset = |id: &'static str,
                      glyph: &'static str,
                      glyph_color: Hsla,
                      name: &'static str,
                      note: String,
                      selected: bool| {
            let mut row = h_flex()
                .id(id)
                .role(Role::ListBoxOption)
                .aria_label(name)
                .aria_description(note.clone())
                .aria_selected(selected)
                .tabbable_in(t.accent)
                .w_full()
                .h(px(26.))
                .gap(px(8.))
                .px(px(8.))
                .rounded(px(RADIUS_S))
                .text_size(px(TEXT_S))
                .cursor_pointer()
                .child(
                    div()
                        .w(px(12.))
                        .flex_shrink_0()
                        .font_family(t.mono.clone())
                        .text_color(glyph_color)
                        .child(glyph),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.text1)
                        .child(name),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(t.text3)
                        .child(truncated(note)),
                );
            row = if selected {
                row.bg(t.accent_dim)
            } else {
                row.hover(|style| style.bg(t.bg_hover))
            };
            row
        };
        let pick = |mode: CompareMode| {
            move |this: &mut Self,
                  _: &gpui::ClickEvent,
                  window: &mut Window,
                  cx: &mut Context<Self>| {
                this.set_compare(mode.clone(), cx);
                this.close_tray(window, cx);
            }
        };
        let mut presets = v_flex()
            .id("compare-presets")
            .role(Role::ListBox)
            .aria_label("Comparison preset")
            .mt(px(8.))
            .p(px(2.))
            .rounded(px(RADIUS_M))
            .border_1()
            .border_color(t.border2)
            .bg(t.bg1)
            .child(
                preset(
                    "compare-parent",
                    "·",
                    t.text3,
                    "Parent",
                    "what this change itself did".into(),
                    from.is_none(),
                )
                .on_click(cx.listener(pick(CompareMode::Parent))),
            );
        if let Some(trunk) = &trunk_from {
            presets = presets.child(
                preset(
                    "compare-trunk",
                    "◆",
                    t.text3,
                    "Trunk",
                    format!("everything between {} and here", short_id(trunk, 4)),
                    !matches!(self.compare, CompareMode::Change(_)) && from.as_ref() == Some(trunk),
                )
                .child(trunk_label(snapshot.trunk_bookmark.clone(), cx))
                .on_click(cx.listener(pick(CompareMode::Trunk))),
            );
        }
        if let Some(base) = base_from
            .as_ref()
            .filter(|base| Some(*base) != trunk_from.as_ref())
        {
            let kind = find_node(snapshot, base).map_or(NodeKind::Mutable, |n| n.kind);
            presets = presets.child(
                preset(
                    "compare-base",
                    kind_glyph(kind),
                    t.text2,
                    "Stack base",
                    format!(
                        "everything this stack changes up to here, from {}",
                        short_id(base, 4)
                    ),
                    self.compare == CompareMode::Base && from.as_ref() == Some(base),
                )
                .on_click(cx.listener(pick(CompareMode::Base))),
            );
        }
        let filter = self.dest_filter(window, cx);
        let list = self.dest_list("Comparison base change", "No matching change", window, cx);
        let close = self.cancel_button("Close", cx);
        self.tray(false, "Compare this change against another point", cx)
            .child(self.tray_title(
                &format!("Compare **{}**", short_id(&node.id, 4)),
                "pick what the diff is measured against — read-only, nothing is rewritten".into(),
                cx,
            ))
            .child(presets)
            .child(self.label("Or against any change", cx))
            .children(filter.map(|filter| filter.mb(px(4.))))
            .child(list)
            .child(self.confirm_row(
                "⌘-click a graph row also compares against it · presets follow the selection; a picked change ends when the selection moves".into(),
                vec![close],
                cx,
            ))
            .into_any_element()
    }

    pub(super) fn working_copy_line(
        &self,
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        own: &'static str,
    ) -> Option<String> {
        if node.id == snapshot.working_copy {
            Some(own.to_owned())
        } else if descendants_of(snapshot, &node.id)
            .iter()
            .any(|d| d.id == snapshot.working_copy)
        {
            Some("The working copy follows the rebase.".to_owned())
        } else {
            None
        }
    }

    pub(super) fn render_confirm_tray(
        &mut self,
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        confirm: Confirm,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let t = theme(cx);
        let acting = self.header.acting;
        let marks = bookmarks_at(snapshot, &node.id);
        let names = marks
            .iter()
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let descendants = descendants_of(snapshot, &node.id).len();
        let mut items: Vec<(String, bool)> = Vec::new();
        let rebase_line = |onto: &str| {
            format!(
                "{descendants} descendant change{} onto {onto}.",
                if descendants == 1 {
                    " rebases"
                } else {
                    "s rebase"
                }
            )
        };
        let (title, go, danger) = match confirm {
            Confirm::Squash => {
                let parent = match node.parents.as_slice() {
                    [only] => find_node(snapshot, only)?,
                    _ => return None,
                };
                items.push((
                    "Everything this change touches moves into the parent; the change itself is abandoned."
                        .into(),
                    false,
                ));
                if !marks.is_empty() {
                    items.push((
                        format!(
                            "Bookmark{} {names} move{} to the parent.",
                            if marks.len() == 1 { "" } else { "s" },
                            if marks.len() == 1 { "s" } else { "" }
                        ),
                        false,
                    ));
                }
                if descendants > 0 {
                    items.push((rebase_line("the parent"), false));
                }
                items.extend(
                    self.working_copy_line(
                        snapshot,
                        node,
                        "The working copy restarts as a new empty change on the parent.",
                    )
                    .map(|line| (line, false)),
                );
                (
                    self.tray_title(
                        &format!(
                            "Squash **{}** into its parent **{}**",
                            short_id(&node.id, 4),
                            short_id(&parent.id, 4)
                        ),
                        format!("\u{201c}{}\u{201d}", title_or_none(&parent.description)),
                        cx,
                    ),
                    btn("confirm-go", if acting { "Squashing…" } else { "Squash" }).primary(),
                    false,
                )
            }
            Confirm::Abandon => {
                items.push((
                    "Its changes disappear from the graph — the operation log can still restore them."
                        .into(),
                    false,
                ));
                if !marks.is_empty() {
                    items.push((
                        format!(
                            "Bookmark{} {names} will be deleted.",
                            if marks.len() == 1 { "" } else { "s" }
                        ),
                        false,
                    ));
                }
                if descendants > 0 {
                    items.push((rebase_line("its parent"), false));
                }
                items.extend(
                    self.working_copy_line(
                        snapshot,
                        node,
                        "The working copy restarts as a new empty change on its parent.",
                    )
                    .map(|line| (line, false)),
                );
                let siblings = divergent_siblings(snapshot, node);
                match siblings.as_slice() {
                    [] => {}
                    [only] => items.push((
                        format!(
                            "Only commit **{}** remains for **{}** — the divergence resolves.",
                            short_id(&only.commit_id, 8),
                            node.change_id
                        ),
                        false,
                    )),
                    many => items.push((
                        format!(
                            "{} other copies of **{}** stay divergent.",
                            many.len(),
                            node.change_id
                        ),
                        false,
                    )),
                }
                (
                    self.tray_title(
                        &format!("Abandon **{}**", short_id(&node.id, 4)),
                        format!("\u{201c}{}\u{201d}", title_or_none(&node.description)),
                        cx,
                    ),
                    btn(
                        "confirm-go",
                        if acting {
                            "Abandoning…"
                        } else {
                            "Abandon change"
                        },
                    )
                    .danger_solid(),
                    true,
                )
            }
        };
        let mut tray = self
            .tray(
                danger,
                match confirm {
                    Confirm::Squash => "Confirm squash",
                    Confirm::Abandon => "Confirm abandon",
                },
                cx,
            )
            .child(title)
            .child(self.consequences(items, cx));
        if confirm == Confirm::Squash {
            let parent = find_node(snapshot, &node.parents[0])?;
            let combined = combined_description(&parent.description, &node.description);
            let empty = combined.is_empty();
            tray = tray.child(self.label("Resulting description", cx)).child(
                div()
                    .id("squash-description")
                    .role(Role::Label)
                    .aria_label("Resulting description")
                    .aria_value(combined.clone())
                    .px(px(12.))
                    .py(px(8.))
                    .max_h(px(120.))
                    .scroll_y()
                    .rounded(px(RADIUS_M))
                    .border_1()
                    .border_color(t.border2)
                    .bg(t.bg1)
                    .text_size(px(TEXT_S))
                    .line_height(gpui::relative(1.5))
                    .text_color(if empty { t.text3 } else { t.text2 })
                    .when(empty, |el| el.italic())
                    .child(if empty {
                        "(no description — describe it afterwards)".to_owned()
                    } else {
                        combined
                    }),
            );
        }
        let buttons = vec![
            self.cancel_button("Cancel", cx),
            go.disabled(acting)
                .on_click(
                    cx.listener(move |this, _, window, cx| this.run_confirmed(confirm, window, cx)),
                )
                .into_any_element(),
        ];
        Some(
            tray.child(self.confirm_row("⌘↵ to confirm".into(), buttons, cx))
                .into_any_element(),
        )
    }

    pub(super) fn render_rebase_tray(
        &mut self,
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let acting = self.header.acting;
        let alone = self.header.rebase_alone;
        let descendants = descendants_of(snapshot, &node.id);
        let count = descendants.len();
        let wc_above = descendants.iter().any(|d| d.id == snapshot.working_copy);
        let dest = self
            .header
            .dest
            .as_deref()
            .and_then(|id| find_node(snapshot, id));
        let mut controls = h_flex().mt(px(8.)).gap(px(8.)).flex_wrap();
        if count > 0 {
            let this = cx.entity().downgrade();
            let id = node.id.clone();
            controls = controls.child(segmented(
                "rebase-mode",
                "Rebase scope",
                vec![
                    Segment::text(false, format!("With {}", counted(count, "descendant"))),
                    Segment::text(true, "Only this change"),
                ],
                &alone,
                cx,
                move |alone, _, cx| {
                    let alone = *alone;
                    let _ = this.update(cx, |this, cx| {
                        this.header.rebase_alone = alone;
                        // A destination the other mode excludes cannot stay
                        // picked.
                        let still = this.header.dest.as_ref().is_some_and(|dest| {
                            rebase_destinations(&this.snapshot, &id, !alone)
                                .iter()
                                .any(|candidate| &candidate.id == dest)
                        });
                        if !still {
                            this.header.dest = None;
                        }
                        cx.notify();
                    });
                },
            ));
        }
        controls = controls.children(self.dest_filter(window, cx));

        let mut items: Vec<(String, bool)> = Vec::new();
        if let Some(dest) = dest {
            let onto = format!(
                "**{}** ~~\u{201c}{}\u{201d}~~",
                short_id(&dest.id, 4),
                title_or_none(&dest.description)
            );
            if alone {
                items.push((format!("Only this change moves onto {onto}."), false));
                if count > 0 {
                    let parents = node
                        .parents
                        .iter()
                        .map(|p| short_id(p, 4))
                        .collect::<Vec<_>>()
                        .join(", ");
                    items.push((
                        format!(
                            "{count} descendant change{} behind, reparented onto **{parents}**.",
                            if count == 1 { " stays" } else { "s stay" }
                        ),
                        false,
                    ));
                }
            } else if count > 0 {
                items.push((
                    format!(
                        "This change and its {} move onto {onto}.",
                        counted(count, "descendant")
                    ),
                    false,
                ));
            } else {
                items.push((format!("This change moves onto {onto}."), false));
            }
            if node.parents.len() > 1 {
                items.push((
                    format!(
                        "Its {} parents are replaced by the destination — the merge is dissolved.",
                        node.parents.len()
                    ),
                    false,
                ));
            }
            if node.id == snapshot.working_copy {
                items.push(("The working copy moves with it.".into(), false));
            } else if wc_above && !alone {
                items.push(("The working copy follows the rebase.".into(), false));
            } else if wc_above {
                items.push((
                    "The working copy stays behind with the reparented descendants.".into(),
                    false,
                ));
            }
            items.push((
                "Changes that no longer apply cleanly become conflicts instead of stopping the rebase; the operation log can undo it."
                    .into(),
                false,
            ));
        }
        let has_dest = dest.is_some();
        let list = self.dest_list("Rebase destination", "No matching destination", window, cx);
        let buttons = vec![
            self.cancel_button("Cancel", cx),
            btn(
                "confirm-go",
                if acting {
                    "Rebasing…"
                } else if alone {
                    "Move here"
                } else {
                    "Rebase here"
                },
            )
            .primary()
            .disabled(acting || !has_dest)
            .on_click(cx.listener(|this, _, window, cx| this.submit_rebase(window, cx)))
            .into_any_element(),
        ];
        let mut tray = self
            .tray(false, "Rebase this change", cx)
            .child(self.tray_title(
                &format!("Rebase **{}**", short_id(&node.id, 4)),
                format!("\u{201c}{}\u{201d}", title_or_none(&node.description)),
                cx,
            ))
            .child(controls)
            .child(self.label("Destination — the new parent", cx))
            .child(list);
        if !items.is_empty() {
            tray = tray.child(self.consequences(items, cx));
        }
        tray.child(self.confirm_row(
            if has_dest {
                "↵ to confirm".into()
            } else {
                "Pick the destination this change moves onto — ↑↓ or click".into()
            },
            buttons,
            cx,
        ))
        .into_any_element()
    }

    pub(super) fn render_bookmark_tray(
        &mut self,
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let acting = self.header.acting;
        let empty = self.header.bookmark_name.read(cx).text().trim().is_empty();
        let mut tray = self
            .tray(false, "Bookmark this change", cx)
            .child(self.tray_title(
                &format!("Bookmark **{}**", short_id(&node.id, 4)),
                format!("\u{201c}{}\u{201d}", title_or_none(&node.description)),
                cx,
            ))
            .child(
                h_flex()
                    .mt(px(8.))
                    .gap(px(8.))
                    .child(
                        field(&self.header.bookmark_name, window, cx)
                            .w(px(280.))
                            .font_family(t.mono.clone()),
                    )
                    .child(
                        btn(
                            "bookmark-create",
                            if acting { "Working…" } else { "Create" },
                        )
                        .primary()
                        .disabled(acting || empty)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.submit_create_bookmark(window, cx)
                        })),
                    ),
            );
        let movable: Vec<&BookmarkState> = snapshot
            .bookmarks
            .iter()
            .filter(|b| b.is_local && b.target != node.id)
            .collect();
        if !movable.is_empty() {
            let mut list = v_flex()
                .max_h(px(160.))
                .id("move-bookmarks")
                .role(Role::List)
                .aria_label("Move an existing bookmark here")
                .scroll_y();
            for (ix, mark) in movable.into_iter().enumerate() {
                let mut notes = Vec::new();
                match move_direction(snapshot, &mark.target, &node.id) {
                    Some(MoveDirection::Backwards) => notes.push("moves backwards".to_owned()),
                    Some(MoveDirection::Sideways) => notes.push("moves sideways".to_owned()),
                    _ => {}
                }
                if let Some(remote) = &mark.remote {
                    notes.push(format!("{remote} updates on push"));
                }
                let mut note = format!("from {}", short_id(&mark.target, 4));
                for extra in notes {
                    note.push_str(" · ");
                    note.push_str(&extra);
                }
                let name = mark.name.clone();
                let mut row = h_flex()
                    .id(("move-bookmark", ix))
                    .role(Role::Button)
                    .aria_label(format!("Move {} here", mark.name))
                    .aria_description(note.clone())
                    .w_full()
                    .h(px(26.))
                    .gap(px(8.))
                    .px(px(8.))
                    .rounded(px(RADIUS_S))
                    .text_size(px(TEXT_S))
                    .text_color(t.text2)
                    .child(icon(IconName::Bookmark, 10., t.text3))
                    .child(
                        div()
                            .min_w_0()
                            .font_family(t.mono.clone())
                            .child(truncated(mark.name.clone())),
                    );
                if mark.is_trunk {
                    row = row.child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(TEXT_XS))
                            .text_color(t.accent_strong)
                            .child("trunk"),
                    );
                }
                row = row.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(TEXT_XS))
                        .text_color(t.text3)
                        .child(truncated(note)),
                );
                if acting {
                    row = row.opacity(0.6);
                } else {
                    row = row
                        .tabbable(cx)
                        .cursor_pointer()
                        .hover(|style| style.bg(t.bg_hover).text_color(t.text1))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let name = name.clone();
                            this.mutate_node(window, cx, move |backend, path, id| {
                                backend.move_bookmark(path, &name, id)
                            })
                        }));
                }
                list = list.child(row);
            }
            tray = tray
                .child(self.label("Or move an existing bookmark here", cx))
                .child(list);
        }
        let cancel = self.cancel_button("Cancel", cx);
        tray.child(self.confirm_row(
            "Bookmarks never rewrite changes; the operation log can restore them".into(),
            vec![cancel],
            cx,
        ))
        .into_any_element()
    }

    pub(super) fn render_manage_tray(
        &mut self,
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let t = theme(cx);
        let mark = bookmarks_at(snapshot, &node.id)
            .into_iter()
            .find(|mark| mark.name == name)?;
        let acting = self.header.acting;
        let draft = self.header.rename.read(cx).text().trim().to_owned();
        let context = match &mark.remote {
            Some(remote) => format!("tracked on {remote} — {}", sync_label(mark.sync).0),
            None => "local-only bookmark".to_owned(),
        };
        let hint = match &mark.remote {
            Some(remote) => format!(
                "Renaming starts a local-only bookmark; deleting removes it from {remote} on the next push"
            ),
            None => "Only exists locally; the operation log can restore it".to_owned(),
        };
        let delete_name = mark.name.clone();
        let buttons = vec![
            btn("bookmark-delete", "Delete")
                .danger()
                .icon(IconName::Trash)
                .disabled(acting)
                .on_click(cx.listener(move |this, _, window, cx| {
                    let name = delete_name.clone();
                    this.mutate_node(window, cx, move |backend, path, _| {
                        backend.delete_bookmark(path, &name)
                    })
                }))
                .into_any_element(),
            self.cancel_button("Close", cx),
        ];
        Some(
            self.tray(false, format!("Manage bookmark {}", mark.name), cx)
                .child(self.tray_title(&format!("**{}**", mark.name), context, cx))
                .child(
                    h_flex()
                        .mt(px(8.))
                        .gap(px(8.))
                        .child(
                            field(&self.header.rename, window, cx)
                                .w(px(280.))
                                .font_family(t.mono.clone()),
                        )
                        .child(
                            btn(
                                "bookmark-rename",
                                if acting { "Working…" } else { "Rename" },
                            )
                            .primary()
                            .disabled(acting || draft.is_empty() || draft == mark.name)
                            .on_click(cx.listener(
                                |this, _, window, cx| this.submit_rename_bookmark(window, cx),
                            )),
                        ),
                )
                .child(self.confirm_row(hint, buttons, cx))
                .into_any_element(),
        )
    }
}
