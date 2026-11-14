//! Nothing selected: the repo at a glance — its workstreams and bookmarks,
//! with inline bookmark rename and delete. A selection puts the diff
//! surface in this pane instead.

use gpui::{
    div, prelude::*, px, AnyElement, Context, Entity, FontWeight, MouseButton, ScrollHandle,
    SharedString, Window,
};
use jiji_core::snapshot::{BookmarkState, RepoSnapshot};

use super::WorkbenchView;
use crate::logic::bookmarks::bookmark_key;
use crate::logic::graph::resolve_focused_workstream;
use crate::logic::inspect::{find_node, sync_label};
use crate::theme::{theme, RADIUS_S, TEXT_M, TEXT_S, TEXT_TITLE, TEXT_XS};
use crate::ui::ScrollY as _;
use crate::ui::{
    btn, h_flex, icon, icon_btn, input::field, kbd, ref_label, scrollbar, tooltip, truncated,
    v_flex, IconName, InputEvent, TextInput,
};
use crate::views::menu;

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditMode {
    Rename,
    Delete,
}

pub(super) struct InspectorState {
    /// The bookmark row being edited: its key, its name, and how. One row
    /// edits at a time; errors render under the list.
    edit: Option<(String, String, EditMode)>,
    pub(super) rename: Entity<TextInput>,
    error: Option<String>,
    busy: bool,
    scroll: ScrollHandle,
}

impl InspectorState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<WorkbenchView>) -> Self {
        Self {
            edit: None,
            rename: cx.new(|cx| TextInput::new(window, cx).placeholder("Bookmark name")),
            error: None,
            busy: false,
            scroll: ScrollHandle::new(),
        }
    }
}

impl WorkbenchView {
    pub(super) fn on_inspector_input(&mut self, event: &InputEvent, cx: &mut Context<Self>) {
        match event {
            InputEvent::Submit => self.save_bookmark_rename(cx),
            InputEvent::Cancel => {
                self.dismiss_inspector(cx);
            }
            _ => cx.notify(),
        }
    }

    /// Close the row being edited; whether one was.
    pub(super) fn dismiss_inspector(&mut self, cx: &mut Context<Self>) -> bool {
        let was_editing = self.inspector.edit.take().is_some();
        if was_editing {
            self.inspector.error = None;
            cx.notify();
        }
        was_editing
    }

    fn start_bookmark_rename(
        &mut self,
        bookmark: &BookmarkState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.inspector.edit = Some((
            bookmark_key(bookmark),
            bookmark.name.clone(),
            EditMode::Rename,
        ));
        self.inspector.error = None;
        self.inspector.rename.update(cx, |input, cx| {
            input.set_text(&bookmark.name, cx);
            input.select_all_text(cx);
            input.focus(window);
        });
        cx.notify();
    }

    fn run_bookmark_edit(
        &mut self,
        action: impl FnOnce(
                &dyn jiji_core::RepoBackend,
                &std::path::Path,
            )
                -> Result<jiji_core::snapshot::MutationOutcome, jiji_core::BackendError>
            + Send
            + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.inspector.busy {
            return;
        }
        self.inspector.busy = true;
        self.inspector.error = None;
        cx.notify();
        let task = self.app.update(cx, |model, cx| model.mutate(cx, action));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.inspector.busy = false;
                match result {
                    Ok(_) => this.inspector.edit = None,
                    Err(error) => this.inspector.error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save_bookmark_rename(&mut self, cx: &mut Context<Self>) {
        let Some((_, name, EditMode::Rename)) = self.inspector.edit.clone() else {
            return;
        };
        let next = self.inspector.rename.read(cx).text().trim().to_owned();
        if next.is_empty() || next == name {
            return;
        }
        self.run_bookmark_edit(
            move |backend, path| backend.rename_bookmark(path, &name, &next),
            cx,
        );
    }

    fn confirm_bookmark_delete(&mut self, cx: &mut Context<Self>) {
        let Some((_, name, EditMode::Delete)) = self.inspector.edit.clone() else {
            return;
        };
        self.run_bookmark_edit(
            move |backend, path| backend.delete_bookmark(path, &name),
            cx,
        );
    }

    fn card_heading(&self, title: &'static str, count: usize, cx: &Context<Self>) -> gpui::Div {
        let t = theme(cx);
        h_flex()
            .gap(px(8.))
            .pb(px(6.))
            .mb(px(4.))
            .border_b_1()
            .border_color(t.border2)
            .child(
                div()
                    .text_size(px(TEXT_S))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text2)
                    .child(title),
            )
            .child(
                div()
                    .font_family(t.mono.clone())
                    .text_size(px(TEXT_XS))
                    .text_color(t.text3)
                    .child(count.to_string()),
            )
    }

    fn render_workstreams(&self, snapshot: &RepoSnapshot, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let focused = self.app.read(cx).ui().focused_workstream.clone();
        let focused =
            resolve_focused_workstream(snapshot, focused.as_deref()).map(|ws| ws.id.clone());
        let mut card = v_flex().flex_1().min_w_0().child(self.card_heading(
            "Workstreams",
            snapshot.workstreams.len(),
            cx,
        ));
        for (index, stream) in snapshot.workstreams.iter().enumerate() {
            let current = Some(&stream.id) == focused.as_ref();
            let lane = t.lane(index);
            let mut dot = div()
                .flex_shrink_0()
                .size(px(8.))
                .rounded(px(4.))
                .border(px(1.5))
                .border_color(lane);
            if stream.is_active {
                dot = dot.border_color(t.working_copy).bg(t.working_copy);
            }
            let mut row = h_flex()
                .id(("inspector-ws", index))
                .gap(px(8.))
                .min_w_0()
                .h(px(28.))
                .mx(px(-8.))
                .px(px(8.))
                .rounded(px(RADIUS_S))
                .cursor_pointer()
                .child(dot)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(TEXT_M))
                        .text_color(t.text1)
                        .child(truncated(stream.title.clone())),
                );
            row = if current {
                row.bg(t.accent_dim)
            } else {
                row.hover(|style| style.bg(t.bg_hover))
            };
            if let Some(bookmark) = &stream.bookmark {
                row = row.child(
                    ref_label(None, bookmark.clone(), cx)
                        .flex_shrink()
                        .min_w_0()
                        .max_w(px(150.)),
                );
            }
            let mut meta = h_flex()
                .flex_shrink_0()
                .gap(px(4.))
                .text_size(px(TEXT_S))
                .text_color(t.text3)
                .child(stream.node_ids.len().to_string());
            if stream.behind_trunk > 0 {
                meta = meta.child(
                    div()
                        .text_color(t.warn)
                        .child(format!("↓{}", stream.behind_trunk)),
                );
            }
            // Jumping to a workstream selects its head, which can be in
            // another lane; the model refocuses so the selection is
            // actually visible in the graph.
            if let Some(head) = stream.node_ids.first().cloned() {
                let app = self.app.clone();
                row = row.on_click(move |_, _, cx| {
                    app.update(cx, |model, cx| model.jump_to_change(&head, cx))
                });
            }
            card = card.child(row.child(meta));
        }
        card.into_any_element()
    }

    fn render_bookmark_list(
        &self,
        snapshot: &RepoSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let busy = self.inspector.busy;
        let mut card = v_flex().flex_1().min_w_0().child(self.card_heading(
            "Bookmarks",
            snapshot.bookmarks.len(),
            cx,
        ));
        for (index, bookmark) in snapshot.bookmarks.iter().enumerate() {
            let key = bookmark_key(bookmark);
            let editing = self
                .inspector
                .edit
                .as_ref()
                .filter(|(open, ..)| *open == key)
                .map(|(.., mode)| *mode);
            let row = h_flex()
                .gap(px(8.))
                .min_w_0()
                .min_h(px(28.))
                .text_size(px(TEXT_M));
            let cancel = btn(("inspector-bm-cancel", index), "Cancel")
                .small()
                .disabled(busy)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.dismiss_inspector(cx);
                }));
            let row = match editing {
                Some(EditMode::Rename) => {
                    let draft = self.inspector.rename.read(cx).text().trim().to_owned();
                    row.child(
                        field(&self.inspector.rename, window, cx)
                            .flex_1()
                            .h(px(24.))
                            .font_family(t.mono.clone()),
                    )
                    .child(
                        btn(("inspector-bm-go", index), "Rename")
                            .small()
                            .primary()
                            .disabled(busy || draft.is_empty() || draft == bookmark.name)
                            .on_click(cx.listener(|this, _, _, cx| this.save_bookmark_rename(cx))),
                    )
                    .child(cancel)
                }
                Some(EditMode::Delete) => row
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(TEXT_S))
                            .text_color(t.text2)
                            .child(truncated(format!(
                                "Delete {}? {}",
                                bookmark.name,
                                match &bookmark.remote {
                                    Some(remote) =>
                                        format!("Removed from {remote} on the next push."),
                                    None => "It only exists locally.".to_owned(),
                                }
                            ))),
                    )
                    .child(
                        btn(("inspector-bm-go", index), "Delete")
                            .small()
                            .danger_solid()
                            .disabled(busy)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.confirm_bookmark_delete(cx)),
                            ),
                    )
                    .child(cancel),
                None => {
                    let group = SharedString::from(format!("inspector-bm-{key}"));
                    let drawn = find_node(snapshot, &bookmark.target).is_some();
                    let (sync_text, tone) = sync_label(bookmark.sync);
                    let mut name = div()
                        .id(("inspector-bm-name", index))
                        .flex_1()
                        .min_w_0()
                        .font_family(t.mono.clone())
                        .text_size(px(TEXT_S))
                        .text_color(t.text1)
                        .child(truncated(bookmark.name.clone()));
                    if drawn {
                        let (app, id) = (self.app.clone(), bookmark.target.clone());
                        name = name
                            .cursor_pointer()
                            .hover(|style| style.text_color(t.accent_strong))
                            .tooltip(tooltip(format!("Go to {}", bookmark.target)))
                            .on_click(move |_, _, cx| {
                                app.update(cx, |model, cx| model.jump_to_change(&id, cx))
                            });
                    }
                    let row = row
                        .id(("inspector-bm", index))
                        .group(group.clone())
                        .on_mouse_down(
                            MouseButton::Right,
                            menu::bookmark_menu(bookmark.name.clone(), bookmark.remote.clone()),
                        )
                        .child(name)
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(TEXT_XS))
                                .text_color(t.tone(tone))
                                .child(sync_text),
                        );
                    // Every row keeps the slot its actions would take, so
                    // the sync labels sit in one column.
                    let mut actions = h_flex().flex_shrink_0().w(px(45.)).gap(px(1.));
                    if bookmark.is_local && !bookmark.is_trunk {
                        // Rename and delete reveal on row hover, like
                        // quiet contextual controls.
                        let (rename, delete) = (bookmark.clone(), bookmark.clone());
                        actions = actions
                            .opacity(0.)
                            .group_hover(group, |style| style.opacity(1.))
                            .child(
                                icon_btn(("inspector-bm-rename", index), IconName::Edit)
                                    .size(22., 11.)
                                    .tooltip(format!("Rename {}", bookmark.name))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.start_bookmark_rename(&rename, window, cx)
                                    })),
                            )
                            .child(
                                icon_btn(("inspector-bm-delete", index), IconName::Trash)
                                    .size(22., 11.)
                                    .tooltip(format!("Delete {}", bookmark.name))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.inspector.edit = Some((
                                            bookmark_key(&delete),
                                            delete.name.clone(),
                                            EditMode::Delete,
                                        ));
                                        this.inspector.error = None;
                                        cx.notify();
                                    })),
                            );
                    }
                    card = card.child(row.child(actions));
                    continue;
                }
            };
            card = card.child(row);
        }
        if let Some(error) = &self.inspector.error {
            card = card.child(
                div()
                    .mt(px(4.))
                    .text_size(px(TEXT_XS))
                    .text_color(t.danger)
                    .child(truncated(error.clone())),
            );
        }
        card.into_any_element()
    }

    pub(super) fn render_inspector(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let snapshot = self.snapshot.clone();
        let workspace = snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.is_current)
            .or(snapshot.workspaces.first());
        let trunk = snapshot.bookmarks.iter().find(|bookmark| bookmark.is_trunk);
        let fact = |id: &'static str, tip: &'static str| {
            h_flex()
                .id(id)
                .gap(px(5.))
                .text_size(px(TEXT_S))
                .text_color(t.text2)
                .tooltip(tooltip(tip))
        };
        // The working state at a glance: the tab owns the repo's identity,
        // so this heading carries only the standing facts.
        let mut facts = h_flex().flex_shrink_0().gap(px(12.));
        if let Some(trunk) = trunk {
            facts = facts.child(
                fact("fact-trunk", "Trunk target")
                    .child(icon(IconName::Branch, 11., t.text3))
                    .child(
                        div()
                            .font_family(t.mono.clone())
                            .child(match &trunk.remote {
                                Some(remote) => format!("{}@{remote}", trunk.name),
                                None => trunk.name.clone(),
                            }),
                    ),
            );
        }
        if let Some(workspace) = workspace {
            facts = facts.child(
                fact("fact-workspace", "Current workspace")
                    .child(icon(IconName::Workspaces, 11., t.text3))
                    .child(workspace.name.clone()),
            );
        }
        facts = facts.child(
            fact("fact-backend", "Backend").child(
                div()
                    .font_family(t.mono.clone())
                    .child(snapshot.backend.clone()),
            ),
        );

        // Sections side by side when the pane is wide, stacked when the
        // split squeezes it.
        let wide = self.diff_width >= 780.;
        let mut cards = div().flex().gap(px(24.));
        cards = if wide {
            cards.flex_row().items_start()
        } else {
            cards.flex_col()
        };
        if !snapshot.workstreams.is_empty() {
            cards = cards.child(self.render_workstreams(&snapshot, cx));
        }
        cards = cards.child(self.render_bookmark_list(&snapshot, window, cx));

        div()
            .relative()
            .size_full()
            .child(
                div()
                    .id("inspector-scroll")
                    .size_full()
                    .scroll_y()
                    .track_scroll(&self.inspector.scroll)
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(px(920.))
                            .mx_auto()
                            .pt(px(20.))
                            .px(px(24.))
                            .pb(px(24.))
                            .child(
                                h_flex()
                                    .gap(px(16.))
                                    .mb(px(20.))
                                    .min_w_0()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_size(px(TEXT_TITLE))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(t.text1)
                                            .child(truncated(snapshot.repo_name.clone())),
                                    )
                                    .child(facts),
                            )
                            .child(cards)
                            .child(
                                h_flex()
                                    .justify_center()
                                    .gap(px(8.))
                                    .pt(px(40.))
                                    .text_size(px(TEXT_S))
                                    .text_color(t.text3)
                                    .child(
                                        h_flex()
                                            .gap(px(2.))
                                            .child(kbd("↑", cx))
                                            .child(kbd("↓", cx)),
                                    )
                                    .child("Select a change in the graph to review its diff"),
                            ),
                    ),
            )
            .child(scrollbar("inspector-scrollbar", &self.inspector.scroll))
    }
}
