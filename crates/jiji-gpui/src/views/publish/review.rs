//! The Pull requests page: every open PR as a row with fetch-for-review
//! (any PR — cross-fork included — lands in a local bookmark via
//! `refs/pull/N/head`) and re-run-failed-CI, plus a by-number lookup for
//! PRs the open list cannot see.

use gpui::{div, prelude::*, px, AnyElement, Context, FontWeight, SharedString, Window};
use jiji_core::snapshot::RepoSnapshot;
use jiji_forge::PrSummary;

use super::{tone_color, PublishView};
use crate::logic::menu::review_bookmark;
use crate::logic::publish::{rerun_summary, review_rows};
use crate::theme::{theme, RADIUS_L, TEXT_M, TEXT_S, TEXT_XS, TOOLBAR_H};
use crate::ui::{
    btn, h_flex, input::field, plan_panel, ref_label, scrollbar, tooltip, truncated, v_flex,
};

const MAX_REVIEW_ROWS: usize = 100;

impl PublishView {
    /// The fetch-for-review panel: pick a PR (row click or number lookup),
    /// name the bookmark, confirm.
    pub(super) fn open_review_panel(
        &mut self,
        pr: PrSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = review_bookmark(pr.number);
        self.review_for = Some(pr);
        self.review_error = None;
        self.review_done = None;
        self.review_name.update(cx, |input, cx| {
            input.set_text(&name, cx);
            input.select_all_text(cx);
            input.focus(window);
        });
        cx.notify();
    }

    pub(super) fn close_review_panel(&mut self, cx: &mut Context<Self>) {
        self.review_for = None;
        self.review_error = None;
        cx.notify();
    }

    fn review_name_taken(&self, snapshot: &RepoSnapshot, cx: &gpui::App) -> bool {
        let name = self.review_name.read(cx).text().trim().to_owned();
        snapshot
            .bookmarks
            .iter()
            .any(|bookmark| bookmark.is_local && bookmark.name == name)
    }

    pub(super) fn fetch_for_review(&mut self, cx: &mut Context<Self>) {
        let (Some(pr), Some(snapshot)) = (self.review_for.clone(), self.snapshot.clone()) else {
            return;
        };
        let remote = self
            .app
            .read(cx)
            .forge
            .status
            .as_ref()
            .and_then(|status| status.repo.as_ref())
            .map(|repo| repo.remote.clone());
        let name = self.review_name.read(cx).text().trim().to_owned();
        let Some(remote) = remote else {
            return;
        };
        if self.review_busy || name.is_empty() || self.review_name_taken(&snapshot, cx) {
            return;
        }
        self.review_busy = true;
        self.review_error = None;
        cx.notify();
        let bookmark = name.clone();
        let task = self.app.update(cx, |model, cx| {
            model.mutate(cx, move |backend, path| {
                backend.fetch_pr_head(path, &remote, pr.number, &bookmark)
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.review_busy = false;
                match result {
                    Ok(outcome) => {
                        this.review_done = Some((name, outcome.target_change));
                        this.review_for = None;
                    }
                    Err(error) => this.review_error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The by-number lookup feeding the same panel.
    pub(super) fn lookup_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let number = self.lookup.read(cx).text().trim().parse::<u64>().ok();
        let (Some(number), Some(repo)) = (number.filter(|n| *n > 0), self.repo_path(cx)) else {
            return;
        };
        if self.lookup_busy {
            return;
        }
        self.lookup_busy = true;
        self.lookup_error = None;
        cx.notify();
        let found = self
            .app
            .read(cx)
            .bg(cx, move |host| host.forge_pr(&repo, number));
        cx.spawn_in(window, async move |this, cx| {
            let result = found.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.lookup_busy = false;
                match result {
                    Ok(pr) => this.open_review_panel(pr, window, cx),
                    Err(error) => this.lookup_error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Re-run failed CI, per row; the note under the bar tells what
    /// happened, and the badge refresh follows GitHub's answer.
    fn rerun_ci(&mut self, number: u64, cx: &mut Context<Self>) {
        let Some(repo) = self.repo_path(cx) else {
            return;
        };
        if self.rerun_busy.is_some() {
            return;
        }
        self.rerun_busy = Some(number);
        self.rerun_note = None;
        cx.notify();
        let asked = self
            .app
            .read(cx)
            .bg(cx, move |host| host.rerun_failed_ci(&repo, number));
        cx.spawn(async move |this, cx| {
            let result = asked.await;
            let _ = this.update(cx, |this, cx| {
                this.rerun_busy = None;
                this.rerun_note = Some(match result {
                    Ok(report) => {
                        this.app.update(cx, |model, cx| model.refresh_forge_prs(cx));
                        format!("#{number}: {}", rerun_summary(&report))
                    }
                    Err(error) => format!("#{number}: {}", error.message),
                });
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_review_page(
        &mut self,
        snapshot: &RepoSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let (prs, owner, prs_loading) = {
            let forge = &self.app.read(cx).forge;
            (
                forge.prs.clone(),
                forge
                    .status
                    .as_ref()
                    .and_then(|status| status.repo.as_ref())
                    .map(|repo| repo.owner.clone()),
                forge.prs_loading,
            )
        };
        let rows = review_rows(prs.as_deref(), owner.as_deref());
        let count = rows.len();
        let lookup_text = self.lookup.read(cx).text().trim().to_owned();
        let lookup_valid =
            !lookup_text.is_empty() && lookup_text.bytes().all(|b| b.is_ascii_digit());

        let mut page = v_flex().flex_1().min_h_0().child(
            h_flex()
                .flex_shrink_0()
                .h(px(TOOLBAR_H))
                .gap(px(12.))
                .pl(px(16.))
                .pr(px(8.))
                .border_b_1()
                .border_color(t.border2)
                .child(
                    div()
                        .text_size(px(TEXT_S))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.text2)
                        .child(if prs_loading && prs.is_none() {
                            "Finding pull requests…".to_owned()
                        } else {
                            format!(
                                "{count} open pull request{}",
                                if count == 1 { "" } else { "s" }
                            )
                        }),
                )
                .child(div().flex_1())
                .child(field(&self.lookup, window, cx).w(px(110.)).h(px(26.)))
                .child(
                    btn(
                        "review-lookup",
                        if self.lookup_busy {
                            "Finding…"
                        } else {
                            "Look up"
                        },
                    )
                    .disabled(self.lookup_busy || !lookup_valid)
                    .on_click(cx.listener(|this, _, window, cx| this.lookup_pr(window, cx))),
                ),
        );
        let notice = |text: String, error: bool| {
            div()
                .flex_shrink_0()
                .py(px(6.))
                .px(px(16.))
                .border_b_1()
                .border_color(t.border2)
                .text_size(px(TEXT_S))
                .text_color(if error { t.danger } else { t.text2 })
                .child(text)
        };
        if let Some(error) = &self.lookup_error {
            page = page.child(notice(error.clone(), true));
        }
        if let Some(pr) = self.review_for.clone() {
            let taken = self.review_name_taken(snapshot, cx);
            let empty = self.review_name.read(cx).text().trim().is_empty();
            let mut panel = plan_panel(cx)
                .flex_shrink_0()
                .m(px(12.))
                .rounded(px(RADIUS_L))
                .child(
                    h_flex()
                        .gap(px(8.))
                        .min_w_0()
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(TEXT_M))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(t.text1)
                                .child(format!("Fetch #{} for review", pr.number)),
                        )
                        .child(
                            div()
                                .min_w(px(40.))
                                .flex_shrink()
                                .text_size(px(TEXT_S))
                                .text_color(t.text3)
                                .child(truncated(pr.title.clone())),
                        ),
                )
                .child(
                    h_flex()
                        .gap(px(8.))
                        .mt(px(10.))
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(TEXT_S))
                                .text_color(t.text2)
                                .child("Local bookmark"),
                        )
                        .child(
                            field(&self.review_name, window, cx)
                                .flex_1()
                                .font_family(t.mono.clone()),
                        )
                        .child(
                            btn(
                                "review-fetch",
                                if self.review_busy {
                                    "Fetching…"
                                } else {
                                    "Fetch"
                                },
                            )
                            .primary()
                            .disabled(self.review_busy || empty || taken)
                            .on_click(cx.listener(|this, _, _, cx| this.fetch_for_review(cx))),
                        )
                        .child(
                            btn("review-cancel", "Cancel")
                                .ghost()
                                .disabled(self.review_busy)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.close_review_panel(cx)),
                                ),
                        ),
                );
            if taken {
                panel = panel.child(
                    div()
                        .mt(px(8.))
                        .text_size(px(TEXT_S))
                        .text_color(t.warn)
                        .child("This bookmark already exists. Choose another name."),
                );
            }
            if let Some(error) = &self.review_error {
                panel = panel.child(
                    div()
                        .mt(px(8.))
                        .text_size(px(TEXT_S))
                        .text_color(t.danger)
                        .child(error.clone()),
                );
            }
            page = page.child(panel);
        }
        if let Some((bookmark, target)) = self.review_done.clone() {
            let mut line = h_flex()
                .flex_shrink_0()
                .gap(px(8.))
                .py(px(6.))
                .px(px(16.))
                .border_b_1()
                .border_color(t.border2)
                .text_size(px(TEXT_S))
                .text_color(t.text2)
                .child("Fetched into")
                .child(ref_label(None, bookmark, cx));
            if let Some(target) = target {
                line = line.child(self.text_button(
                    "review-open",
                    "Open in workbench ↗",
                    move |this, _, cx| {
                        let target = target.clone();
                        this.app
                            .update(cx, |model, cx| model.jump_to_change(&target, cx));
                    },
                    cx,
                ));
            }
            page = page.child(line);
        }
        if let Some(note) = &self.rerun_note {
            page = page.child(notice(note.clone(), false));
        }

        let mut list = v_flex()
            .id("review-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.review_scroll);
        for row in rows.iter().take(MAX_REVIEW_ROWS) {
            let pr = row.pr;
            let number = pr.number;
            let url = pr.url.clone();
            let mut actions = h_flex().flex_shrink_0().gap(px(4.));
            if row.can_rerun {
                actions = actions.child(
                    btn(
                        SharedString::from(format!("review-rerun-{number}")),
                        "Re-run CI",
                    )
                    .ghost()
                    .disabled(self.rerun_busy.is_some())
                    .on_click(cx.listener(move |this, _, _, cx| this.rerun_ci(number, cx))),
                );
            }
            let fetch = pr.clone();
            actions = actions.child(
                btn(
                    SharedString::from(format!("review-fetch-{number}")),
                    "Fetch for review",
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_review_panel(fetch.clone(), window, cx)
                })),
            );
            let mut line = h_flex()
                .id(SharedString::from(format!("review-row-{number}")))
                .flex_shrink_0()
                .gap(px(12.))
                .min_h(px(44.))
                .py(px(5.))
                .pl(px(16.))
                .pr(px(12.))
                .border_b_1()
                .border_color(t.border2)
                .hover(|style| style.bg(t.bg_hover))
                .on_mouse_down(
                    gpui::MouseButton::Right,
                    crate::views::menu::pr_menu(number),
                )
                .child(
                    div()
                        .w(px(44.))
                        .flex_shrink_0()
                        .font_family(t.mono.clone())
                        .text_size(px(TEXT_S))
                        .text_color(t.text3)
                        .child(format!("#{number}")),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(1.))
                        .child(
                            div()
                                .id(SharedString::from(format!("review-title-{number}")))
                                .min_w_0()
                                .text_size(px(TEXT_M))
                                .text_color(t.text1)
                                .cursor_pointer()
                                .hover(|style| style.text_color(t.accent_strong))
                                .tooltip(tooltip(format!("Open #{number} on GitHub")))
                                .on_click(move |_, _, cx| cx.open_url(&url))
                                .child(truncated(pr.title.clone())),
                        )
                        .child(
                            h_flex()
                                .gap(px(8.))
                                .min_w_0()
                                .text_size(px(TEXT_XS))
                                .text_color(t.text3)
                                .child(div().flex_shrink_0().child(if pr.is_draft {
                                    "Draft"
                                } else {
                                    "Open"
                                }))
                                .child(div().w(px(1.)).h(px(9.)).flex_shrink_0().bg(t.border1))
                                .child(
                                    div()
                                        .min_w(px(40.))
                                        .flex_shrink()
                                        .font_family(t.mono.clone())
                                        .child(truncated(format!(
                                            "{} → {}",
                                            row.head_label, pr.base_branch
                                        ))),
                                ),
                        ),
                );
            if let Some(checks) = &row.badge.checks {
                line = line.child(
                    div()
                        .id(SharedString::from(format!("review-checks-{number}")))
                        .w(px(16.))
                        .flex_shrink_0()
                        .flex()
                        .justify_center()
                        .text_size(px(TEXT_S))
                        .text_color(tone_color(&t, checks.tone))
                        .tooltip(tooltip(checks.label))
                        .child(checks.glyph),
                );
            }
            list = list.child(line.child(actions));
        }
        if rows.is_empty() {
            list = list.child(
                div()
                    .py(px(10.))
                    .px(px(16.))
                    .text_size(px(TEXT_S))
                    .text_color(t.text3)
                    .child(if prs_loading {
                        "Finding pull requests…"
                    } else {
                        "No open pull requests."
                    }),
            );
        }
        page.child(
            div()
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(list)
                .child(scrollbar("review-scrollbar", &self.review_scroll)),
        )
        .into_any_element()
    }
}
