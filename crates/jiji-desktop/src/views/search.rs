//! The history search overlay (⌘⇧F): `jj log -S` for people. Styled like
//! the command palette — same overlay, same manners — but its rows are
//! backend search hits, not commands.

use std::path::Path;
use std::time::Duration;

use gpui::{
    actions, div, prelude::*, px, AnyView, App, Context, Entity, KeyBinding, MouseButton,
    ScrollHandle, Subscription, Task, Window,
};
use jiji_core::snapshot::{HistorySearch, SearchHit};

use crate::app::{app, AppModel};
use crate::logic::inspect::summary_of;
use crate::logic::search::{file_hit_label, search_status, HitTone, MIN_QUERY_LENGTH};
use crate::logic::text::short_id;
use crate::theme::{theme, Theme, RADIUS_M, TEXT_M, TEXT_S, TEXT_XS};
use crate::ui::a11y::spoken;
use crate::ui::ScrollY as _;
use crate::ui::{
    icon, kbd, popover, scrollbar, short_age, truncated, IconName, InputEvent, NodeExt as _,
    TextInput,
};

actions!(history_search, [SearchUp, SearchDown, SearchHold]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SearchUp, Some("HistorySearch")),
        KeyBinding::new("down", SearchDown, Some("HistorySearch")),
        KeyBinding::new("tab", SearchHold, Some("HistorySearch")),
        KeyBinding::new("shift-tab", SearchHold, Some("HistorySearch")),
    ]);
}

/// The pickaxe walks and diffs history, so it fires on a settled query,
/// not per keystroke.
const DEBOUNCE: Duration = Duration::from_millis(300);

struct Search {
    app: Entity<AppModel>,
    input: Entity<TextInput>,
    result: Option<HistorySearch>,
    searching: bool,
    error: Option<String>,
    active: usize,
    scroll: ScrollHandle,
    /// Stale responses (an older request resolving late) are dropped by
    /// sequence.
    seq: u64,
    pending: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Search {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let app = app(cx);
        // The last query comes back selected: re-running a search is one
        // keystroke and a new one is just typing.
        let seed = {
            let model = app.read(cx);
            if model.search_seed.is_empty() {
                model.search_last.clone()
            } else {
                model.search_seed.clone()
            }
        };
        let input = cx.new(|cx| {
            TextInput::new(window, cx)
                .placeholder("Search history for text or a filename…")
                .label("Search history")
                .with_text(&seed)
        });
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all_text(cx);
        });
        let subscriptions = vec![cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::Change => this.query_changed(cx),
                InputEvent::Submit => this.pick(this.active, cx),
                _ => {}
            },
        )];
        let mut this = Self {
            app,
            input,
            result: None,
            searching: false,
            error: None,
            active: 0,
            scroll: ScrollHandle::new(),
            seq: 0,
            pending: None,
            _subscriptions: subscriptions,
        };
        this.query_changed(cx);
        this
    }

    fn query(&self, cx: &App) -> String {
        self.input.read(cx).text().trim().to_owned()
    }

    fn query_changed(&mut self, cx: &mut Context<Self>) {
        let raw = self.input.read(cx).text().to_owned();
        self.app.update(cx, |model, _| model.search_last = raw);
        let query = self.query(cx);
        self.seq += 1;
        let seq = self.seq;
        self.error = None;
        cx.notify();
        if query.chars().count() < MIN_QUERY_LENGTH {
            self.result = None;
            self.searching = false;
            self.pending = None;
            return;
        }
        let Some(repo) = self.app.read(cx).repo_path() else {
            return;
        };
        self.searching = true;
        let app = self.app.clone();
        self.pending = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let searched = app.read_with(cx, |model, cx| {
                model.bg(cx, move |host| {
                    host.backend()
                        .search_history(Path::new(&repo), &query)
                        .map_err(crate::host::CommandError::from)
                })
            });
            let response = searched.await;
            let _ = this.update(cx, |this, cx| {
                if this.seq != seq {
                    return;
                }
                this.searching = false;
                match response {
                    Ok(result) => {
                        this.result = Some(result);
                        this.active = 0;
                        this.scroll.set_offset(gpui::Point::default());
                    }
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        }));
    }

    fn hits(&self) -> &[SearchHit] {
        self.result.as_ref().map_or(&[], |result| &result.hits)
    }

    fn travel(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.hits().len();
        if count == 0 {
            return;
        }
        self.active = (self.active as isize + delta).clamp(0, count as isize - 1) as usize;
        self.scroll.scroll_to_item(self.active);
        cx.notify();
    }

    fn pick(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(id) = self.hits().get(index).map(|hit| hit.id.clone()) else {
            return;
        };
        self.app.update(cx, |model, cx| {
            model.close_search(cx);
            model.reveal_change(id, cx);
        });
    }

    fn render_hit(
        &self,
        hit: &SearchHit,
        index: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let title = match summary_of(&hit.description) {
            "" => "no description",
            title => title,
        };
        let head = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .min_w_0()
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(62.))
                    .font_family(t.mono.clone())
                    .text_size(px(TEXT_S))
                    .text_color(t.text2)
                    .child(short_id(&hit.id, 8).to_owned()),
            )
            .child(div().flex_1().min_w_0().child(truncated(title.to_owned())))
            .child(
                div()
                    .flex_shrink_0()
                    .max_w(px(220.))
                    .text_size(px(TEXT_XS))
                    .text_color(t.text3)
                    .child(truncated(format!(
                        "{} · {}",
                        hit.author,
                        short_age(&hit.timestamp)
                    ))),
            );
        let mut files = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .min_w_0()
            .pl(px(70.))
            .text_size(px(TEXT_XS))
            .text_color(t.text3);
        for file in hit.files.iter().take(3) {
            let (label, tone) = file_hit_label(file);
            files = files.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.))
                    .min_w_0()
                    .font_family(t.mono.clone())
                    .child(div().min_w_0().child(truncated(file.path.clone())))
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(match tone {
                                HitTone::Add => t.add_fg,
                                HitTone::Del => t.del_fg,
                                HitTone::Name => t.accent_strong,
                            })
                            .child(label),
                    ),
            );
        }
        let more = hit.files.len().saturating_sub(3) + hit.more_files as usize;
        if more > 0 {
            files = files.child(div().flex_shrink_0().child(format!("+{more} more")));
        }
        let mut row = div()
            .id(("search-hit", index))
            .role(gpui::Role::ListBoxOption)
            .aria_label(hit_name(hit))
            .aria_selected(index == self.active)
            .flex()
            .flex_col()
            .flex_shrink_0()
            .gap(px(2.))
            .py(px(6.))
            .px(px(8.))
            .rounded(px(RADIUS_M))
            .text_size(px(TEXT_M))
            .text_color(t.text1)
            .child(head)
            .child(files);
        if index == self.active {
            row = row.bg(Theme::fade(t.text1, 0.08));
        }
        row.on_mouse_move(cx.listener(move |this, _, _, cx| {
            if this.active != index {
                this.active = index;
                cx.notify();
            }
        }))
        .on_click(cx.listener(move |this, _, _, cx| this.pick(index, cx)))
    }
}

impl Render for Search {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let viewport = window.viewport_size();
        let width = (f32::from(viewport.width) - 96.).min(700.);
        let max_height = (f32::from(viewport.height) * 0.72).min(520.);

        let mut list = div()
            .id("search-list")
            .role(gpui::Role::ListBox)
            .aria_label("Matching changes")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .scroll_y()
            .track_scroll(&self.scroll)
            .p(px(8.));
        for index in 0..self.hits().len() {
            let hit = self.hits()[index].clone();
            list = list.child(self.render_hit(&hit, index, cx));
        }
        let note = |text: String, color| {
            div()
                .p(px(20.))
                .flex()
                .justify_center()
                .text_size(px(TEXT_S))
                .text_color(color)
                .child(text)
        };
        if self.hits().is_empty() && !self.searching && self.error.is_none() {
            let short = self.query(cx).chars().count() < MIN_QUERY_LENGTH;
            list = list.child(note(
                if short {
                    "Find the changes that introduced or removed a string — or touched a file"
                } else {
                    "No matching change"
                }
                .to_owned(),
                t.text3,
            ));
        }
        if let Some(error) = &self.error {
            list = list.child(
                note(error.clone(), t.warn)
                    .id("search-error")
                    .alert(error.clone()),
            );
        }
        let status = if self.searching {
            Some("Searching…".to_owned())
        } else {
            self.result.as_ref().map(search_status)
        };

        let highlighted = self
            .hits()
            .get(self.active)
            .map(|hit| {
                format!(
                    "{}, {} of {}",
                    hit_name(hit),
                    self.active + 1,
                    self.hits().len()
                )
            })
            .unwrap_or_default();
        let panel = popover(cx)
            .id("history-search")
            .role(gpui::Role::Dialog)
            .aria_label("Search history")
            .a11y(|node| node.set_modal())
            .key_context("HistorySearch")
            .on_action(cx.listener(|this, _: &SearchUp, _, cx| this.travel(-1, cx)))
            .on_action(cx.listener(|this, _: &SearchDown, _, cx| this.travel(1, cx)))
            .on_action(|_: &SearchHold, _, _| {})
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .w(px(width))
            .max_h(px(max_height))
            .flex()
            .flex_col()
            .rounded(px(10.))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .flex_shrink_0()
                    .gap(px(12.))
                    .h(px(48.))
                    .px(px(16.))
                    .border_b_1()
                    .border_color(t.border2)
                    .child(icon(IconName::Search, 15., t.text3))
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(15.))
                            .line_height(px(22.))
                            .child(self.input.clone()),
                    )
                    .child(kbd("esc", cx)),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(list)
                    .child(scrollbar("search-scroll", &self.scroll)),
            )
            .child(
                div()
                    .id("search-status")
                    .status(status.clone().unwrap_or_default())
                    .flex()
                    .flex_row()
                    .items_center()
                    .flex_shrink_0()
                    .min_h(px(28.))
                    .px(px(16.))
                    .border_t_1()
                    .border_color(t.border2)
                    .text_size(px(TEXT_XS))
                    .text_color(t.text3)
                    .children(status),
            )
            .child(spoken("search-highlight", highlighted));

        let app = self.app.clone();
        div()
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            .items_start()
            // Like the palette: out of the title bar's command field.
            .pt(px(4.))
            .bg(Theme::fade(t.bg0, 0.3))
            .occlude()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                app.update(cx, |model, cx| model.close_search(cx));
            })
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(panel)
    }
}

/// What a hit says to assistive technology: the change, then what in it
/// matched.
fn hit_name(hit: &SearchHit) -> String {
    let title = summary_of(&hit.description);
    let mut text = format!(
        "{}, {}",
        if title.is_empty() {
            "no description"
        } else {
            title
        },
        short_id(&hit.id, 8)
    );
    for file in hit.files.iter().take(3) {
        text.push_str(&format!(", {} {}", file.path, file_hit_label(file).0));
    }
    text
}

pub fn build(window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| Search::new(window, cx)).into()
}
