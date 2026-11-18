//! One file annotated line by line (`jj file annotate`): every line wears
//! the change that introduced it, grouped into runs. Read-only — the one
//! verb is the origin gutter itself, which jumps the graph to that change
//! (revealing it under a transient revset when the current lens does not
//! draw it). Replaces the diff surface while open; any selection move —
//! including that jump — closes it.

use std::ops::Range;
use std::sync::Arc;

use gpui::{
    div, prelude::*, px, uniform_list, AnyElement, Context, FontWeight, ScrollEvent, SharedString,
    UniformListScrollHandle, Window,
};
use jiji_core::snapshot::FileAnnotation;

use super::WorkbenchView;
use crate::diff::model::LINE_H;
use crate::logic::text::short_id;
use crate::theme::{theme, Theme, TEXT_S, TEXT_XS};
use crate::ui::{
    from_now, h_flex, icon, icon_btn, label_box, short_age, tooltip, truncated, v_flex,
    vertical_only, wheel_first, AxisLock, IconName,
};

const GUTTER_W: f32 = 224.;
/// The line-number column, and the room kept clear after the longest line.
const NUMBER_W: f32 = 40.;
const CODE_PAD_R: f32 = 16.;
const CODE_SIZE: f32 = 12.;

#[derive(Default)]
pub(super) struct AnnotateState {
    /// What the pane was last asked for: change, path, operation. A new
    /// operation refetches — the change may have been rewritten under it.
    key: Option<(String, String, String)>,
    data: Option<Arc<FileAnnotation>>,
    error: Option<String>,
    seq: u64,
    /// Hovering any line tints its whole origin's runs — the fastest way
    /// to read "what else came from this change" in a multi-author file.
    hover: Option<u32>,
    scroll: UniformListScrollHandle,
    /// How far the code is panned sideways; every line rides one offset.
    pan: f32,
    wheel: AxisLock,
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

    fn annotate_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let t = theme(cx);
        let Some(view) = self.annotate.data.clone() else {
            return Vec::new();
        };
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
            let mut gutter = h_flex()
                .id(("annotate-origin", index))
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
                        let pan = (this.annotate.pan - dx).clamp(0., max_pan);
                        if pan != this.annotate.pan {
                            this.annotate.pan = pan;
                            cx.notify();
                        }
                    }));
                let mut body = v_flex().relative().flex_1().min_h_0().child(
                    vertical_only(uniform_list(
                        "annotate-lines",
                        view.lines.len(),
                        cx.processor(|this, range: Range<usize>, _, cx| {
                            this.annotate_rows(range, cx)
                        }),
                    ))
                    .track_scroll(&self.annotate.scroll)
                    .flex_1()
                    .min_h_0(),
                );
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
            (None, Some(error)) => note(error.clone(), t.warn).into_any_element(),
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
                skeleton.into_any_element()
            }
        };
        Some(
            v_flex()
                .id("annotate")
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
