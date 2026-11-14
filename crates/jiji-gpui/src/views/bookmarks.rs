//! The bookmarks manager: every bookmark the repo knows — local work and
//! each remote's branches (teammates' work included) — searchable, with
//! track/push/delete actions and a merged-work cleanup sweep. Seeing remote
//! refs beside local ones is how a team knows what exists and what to
//! throw away.

use std::collections::HashSet;

use gpui::{
    div, prelude::*, px, AnyElement, Context, Entity, FocusHandle, Focusable, FontWeight,
    MouseButton, ScrollHandle, SharedString, Window,
};
use jiji_core::snapshot::{BookmarkState, MutationOutcome, RepoSnapshot};
use jiji_core::{BackendError, RepoBackend};

use crate::app::{app, AppModel};
use crate::logic::bookmarks::{bookmark_key, group_bookmarks, merged_bookmarks};
use crate::logic::graph::sync_glyph;
use crate::logic::text::short_id;
use crate::shell::topbar::checked_age_label;
use crate::shell::Dismiss;
use crate::theme::{theme, TEXT_S, TEXT_XS};
use crate::ui::pinned;
use crate::ui::ScrollY as _;
use crate::ui::{
    btn, checkbox, empty_state, group_head, h_flex, page_bar, page_filter, page_notice, scrollbar,
    short_age, tag, tooltip, tray, truncated, v_flex, IconName, InputEvent, Tabular as _,
    TextInput, PAGE_PAD,
};
use crate::views::menu;

/// One bookmark, one line.
const BOOKMARK_ROW_H: f32 = 32.;
/// The name column takes a third of the page, within these.
const NAME_MIN: f32 = 200.;
const NAME_MAX: f32 = 440.;
/// A short change id, and the author-and-age cell that the row's actions
/// take over under the pointer.
const ID_W: f32 = 62.;
const META_W: f32 = 150.;
/// How much of the page a cleanup sweep's checklist may take before it
/// scrolls.
const CLEANUP_LIST_MAX: f32 = 216.;

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeleteKind {
    Local,
    Remote,
}

type Mutation = Box<
    dyn FnOnce(&dyn RepoBackend, &std::path::Path) -> Result<MutationOutcome, BackendError> + Send,
>;

pub struct BookmarksView {
    app: Entity<AppModel>,
    focus: FocusHandle,
    filter: Entity<TextInput>,
    /// Every row action shares one lane: one at a time, errors surface in
    /// the view's banner (rows have no room for a story).
    busy: bool,
    error: Option<String>,
    cleanup_open: bool,
    checked: HashSet<String>,
    also_remote: bool,
    /// The row whose destructive action awaits its confirm click.
    confirming: Option<(String, DeleteKind)>,
    scroll: ScrollHandle,
}

impl Focusable for BookmarksView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

fn delete_remote(name: String, remote: String) -> Mutation {
    Box::new(move |backend, path| backend.delete_remote_bookmark(path, &name, &remote))
}

fn delete_local(name: String) -> Mutation {
    Box::new(move |backend, path| backend.delete_bookmark(path, &name))
}

impl BookmarksView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let app = app(cx);
        cx.observe(&app, |_, _, cx| cx.notify()).detach();
        let filter = cx.new(|cx| {
            TextInput::new(window, cx).placeholder("Filter by name, author, or description")
        });
        cx.subscribe(&filter, |_, _, event: &InputEvent, cx| {
            if *event == InputEvent::Change {
                cx.notify();
            }
        })
        .detach();
        Self {
            app,
            focus: cx.focus_handle(),
            filter,
            busy: false,
            error: None,
            cleanup_open: false,
            checked: HashSet::new(),
            also_remote: false,
            confirming: None,
            scroll: ScrollHandle::new(),
        }
    }

    /// Run mutations in order, stopping at the first refusal.
    fn run(&mut self, steps: Vec<Mutation>, close_cleanup: bool, cx: &mut Context<Self>) {
        if self.busy || steps.is_empty() {
            return;
        }
        self.busy = true;
        self.error = None;
        self.confirming = None;
        cx.notify();
        let app = self.app.clone();
        cx.spawn(async move |this, cx| {
            let mut failure = None;
            for step in steps {
                let Ok(task) = app.update(cx, |model, cx| model.mutate(cx, step)) else {
                    return;
                };
                if let Err(error) = task.await {
                    failure = Some(error.message);
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                if failure.is_none() && close_cleanup {
                    this.cleanup_open = false;
                }
                this.error = failure;
                cx.notify();
            });
        })
        .detach();
    }

    fn run_one(&mut self, step: Mutation, cx: &mut Context<Self>) {
        self.run(vec![step], false, cx);
    }

    fn open_cleanup(&mut self, snapshot: &RepoSnapshot, cx: &mut Context<Self>) {
        self.checked = merged_bookmarks(&snapshot.bookmarks)
            .into_iter()
            .map(bookmark_key)
            .collect();
        self.also_remote = false;
        self.cleanup_open = true;
        cx.notify();
    }

    fn run_cleanup(&mut self, snapshot: &RepoSnapshot, cx: &mut Context<Self>) {
        let steps = merged_bookmarks(&snapshot.bookmarks)
            .into_iter()
            .filter(|bookmark| self.checked.contains(&bookmark_key(bookmark)))
            .map(|bookmark| match (&bookmark.remote, bookmark.is_local) {
                (Some(remote), false) => delete_remote(bookmark.name.clone(), remote.clone()),
                (Some(remote), true) if self.also_remote && bookmark.tracked => {
                    delete_remote(bookmark.name.clone(), remote.clone())
                }
                _ => delete_local(bookmark.name.clone()),
            })
            .collect();
        self.run(steps, true, cx);
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirming.take().is_some() {
            cx.notify();
        } else if self.cleanup_open {
            self.cleanup_open = false;
            cx.notify();
        } else if self.filter.read(cx).is_focused(window) {
            // Out of the filter, back to the list.
            window.focus(&self.focus);
        } else {
            cx.propagate();
        }
    }

    /// The page's bar: how many bookmarks, the filter, and the two acts
    /// that concern all of them.
    fn render_bar(
        &self,
        snapshot: &RepoSnapshot,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let merged = merged_bookmarks(&snapshot.bookmarks).len();
        let mut bar = page_bar("Bookmarks", Some(snapshot.bookmarks.len().to_string()), cx)
            .child(div().w(px(4.)))
            .child(page_filter(&self.filter, window, cx))
            .child(div().flex_1());
        if !snapshot.git_remotes.is_empty() {
            let (checking, last, now) = {
                let upstream = &self.app.read(cx).upstream;
                (upstream.checking, upstream.last_checked, upstream.now)
            };
            let label = match (checking, last) {
                (true, _) => "Fetching…".to_owned(),
                (false, Some(at)) => format!("Fetched {}", checked_age_label(at, now)),
                (false, None) => "Fetch now".to_owned(),
            };
            let app = self.app.clone();
            bar = bar.child(
                btn("bookmarks-fetch", label)
                    .ghost()
                    .icon(IconName::Refresh)
                    .disabled(checking)
                    .tooltip("jj git fetch — refresh what the remotes have")
                    .on_click(move |_, _, cx| {
                        app.update(cx, |model, cx| model.check_upstream(true, cx))
                    }),
            );
        }
        if merged > 0 {
            let snapshot = snapshot.clone();
            bar = bar.child(
                btn("bookmarks-cleanup", format!("Clean up merged ({merged})"))
                    .icon(IconName::Trash)
                    .armed(self.cleanup_open)
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, _, cx| this.open_cleanup(&snapshot, cx))),
            );
        }
        bar
    }

    /// The consequence-stating tray: exactly what a sweep will delete,
    /// opt-out per row, remote deletion opt-in.
    fn render_cleanup(&self, snapshot: &RepoSnapshot, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let merged = merged_bookmarks(&snapshot.bookmarks);
        let trunk = if snapshot.trunk_bookmark.is_empty() {
            "trunk"
        } else {
            &snapshot.trunk_bookmark
        };
        let mut card = tray(cx).child(div().mb(px(8.)).text_color(t.text2).child(format!(
            "These bookmarks point at work that is already part of {trunk} — deleting them \
             loses nothing."
        )));
        // The tray stays under the bar while the page scrolls, so a long
        // sweep scrolls inside it.
        let mut list = div()
            .id("cleanup-list")
            .max_h(px(CLEANUP_LIST_MAX))
            .scroll_y();
        for (index, bookmark) in merged.iter().enumerate() {
            let key = bookmark_key(bookmark);
            let on = self.checked.contains(&key);
            let toggle = cx.listener(move |this, _, _, cx| {
                if !this.checked.remove(&key) {
                    this.checked.insert(key.clone());
                }
                cx.notify();
            });
            list = list.child(
                h_flex()
                    .gap(px(8.))
                    .py(px(3.))
                    .text_color(t.text1)
                    .child(checkbox(("cleanup", index), Some(on), cx, toggle))
                    .child(
                        div()
                            .max_w(gpui::relative(0.45))
                            .font_family(t.mono.clone())
                            .child(truncated(match (&bookmark.remote, bookmark.is_local) {
                                (Some(remote), false) => format!("{}@{remote}", bookmark.name),
                                _ => bookmark.name.clone(),
                            })),
                    )
                    .child(div().text_size(px(TEXT_XS)).text_color(t.text3).child(
                        match (bookmark.is_local, bookmark.tracked) {
                            (true, true) => "local, tracked",
                            (true, false) => "local only",
                            (false, _) => "remote only — deletes on the remote",
                        },
                    )),
            );
        }
        card = card.child(list);
        if merged.iter().any(|b| b.is_local && b.tracked) {
            card = card.child(
                h_flex()
                    .gap(px(8.))
                    .mt(px(8.))
                    .pt(px(8.))
                    .border_t_1()
                    .border_color(t.border2)
                    .text_color(t.text2)
                    .child(checkbox(
                        "cleanup-also-remote",
                        Some(self.also_remote),
                        cx,
                        cx.listener(|this, _, _, cx| {
                            this.also_remote = !this.also_remote;
                            cx.notify();
                        }),
                    ))
                    .child("Also delete tracked ones on their remote"),
            );
        }
        let count = merged
            .iter()
            .filter(|bookmark| self.checked.contains(&bookmark_key(bookmark)))
            .count();
        let sweep = snapshot.clone();
        card.child(
            h_flex()
                .gap(px(8.))
                .mt(px(12.))
                .child(
                    btn(
                        "cleanup-go",
                        format!(
                            "Delete {count} bookmark{}",
                            if count == 1 { "" } else { "s" }
                        ),
                    )
                    .danger_solid()
                    .disabled(self.busy || count == 0)
                    .on_click(cx.listener(move |this, _, _, cx| this.run_cleanup(&sweep, cx))),
                )
                .child(
                    btn("cleanup-cancel", "Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.cleanup_open = false;
                        cx.notify();
                    })),
                ),
        )
    }

    fn render_row(
        &self,
        bookmark: &BookmarkState,
        snapshot: &RepoSnapshot,
        name_w: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let key = bookmark_key(bookmark);
        let group = SharedString::from(format!("bm-{key}"));
        let can_jump = snapshot.nodes.iter().any(|node| node.id == bookmark.target);
        let has_remotes = !snapshot.git_remotes.is_empty();
        let (glyph, tone, sync_text) = sync_glyph(bookmark.sync);

        // One line, in columns: the name and its states, the change it
        // points at, then who moved it last and when.
        // Where the bookmark stands against its remote leads the row, in a
        // column of its own, so the states scan down the page.
        let sync = div()
            .id(SharedString::from(format!("sync-{key}")))
            .w(px(12.))
            .flex_shrink_0()
            .flex()
            .justify_center()
            .text_size(px(TEXT_XS))
            .text_color(t.tone(tone))
            .tooltip(tooltip(sync_text))
            .child(glyph);
        let mut name = h_flex()
            .w(px(name_w))
            .flex_shrink_0()
            .gap(px(8.))
            .child(sync)
            .child(
                div()
                    .min_w_0()
                    .flex_shrink()
                    .font_family(t.mono.clone())
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.text1)
                    .when(can_jump, |el| {
                        el.group_hover(group.clone(), |style| style.text_color(t.accent_strong))
                    })
                    .child(truncated(bookmark.name.clone())),
            );
        if bookmark.is_trunk {
            name = name.child(tag("trunk", t.accent_strong));
        }
        if !bookmark.is_local && bookmark.tracked {
            name = name.child(tag("tracked", t.text3));
        }
        if bookmark.is_merged {
            name = name.child(tag("merged", t.ok));
        }
        let mut ident = h_flex()
            .id(SharedString::from(format!("ident-{key}")))
            .flex_1()
            .min_w_0()
            .h_full()
            .gap(px(12.))
            .child(name)
            .child(
                div()
                    .w(px(ID_W))
                    .flex_shrink_0()
                    .font_family(t.mono.clone())
                    .text_size(px(TEXT_XS))
                    .text_color(t.text3)
                    .child(short_id(&bookmark.target, 8).to_owned()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(40.))
                    .text_color(t.text2)
                    .child(truncated(if bookmark.target_description.is_empty() {
                        "(no description)".to_owned()
                    } else {
                        bookmark.target_description.clone()
                    })),
            );
        if can_jump {
            let app = self.app.clone();
            let id = bookmark.target.clone();
            ident = ident
                .cursor_pointer()
                .tooltip(tooltip("Show in the workbench"))
                .on_click(move |_, _, cx| {
                    app.update(cx, |model, cx| model.jump_to_change(&id, cx))
                });
        } else {
            ident = ident.tooltip(tooltip(
                "Not drawn in the current view — switch the workbench view to see it",
            ));
        }

        let mut row = h_flex()
            .id(SharedString::from(format!("row-{key}")))
            .group(group.clone())
            .h(px(BOOKMARK_ROW_H))
            .gap(px(12.))
            .pl(px(PAGE_PAD - 4.))
            .pr(px(12.))
            .text_size(px(TEXT_S))
            .border_b_1()
            .border_color(t.border2)
            .hover(|style| style.bg(t.bg_hover))
            .on_mouse_down(
                MouseButton::Right,
                menu::bookmark_menu(bookmark.name.clone(), bookmark.remote.clone()),
            )
            .child(ident);
        if self.busy {
            row = row.opacity(0.7);
        }

        let confirming = self
            .confirming
            .as_ref()
            .filter(|(open, _)| *open == key)
            .map(|(_, kind)| *kind);
        if let Some(kind) = confirming {
            let place = bookmark.remote.as_deref().unwrap_or("the remote");
            let label = match (kind, bookmark.is_local) {
                (DeleteKind::Local, _) => format!("Delete {}?", bookmark.name),
                (DeleteKind::Remote, true) => {
                    format!("Delete {} here and on {place}?", bookmark.name)
                }
                (DeleteKind::Remote, false) => {
                    format!("Push a deletion of {} to {place}?", bookmark.name)
                }
            };
            let step_name = bookmark.name.clone();
            let step_remote = bookmark.remote.clone();
            return row
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .gap(px(8.))
                        .max_w(gpui::relative(0.6))
                        .child(
                            div()
                                .min_w(px(40.))
                                .flex_shrink()
                                .text_size(px(TEXT_XS))
                                .text_color(t.text2)
                                .child(truncated(label)),
                        )
                        .child(
                            btn("bm-confirm", "Delete")
                                .small()
                                .danger_solid()
                                .disabled(self.busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let step = match (kind, step_remote.clone()) {
                                        (DeleteKind::Remote, Some(remote)) => {
                                            delete_remote(step_name.clone(), remote)
                                        }
                                        _ => delete_local(step_name.clone()),
                                    };
                                    this.run_one(step, cx);
                                })),
                        )
                        .child(
                            btn("bm-confirm-cancel", "Cancel")
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirming = None;
                                    cx.notify();
                                })),
                        ),
                )
                .into_any_element();
        }

        // Actions surface on hover, like graph-row affordances; the meta
        // yields to them so the row never jumps. Both always hold their
        // place in the layout — only visibility follows the pointer.
        let mut actions = h_flex()
            .gap(px(4.))
            .invisible()
            .group_hover(group.clone(), |style| style.visible());
        let small = |id: &'static str, label: &'static str| {
            btn(SharedString::from(format!("{id}-{key}")), label)
                .small()
                .disabled(self.busy)
        };
        let name = bookmark.name.clone();
        let remote = bookmark.remote.clone();
        let tracked_remote = remote.clone().filter(|_| bookmark.tracked);
        if bookmark.is_local {
            if has_remotes {
                let (name, remote) = (name.clone(), remote.clone());
                actions = actions.child(small("push", "Push").on_click(cx.listener(
                    move |this, _, _, cx| {
                        let (name, remote) = (name.clone(), remote.clone());
                        this.run_one(
                            Box::new(move |backend, path| {
                                backend.push_bookmarks(path, &[name], remote.as_deref())
                            }),
                            cx,
                        );
                    },
                )));
            }
            if !bookmark.is_trunk {
                if let Some(remote) = tracked_remote.clone() {
                    let name = name.clone();
                    actions = actions.child(small("untrack", "Untrack").on_click(cx.listener(
                        move |this, _, _, cx| {
                            let (name, remote) = (name.clone(), remote.clone());
                            this.run_one(
                                Box::new(move |backend, path| {
                                    backend.untrack_bookmark(path, &name, &remote)
                                }),
                                cx,
                            );
                        },
                    )));
                }
                let open = key.clone();
                actions = actions.child(small("delete", "Delete").danger().on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.confirming = Some((open.clone(), DeleteKind::Local));
                        cx.notify();
                    },
                )));
                if tracked_remote.is_some() {
                    let open = key.clone();
                    actions = actions.child(
                        small("delete-remote", "Delete + remote")
                            .danger()
                            .tooltip("Delete here and push the deletion to the remote")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.confirming = Some((open.clone(), DeleteKind::Remote));
                                cx.notify();
                            })),
                    );
                }
            }
        } else if let Some(remote) = remote {
            if !bookmark.tracked {
                let (name, remote) = (name.clone(), remote.clone());
                actions = actions.child(small("track", "Track").on_click(cx.listener(
                    move |this, _, _, cx| {
                        let (name, remote) = (name.clone(), remote.clone());
                        this.run_one(
                            Box::new(move |backend, path| {
                                backend.track_bookmark(path, &name, &remote)
                            }),
                            cx,
                        );
                    },
                )));
            }
            let open = key.clone();
            actions = actions.child(
                small("delete-remote", "Delete on remote")
                    .danger()
                    .tooltip("Push a deletion of this branch to the remote")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.confirming = Some((open.clone(), DeleteKind::Remote));
                        cx.notify();
                    })),
            );
        }
        let meta = h_flex()
            .id(SharedString::from(format!("meta-{key}")))
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(META_W))
            .justify_end()
            .gap(px(10.))
            .text_size(px(TEXT_XS))
            .text_color(t.text3)
            .group_hover(group, |style| style.invisible())
            .tooltip(tooltip(format!(
                "{} · {}",
                bookmark.target_author,
                crate::ui::absolute_time(&bookmark.target_timestamp)
            )))
            .child(
                div()
                    .max_w(px(META_W - 42.))
                    .child(truncated(bookmark.target_author.clone())),
            )
            .child(div().w(px(32.)).flex().justify_end().tabular().child(
                if bookmark.target_timestamp.is_empty() {
                    String::new()
                } else {
                    short_age(&bookmark.target_timestamp)
                },
            ));
        row.child(
            h_flex()
                .relative()
                .flex_shrink_0()
                .min_w(px(META_W))
                .min_h(px(22.))
                .justify_end()
                .child(actions)
                .child(meta),
        )
        .into_any_element()
    }

    fn render_group(
        &self,
        label: &str,
        note: Option<String>,
        bookmarks: &[&BookmarkState],
        snapshot: &RepoSnapshot,
        name_w: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // The heading stays put while its group scrolls under it.
        let heading = group_head(
            label.to_owned(),
            Some(bookmarks.len()),
            note.map(Into::into),
            cx,
        );
        let mut group = div();
        for bookmark in bookmarks {
            group = group.child(self.render_row(bookmark, snapshot, name_w, cx));
        }
        pinned(heading, group)
    }
}

impl Render for BookmarksView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let Some(snapshot) = self.app.read(cx).snapshot().cloned() else {
            return div().into_any_element();
        };
        let root = div()
            .id("bookmarks")
            .key_context("Bookmarks")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss(window, cx)));
        if snapshot.bookmarks.is_empty() {
            return root
                .child(empty_state(
                    IconName::Bookmark,
                    "No bookmarks",
                    "Bookmarks are jj's branch names — yours locally, and every branch on the \
                     remotes after a fetch. Create one from a change's header, or fetch to see \
                     what the team has pushed.",
                    Some("jj bookmark create".into()),
                    cx,
                ))
                .into_any_element();
        }
        let query = self.filter.read(cx).text().to_owned();
        let groups = group_bookmarks(&snapshot.bookmarks, &query);
        // The name column takes a third of the page, the same on every
        // row whatever else the row is showing.
        let page_w = f32::from(window.viewport_size().width);
        let name_w = (page_w * 0.3).clamp(NAME_MIN, NAME_MAX);
        // Block flow: see the Operations page's column.
        let mut column = div().w_full().pb(px(32.));
        if groups.local.is_empty() && groups.remotes.is_empty() {
            column = column.child(
                div()
                    .py(px(14.))
                    .px(px(PAGE_PAD))
                    .text_size(px(TEXT_S))
                    .text_color(t.text3)
                    .child(format!("Nothing matches \u{201c}{query}\u{201d}.")),
            );
        }
        if !groups.local.is_empty() {
            column = column.child(self.render_group(
                "Local",
                None,
                &groups.local,
                &snapshot,
                name_w,
                cx,
            ));
        }
        for (remote, bookmarks) in &groups.remotes {
            column = column.child(self.render_group(
                remote,
                Some(format!(
                    "Branches on {remote} with no local bookmark — fetched, not tracked. Track \
                     one to work with it; jump via the Team view."
                )),
                bookmarks,
                &snapshot,
                name_w,
                cx,
            ));
        }
        let mut page = v_flex()
            .size_full()
            .child(self.render_bar(&snapshot, window, cx));
        if let Some(error) = &self.error {
            page = page.child(page_notice(
                "bookmarks-error-dismiss",
                error.clone(),
                cx,
                cx.listener(|this, _, _, cx| {
                    this.error = None;
                    cx.notify();
                }),
            ));
        }
        if self.cleanup_open {
            page = page.child(self.render_cleanup(&snapshot, cx));
        }
        root.child(
            page.child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("bookmarks-scroll")
                            .size_full()
                            .scroll_y()
                            .track_scroll(&self.scroll)
                            .child(column),
                    )
                    .child(scrollbar("bookmarks-scrollbar", &self.scroll)),
            ),
        )
        .into_any_element()
    }
}
