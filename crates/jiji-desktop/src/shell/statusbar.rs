//! The status line: what just happened (with the way back one click
//! away), what a drag would do, or the repo's latest operation — plus the
//! activity chips that stay visible from every section.

use gpui::{div, prelude::*, px, Context, FontWeight};

use super::Shell;
use crate::app::Section;
use crate::logic::autoland::{self, ChipTone};
use crate::logic::inspect::Tone;
use crate::logic::text::{abbreviate_hashes, short_id};
use crate::theme::{theme, Theme, RADIUS_S, STATUSBAR_H, TEXT_XS};
use crate::ui::{dot, h_flex, icon, tooltip, IconName, Tabular as _};

impl Shell {
    pub(super) fn render_statusbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let model = self.app.read(cx);
        let snapshot = model.snapshot().cloned();
        let ui = model.ui();
        let quiet_dot = Theme::fade(t.text3, 0.7);
        let msg = |text: String| div().min_w_0().truncate().text_color(t.text2).child(text);
        let dismiss = |id: &'static str| {
            div()
                .id(id)
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .size(px(16.))
                .rounded(px(RADIUS_S))
                .cursor_pointer()
                .hover(|style| style.bg(t.bg_hover))
                .child(icon(IconName::Close, 10., t.text3))
        };

        let mut bar = h_flex()
            .h(px(STATUSBAR_H))
            .flex_shrink_0()
            .gap(px(8.))
            .px(px(12.))
            .border_t_1()
            .border_color(t.border2)
            .text_size(px(TEXT_XS))
            .text_color(t.text3);

        if let Some(hint) = &model.drag_hint {
            // The live meaning of the drag gesture, mirroring the plan card.
            bar = match hint.allowed {
                Some(true) => bar
                    .child(dot(t.accent, 6.))
                    .child(msg(format!("{} — release to apply", hint.text))),
                Some(false) => bar.child(dot(t.danger, 6.)).child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(t.danger)
                        .child(hint.text.clone()),
                ),
                None => bar.child(dot(quiet_dot, 6.)).child(msg(hint.text.clone())),
            };
        } else if let Some(error) = &ui.error {
            // Errors hold the line until the next snapshot or a dismissal;
            // the full text (jj's multi-line hints included) reads on hover.
            let app = self.app.clone();
            bar = bar
                .child(dot(t.danger, 6.))
                .child(
                    div()
                        .id("status-error")
                        .min_w_0()
                        .truncate()
                        .text_color(t.danger)
                        .tooltip(tooltip(error.clone()))
                        .child(error.lines().next().unwrap_or_default().to_owned()),
                )
                .child(
                    dismiss("error-dismiss")
                        .tooltip(tooltip("Dismiss"))
                        .on_click(move |_, _, cx| app.update(cx, |m, cx| m.dismiss_error(cx))),
                );
        } else if let Some(crumb) = &ui.last_mutation {
            // A just-run mutation takes the line for a few seconds, with
            // the way back one click away.
            let app = self.app.clone();
            let op_id = crumb.outcome.operation_id.clone();
            let mut link = h_flex()
                .id("breadcrumb")
                .gap(px(8.))
                .min_w_0()
                .text_color(t.text1)
                .cursor_pointer()
                .hover(|style| style.text_color(t.accent_strong))
                .tooltip(tooltip("Show in Operations"))
                .on_mouse_down(gpui::MouseButton::Right, |event, _, cx| {
                    cx.stop_propagation();
                    crate::app::app(cx).update(cx, |model, cx| {
                        model.open_breadcrumb_menu(event.position, cx)
                    });
                })
                .on_click(move |_, _, cx| {
                    app.update(cx, |m, cx| {
                        m.dismiss_breadcrumb(cx);
                        m.go_to_section(Section::Operations, cx);
                    })
                })
                .child(div().truncate().child(crumb.outcome.summary.clone()));
            if let Some(op) = &op_id {
                link = link.child(
                    div()
                        .flex_shrink_0()
                        .font_family(t.mono.clone())
                        .text_color(t.text3)
                        .child(short_id(op, 8).to_owned()),
                );
            }
            bar = bar.child(dot(t.accent, 6.)).child(link);
            if op_id.is_some() {
                let undoing = self.undoing;
                let mut undo = h_flex()
                    .id("breadcrumb-undo")
                    .flex_shrink_0()
                    .gap(px(4.))
                    .h(px(18.))
                    .px(px(6.))
                    .rounded(px(RADIUS_S))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.accent_strong)
                    .child(icon(IconName::Undo, 11., t.accent_strong))
                    .child(if undoing { "Undoing…" } else { "Undo" });
                undo = if undoing {
                    undo.opacity(0.6)
                } else {
                    undo.cursor_pointer()
                        .hover(|style| style.bg(t.accent_dim))
                        .tooltip(tooltip("Revert this operation"))
                        .on_click(cx.listener(|this, _, _, cx| this.undo(cx)))
                };
                bar = bar.child(undo);
            }
        } else if model.crashed {
            // The panic that ended the last session is in the log; the tabs
            // it had open are back.
            let app = self.app.clone();
            bar = bar
                .child(dot(t.warn, 6.))
                .child(msg("The last session ended unexpectedly.".into()))
                .child(
                    div()
                        .id("crash-log")
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .h(px(18.))
                        .px(px(6.))
                        .rounded(px(RADIUS_S))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.accent_strong)
                        .cursor_pointer()
                        .hover(|style| style.bg(t.accent_dim))
                        .tooltip(tooltip("Open the log, where the error was recorded"))
                        .on_click(|_, _, cx| cx.open_with_system(&crate::diagnostics::log_path()))
                        .child("Show Log"),
                )
                .child(
                    dismiss("crash-dismiss")
                        .tooltip(tooltip("Dismiss"))
                        .on_click(move |_, _, cx| {
                            app.update(cx, |m, cx| m.dismiss_crash_notice(cx))
                        }),
                );
        } else if let Some(op) = snapshot.as_ref().and_then(|s| {
            // Every look at the repo snapshots the working copy, so the
            // newest operation is nearly always that; the line names the
            // newest one that did something.
            s.operations
                .iter()
                .find(|op| !op.is_snapshot)
                .or(s.operations.first())
        }) {
            // The operation's station, in its family's hue as the journal
            // draws it.
            let hue = crate::views::operations::op_hue(
                crate::logic::ops::op_kind(&op.description, op.is_snapshot),
                &t,
            );
            bar = bar
                .child(crate::ui::node_mark(
                    crate::ui::Mark::Station { filled: false },
                    hue,
                    8.,
                ))
                .child(msg(abbreviate_hashes(&op.description)))
                .child(
                    div()
                        .id("status-op-age")
                        .flex_shrink_0()
                        .tabular()
                        .tooltip(tooltip(crate::ui::absolute_time(&op.timestamp)))
                        .child(crate::ui::from_now(&op.timestamp)),
                );
            if snapshot.as_ref().is_some_and(|s| s.backend == "mock") {
                bar = bar.child(div().flex_shrink_0().text_color(t.warn).child("mock data"));
            }
        } else {
            bar = bar
                .child(dot(quiet_dot, 6.))
                .child(msg("Idle — open a repository to begin".into()));
        }

        bar = bar.child(div().flex_1());

        // The activity chips: every auto-land job story stays visible from
        // every section (the live job first), and clicking one lands on
        // the Publish job cards.
        let repo_path = snapshot.as_ref().map(|s| s.repo_path.as_str());
        for (ix, status) in autoland::visible_jobs(&model.autoland.jobs, repo_path)
            .into_iter()
            .enumerate()
        {
            let chip = autoland::chip(status);
            let color = match chip.tone {
                ChipTone::Accent => t.accent_strong,
                ChipTone::Tone(Tone::Ok) => t.ok,
                ChipTone::Tone(Tone::Warn) => t.warn,
                ChipTone::Tone(Tone::Danger) => t.danger,
                ChipTone::Tone(Tone::Muted) => t.text3,
            };
            let label = if model.autoland.stopping && status.live {
                format!("Auto-land {}: stopping…", status.record.state.head_bookmark)
            } else {
                chip.label
            };
            let app = self.app.clone();
            let mut wrap = h_flex().flex_shrink_0().gap(px(2.)).child(
                h_flex()
                    .id(("autoland-chip", ix))
                    .gap(px(5.))
                    .h(px(18.))
                    .px(px(6.))
                    .max_w(px(280.))
                    .rounded(px(RADIUS_S))
                    .text_color(color)
                    .cursor_pointer()
                    .hover(|style| style.bg(t.bg_hover))
                    .tooltip(tooltip(autoland::story(status)))
                    .on_click(move |_, _, cx| {
                        app.update(cx, |m, cx| m.go_to_section(Section::Publish, cx))
                    })
                    .child(dot(color, 6.))
                    .child(div().truncate().child(label)),
            );
            if chip.dismissable {
                let app = self.app.clone();
                let status = status.clone();
                wrap = wrap.child(
                    dismiss("autoland-dismiss")
                        .id(("autoland-dismiss", ix))
                        .tooltip(tooltip("Dismiss"))
                        .on_click(move |_, _, cx| {
                            app.update(cx, |m, cx| m.dismiss_autoland(&status, cx))
                        }),
                );
            }
            bar = bar.child(wrap);
        }

        if let Some(version) = &model.update.version {
            // A downloaded update waits for the user; nothing restarts on
            // its own.
            let installing = model.update.installing;
            let app = self.app.clone();
            let mut el = h_flex()
                .id("app-update")
                .flex_shrink_0()
                .gap(px(4.))
                .h(px(18.))
                .px(px(6.))
                .rounded(px(RADIUS_S))
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.accent_strong)
                .child(icon(IconName::ArrowUp, 11., t.accent_strong))
                .child(if installing {
                    "Updating…".to_owned()
                } else {
                    format!("Restart to update to {version}")
                });
            if !installing {
                el = el
                    .cursor_pointer()
                    .hover(|style| style.bg(t.accent_dim))
                    .tooltip(tooltip(format!(
                        "Jiji {version} is downloaded. Restart to install it."
                    )))
                    .on_click(move |_, _, cx| app.update(cx, |m, cx| m.restart_to_update(cx)));
            }
            bar = bar.child(el);
        }

        // The backend names itself only when it is not the usual one.
        if let Some(snapshot) = snapshot.as_ref().filter(|s| s.backend != "jj-lib") {
            bar = bar.child(
                div()
                    .id("backend")
                    .flex_shrink_0()
                    .font_family(t.mono.clone())
                    .tooltip(tooltip("Repository backend"))
                    .child(snapshot.backend.clone()),
            );
        }
        bar
    }
}
