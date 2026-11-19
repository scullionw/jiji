//! One file annotated line by line (`jj file annotate`): every line wears
//! the change that introduced it, grouped into runs. Read-only — the one
//! verb is the origin gutter itself, which jumps the graph to that change
//! (revealing it under a transient revset when the current lens does not
//! draw it). Replaces the diff surface while open; any selection move —
//! including that jump — closes it.
//!
//! From the keyboard the lines are one stop: ↑ and ↓ step from run to run
//! (each run's origin is what there is to follow), and Return or Space
//! follows the one the keyboard is on.

use std::ops::Range;
use std::sync::Arc;

use gpui::{
    actions, div, prelude::*, px, uniform_list, AnyElement, App, Context, FocusHandle, FontWeight,
    KeyBinding, Role, ScrollEvent, ScrollStrategy, SharedString, UniformListScrollHandle, Window,
};
use jiji_core::snapshot::FileAnnotation;

use super::WorkbenchView;
use crate::diff::model::LINE_H;
use crate::logic::text::short_id;
use crate::theme::{theme, Theme, TEXT_S, TEXT_XS};
use crate::ui::{
    from_now, h_flex, icon, icon_btn, keyboard_on, label_box, pan_scrollbar, ring, short_age,
    tooltip, truncated, v_flex, vertical_only, wheel_first, AxisLock, IconName, NodeExt as _,
};

actions!(annotate, [OriginUp, OriginDown, OriginFollow]);

pub(super) fn bind_keys(cx: &mut App) {
    let lines = Some("Annotate");
    cx.bind_keys([
        KeyBinding::new("up", OriginUp, lines),
        KeyBinding::new("down", OriginDown, lines),
        KeyBinding::new("enter", OriginFollow, lines),
        KeyBinding::new("space", OriginFollow, lines),
    ]);
}

const GUTTER_W: f32 = 224.;
/// The line-number column, and the room kept clear after the longest line.
const NUMBER_W: f32 = 40.;
const CODE_PAD_R: f32 = 16.;
const CODE_SIZE: f32 = 12.;

pub(super) struct AnnotateState {
    /// What the pane was last asked for: change, path, operation. A new
    /// operation refetches — the change may have been rewritten under it.
    key: Option<(String, String, String)>,
    pub(super) data: Option<Arc<FileAnnotation>>,
    error: Option<String>,
    seq: u64,
    /// Hovering any line tints its whole origin's runs — the fastest way
    /// to read "what else came from this change" in a multi-author file.
    hover: Option<u32>,
    scroll: UniformListScrollHandle,
    /// How far the code is panned sideways; every line rides one offset.
    pub(super) pan: f32,
    /// The farthest it pans: the widest line's overhang past the code
    /// column, as the last frame measured it.
    pub(super) max_pan: f32,
    wheel: AxisLock,
    /// The lines' one stop on the keyboard's path.
    pub(super) focus: FocusHandle,
    /// The run the keyboard is on, by its first line.
    pub(super) cursor: Option<usize>,
}

impl AnnotateState {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            key: None,
            data: None,
            error: None,
            seq: 0,
            hover: None,
            scroll: UniformListScrollHandle::new(),
            pan: 0.,
            max_pan: 0.,
            wheel: AxisLock::default(),
            focus: cx.focus_handle().tab_stop(true),
            cursor: None,
        }
    }
}

/// Where a run of one origin's lines begins.
pub(super) fn run_starts(view: &FileAnnotation) -> impl Iterator<Item = usize> + '_ {
    (0..view.lines.len())
        .filter(|&index| index == 0 || view.lines[index - 1].origin != view.lines[index].origin)
}

impl WorkbenchView {
    /// Follow the model: fetch the annotation the tab asks for.
    pub(super) fn sync_annotate(&mut self, cx: &mut Context<Self>) {
        let wanted = self.app.read(cx).ui().annotate.clone();
        let Some((change, path)) = wanted else {
            if self.annotate.key.take().is_some() {
                self.annotate.seq += 1;
                self.annotate.data = None;
                self.annotate.error = None;
                self.annotate.hover = None;
                self.annotate.cursor = None;
            }
            return;
        };
        let key = (change.clone(), path.clone(), self.op_id());
        if self.annotate.key.as_ref() == Some(&key) {
            return;
        }
        // The same file at a new operation keeps what it shows until the
        // fresh answer lands; a different subject starts clean.
        let same_subject = self
            .annotate
            .key
            .as_ref()
            .is_some_and(|(c, p, _)| *c == change && *p == path);
        if !same_subject {
            self.annotate.data = None;
            self.annotate.hover = None;
            self.annotate.pan = 0.;
            self.annotate.cursor = None;
        }
        self.annotate.key = Some(key);
        self.annotate.error = None;
        self.annotate.seq += 1;
        let seq = self.annotate.seq;
        let Some(repo) = self.app.read(cx).repo_path() else {
            return;
        };
        let asked = self.app.read(cx).bg(cx, move |host| {
            host.backend()
                .file_annotate(std::path::Path::new(&repo), &change, &path)
                .map_err(crate::host::CommandError::from)
        });
        cx.spawn(async move |this, cx| {
            let result = asked.await;
            let _ = this.update(cx, |this, cx| {
                if this.annotate.seq != seq {
                    return;
                }
                match result {
                    Ok(annotation) => {
                        this.annotate.data = Some(Arc::new(annotation));
                        this.annotate.error = None;
                    }
                    Err(error) => {
                        this.annotate.data = None;
                        this.annotate.error = Some(error.message);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Step the keyboard to the next run (`forward`) or the one before,
    /// starting from the first run on screen when it is on none yet.
    fn step_origin(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(view) = self.annotate.data.clone() else {
            return;
        };
        let starts: Vec<usize> = run_starts(&view).collect();
        let Some(&first) = starts.first() else {
            return;
        };
        let next = match self.annotate.cursor {
            None => {
                let top = self.annotate.scroll.0.borrow().base_handle.top_item();
                starts
                    .iter()
                    .copied()
                    .find(|&start| start >= top)
                    .unwrap_or(first)
            }
            Some(at) if forward => starts.iter().copied().find(|&s| s > at).unwrap_or(at),
            Some(at) => starts.iter().rev().copied().find(|&s| s < at).unwrap_or(at),
        };
        self.annotate.cursor = Some(next);
        self.annotate
            .scroll
            .scroll_to_item(next, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// Return on a run: go to the change its lines came from.
    fn follow_origin(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.annotate.data.clone() else {
            return;
        };
        let origin = self
            .annotate
            .cursor
            .or_else(|| run_starts(&view).next())
            .and_then(|at| view.lines.get(at))
            .and_then(|line| view.origins.get(line.origin as usize));
        if let Some(origin) = origin {
            let id = origin.id.clone();
            self.app.update(cx, |model, cx| model.reveal_change(id, cx));
        }
    }

    /// Pan the code to `pan`, within what the widest line allows.
    fn pan_annotate(&mut self, pan: f32, cx: &mut Context<Self>) {
        let pan = pan.clamp(0., self.annotate.max_pan);
        if pan != self.annotate.pan {
            self.annotate.pan = pan;
            cx.notify();
        }
    }

    fn annotate_rows(
        &mut self,
        range: Range<usize>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let t = theme(cx);
        let Some(view) = self.annotate.data.clone() else {
            return Vec::new();
        };
        let focused = self.annotate.focus.is_focused(window);
        let ringed = keyboard_on(&self.annotate.focus, window);
        let cursor = self.annotate.cursor;
        let hover = self.annotate.hover;
        let pan = self.annotate.pan;
        let tint = Theme::blend(t.accent, 0.07, t.bg1);
        let mut rows = Vec::with_capacity(range.len());
        for index in range {
            let line = &view.lines[index];
            let run_start = index == 0 || view.lines[index - 1].origin != line.origin;
            let Some(origin) = view.origins.get(line.origin as usize) else {
                rows.push(div().h(px(LINE_H)).into_any_element());
                continue;
            };
            let tinted = hover == Some(line.origin);
            let group = SharedString::from(format!("annotate-{index}"));
            // The gutter is the pane's one verb: jump to the change this
            // line came from.
            let here = cursor == Some(index);
            let mut gutter = h_flex()
                .id(("annotate-origin", index))
                .when(run_start, |gutter| {
                    gutter
                        .role(Role::Button)
                        .aria_label(format!(
                            "{}, {}, {}{}",
                            origin.change_id,
                            origin.author,
                            from_now(&origin.timestamp),
                            if origin.summary.is_empty() {
                                String::new()
                            } else {
                                format!(", {}", origin.summary)
                            }
                        ))
                        .aria_description("Go to this change")
                })
                .when(focused && here, |gutter| gutter.aria_active_descendant())
                .when(ringed && here, |gutter| gutter.shadow(ring(t.accent)))
                .w(px(GUTTER_W))
                .h_full()
                .flex_shrink_0()
                .gap(px(8.))
                .pl(px(12.))
                .pr(px(8.))
                .border_r_1()
                .border_color(t.border2)
                .text_size(px(TEXT_XS))
                .text_color(t.text3)
                .cursor_pointer()
                .group_hover(group.clone(), |style| style.text_color(t.text1))
                .tooltip(tooltip({
                    let head = format!(
                        "{} · {} · {}",
                        origin.change_id,
                        origin.author,
                        from_now(&origin.timestamp)
                    );
                    if origin.summary.is_empty() {
                        format!("{head}\nClick to jump to this change")
                    } else {
                        format!("{head}\n{}\nClick to jump to this change", origin.summary)
                    }
                }))
                .on_mouse_down(
                    gpui::MouseButton::Right,
                    crate::views::menu::change_menu(origin.id.clone()),
                )
                .on_click({
                    let (app, id) = (self.app.clone(), origin.id.clone());
                    move |_, _, cx| app.update(cx, |model, cx| model.reveal_change(id.clone(), cx))
                });
            if run_start {
                let (lead, rest) = origin.id.split_at(origin.id.len().min(2));
                gutter = gutter
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .font_family(t.mono.clone())
                            .text_color(t.text2)
                            .group_hover(group.clone(), |style| style.text_color(t.accent_strong))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(t.accent_strong)
                                    .child(lead.to_owned()),
                            )
                            .child(rest.chars().take(6).collect::<String>()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(truncated(origin.author.clone())),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .font_family(t.mono.clone())
                            .child(short_age(&origin.timestamp)),
                    );
            }
            let mut row = div()
                .id(("annotate-row", index))
                .group(group)
                .flex()
                .flex_row()
                .h(px(LINE_H))
                .w_full()
                .overflow_hidden()
                .on_mouse_move(cx.listener({
                    let origin = line.origin;
                    move |this, _, _, cx| {
                        if this.annotate.hover != Some(origin) {
                            this.annotate.hover = Some(origin);
                            cx.notify();
                        }
                    }
                }));
            if tinted {
                row = row.bg(tint);
            }
            // A hairline where a new origin's run begins — the
            // annotation's actual information is the boundaries.
            if run_start && index > 0 {
                row = row.border_t_1().border_color(t.border2);
            }
            rows.push(
                row.child(gutter)
                    .child(
                        div()
                            .w(px(NUMBER_W))
                            .flex_shrink_0()
                            .pr(px(8.))
                            .flex()
                            .justify_end()
                            .font_family(t.mono.clone())
                            .text_size(px(TEXT_XS))
                            .line_height(px(LINE_H))
                            .text_color(Theme::fade(t.text3, 0.6))
                            .child((index + 1).to_string()),
                    )
                    .child(
                        // The code cell clips; the line inside it slides.
                        div().flex_1().min_w_0().overflow_hidden().child(
                            div()
                                .ml(px(-pan))
                                .font_family(t.mono.clone())
                                .text_size(px(CODE_SIZE))
                                .line_height(px(LINE_H))
                                .text_color(t.text1)
                                .whitespace_nowrap()
                                .child(line.text.clone()),
                        ),
                    )
                    .into_any_element(),
            );
        }
        rows
    }

    pub(super) fn render_annotate(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let t = theme(cx);
        let (change, path) = self.app.read(cx).ui().annotate.clone()?;
        // Only an answer for the shown subject renders.
        let view = self
            .annotate
            .data
            .clone()
            .filter(|view| view.id == change && view.path == path);
        let (lead, rest) = change.split_at(change.len().min(2));
        let app = self.app.clone();
        let head = h_flex()
            .flex_shrink_0()
            .min_h(px(34.))
            .gap(px(8.))
            .pl(px(16.))
            .pr(px(8.))
            .border_b_1()
            .border_color(t.border2)
            .child(icon(IconName::Eye, 13., t.text3))
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(TEXT_S))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text1)
                    .child("Annotate"),
            )
            .child(
                div()
                    .min_w(px(40.))
                    .flex_shrink_1()
                    .font_family(t.mono.clone())
                    .text_size(px(TEXT_S))
                    .text_color(t.text2)
                    .child(truncated(path.clone())),
            )
            .child(
                label_box(t.text2, t.border1)
                    .id("annotate-in")
                    .gap(px(4.))
                    .font_family(t.mono.clone())
                    .tooltip(tooltip("The file as it stands in this change's tree"))
                    .child("in")
                    .child(
                        h_flex()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(t.text1)
                                    .child(lead.to_owned()),
                            )
                            .child(rest.chars().take(6).collect::<String>()),
                    ),
            )
            .child(div().flex_1())
            .child(
                icon_btn("annotate-close", IconName::Close)
                    .size(24., 13.)
                    .tooltip("Back to the diff (esc)")
                    .on_click(move |_, _, cx| app.update(cx, |model, cx| model.close_annotate(cx))),
            );
        let note = |text: String, color| {
            div()
                .py(px(12.))
                .px(px(16.))
                .text_size(px(TEXT_S))
                .text_color(color)
                .child(text)
        };
        let body = match (&view, &self.annotate.error) {
            (Some(view), _) if view.lines.is_empty() => note(
                format!("This file is empty in {}.", short_id(&change, 8).to_owned()),
                t.text3,
            )
            .italic()
            .into_any_element(),
            (Some(view), _) => {
                // Long lines pan sideways together. The widest line sets
                // the travel; the list itself only ever scrolls down.
                let longest = view
                    .lines
                    .iter()
                    .map(|line| line.text.chars().count())
                    .max()
                    .unwrap_or(0);
                let advance =
                    crate::ui::text_width_in("0", &t.mono, CODE_SIZE, FontWeight::NORMAL, window);
                let room = (self.diff_width - GUTTER_W - NUMBER_W).max(0.);
                let max_pan = (longest as f32 * advance + CODE_PAD_R - room).max(0.);
                self.annotate.max_pan = max_pan;
                self.annotate.pan = self.annotate.pan.clamp(0., max_pan);
                // A sideways gesture is taken off the list before the list
                // spends its drift on the rows.
                let pan_first =
                    wheel_first(cx.listener(move |this, event: &ScrollEvent, _, cx| {
                        let (dx, _) = this.annotate.wheel.travel(event, LINE_H * 2.5);
                        if dx == 0. {
                            return;
                        }
                        cx.stop_propagation();
                        this.pan_annotate(this.annotate.pan - dx, cx);
                    }));
                let mut lines = div()
                    .id("annotate-body")
                    .role(Role::Group)
                    .aria_label("Lines and the changes they came from")
                    .key_context("Annotate")
                    .track_focus(&self.annotate.focus)
                    .on_action(cx.listener(|this, _: &OriginUp, _, cx| this.step_origin(false, cx)))
                    .on_action(
                        cx.listener(|this, _: &OriginDown, _, cx| this.step_origin(true, cx)),
                    )
                    .on_action(cx.listener(|this, _: &OriginFollow, _, cx| this.follow_origin(cx)))
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        vertical_only(uniform_list(
                            "annotate-lines",
                            view.lines.len(),
                            cx.processor(|this, range: Range<usize>, window, cx| {
                                this.annotate_rows(range, window, cx)
                            }),
                        ))
                        .debug_selector(|| "annotate-lines".into())
                        .track_scroll(&self.annotate.scroll)
                        .size_full(),
                    );
                // Without a trackpad, the thumb is the way sideways: it runs
                // under the code column, which is all that pans.
                if max_pan > 0. {
                    let this = cx.entity().downgrade();
                    lines = lines.child(
                        div()
                            .debug_selector(|| "annotate-pan".into())
                            .absolute()
                            .left(px(GUTTER_W + NUMBER_W))
                            .right_0()
                            .bottom_0()
                            .child(pan_scrollbar(
                                "annotate-pan",
                                self.annotate.pan,
                                max_pan,
                                move |pan, _, cx| {
                                    let _ = this.update(cx, |this, cx| this.pan_annotate(pan, cx));
                                },
                            )),
                    );
                }
                let mut body = v_flex().relative().flex_1().min_h_0().child(lines);
                if view.truncated {
                    body = body.child(
                        note(
                            "Annotation truncated — the file has more lines than Jiji renders"
                                .to_owned(),
                            t.text3,
                        )
                        .italic()
                        .border_t_1()
                        .border_color(t.border2),
                    );
                }
                body.child(pan_first).into_any_element()
            }
            (None, Some(error)) => note(error.clone(), t.warn)
                .id("annotate-error")
                .alert(error.clone())
                .into_any_element(),
            (None, None) => {
                let mut skeleton = v_flex().gap(px(9.)).p(px(16.));
                for width in [1., 0.86, 0.64, 0.78, 0.42] {
                    skeleton = skeleton.child(
                        div()
                            .h(px(12.))
                            .w(gpui::relative(width))
                            .rounded(px(4.))
                            .bg(t.bg3),
                    );
                }
                crate::ui::motion::breathing("annotate-skeleton", skeleton).into_any_element()
            }
        };
        Some(
            v_flex()
                .id("annotate")
                .role(Role::Region)
                .aria_label("File annotation")
                .flex_1()
                .min_w_0()
                .min_h_0()
                // Leaving the lines clears the same-origin tint.
                .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                    if !*hovered && this.annotate.hover.take().is_some() {
                        cx.notify();
                    }
                }))
                .child(head)
                .child(body)
                .into_any_element(),
        )
    }
}
