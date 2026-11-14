//! The welcome screen: an empty tab. Open a repository, pick a recent one,
//! or install the terminal launcher.

use gpui::{div, prelude::*, px, Context, Entity, FontWeight, Render, ScrollHandle, Window};

use crate::app::{app, AppModel};
use crate::theme::{theme, RADIUS_L, RADIUS_M, RADIUS_S, TEXT_M, TEXT_S, TEXT_XL, TEXT_XS};
use crate::ui::ScrollY as _;
use crate::ui::{btn, h_flex, icon, kbd, scrollbar, tooltip, v_flex, IconName};

pub struct WelcomeView {
    app: Entity<AppModel>,
    scroll: ScrollHandle,
}

impl WelcomeView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let app = app(cx);
        cx.observe(&app, |_, _, cx| cx.notify()).detach();
        Self {
            app,
            scroll: ScrollHandle::new(),
        }
    }
}

fn short_path(path: &str) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        if let Some(rest) = path.strip_prefix(home.as_ref()) {
            return format!("~{rest}");
        }
    }
    path.to_owned()
}

/// "just now", "5m ago", "3h ago", "2d ago" for a unix-ms instant.
fn from_now(at_ms: u64) -> String {
    let minutes = crate::store::now_ms().saturating_sub(at_ms) / 60_000;
    match minutes {
        0 => "just now".into(),
        1..=59 => format!("{minutes}m ago"),
        60..=1439 => format!("{}h ago", minutes / 60),
        _ => format!("{}d ago", minutes / 1440),
    }
}

impl Render for WelcomeView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let model = self.app.read(cx);
        let opening = model.tab().opening;
        let error = model.ui().error.clone();
        let recents = model.recent_repos.clone();
        let cli = &model.cli;
        // The one-time offer to install the terminal launcher. A foreign
        // `jiji` on PATH means they already have a CLI by that name — no
        // offer. After an install from this card, `just_installed` keeps
        // it up as the confirmation of where the command landed.
        let cli_offer = cli.status.clone().filter(|status| {
            !cli.dismissed && status.conflict.is_none() && (!status.installed || cli.just_installed)
        });
        let installing = cli.installing;

        let code = |text: &'static str| {
            div()
                .font_family(t.mono.clone())
                .text_size(px(TEXT_XS))
                .text_color(t.text1)
                .child(text)
        };

        let open_app = self.app.clone();
        let palette_app = self.app.clone();
        let mut stage = v_flex()
            .w(px(560.))
            .max_w_full()
            .gap(px(20.))
            .px(px(24.))
            .pt(px(32.))
            .pb(px(96.))
            .child(
                h_flex()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(40.))
                            .rounded(px(10.))
                            .bg(t.bg3)
                            .border_1()
                            .border_color(t.border1)
                            .child(icon(IconName::Logo, 25., t.accent_strong)),
                    )
                    .child(
                        v_flex()
                            .gap(px(1.))
                            .child(
                                div()
                                    .text_size(px(TEXT_XL))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .line_height(gpui::relative(1.2))
                                    .child("Jiji"),
                            )
                            .child(
                                h_flex()
                                    .gap(px(4.))
                                    .text_color(t.text2)
                                    .child("A workbench for")
                                    .child(
                                        div()
                                            .font_family(t.mono.clone())
                                            .text_size(px(TEXT_S))
                                            .text_color(t.text1)
                                            .child("jj"),
                                    )
                                    .child("repositories"),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        btn(
                            "open-repo",
                            if opening {
                                "Opening…"
                            } else {
                                "Open Repository…"
                            },
                        )
                        .primary()
                        .large()
                        .icon(IconName::Folder)
                        .disabled(opening)
                        .on_click(move |_, _, cx| open_app.update(cx, |m, cx| m.choose_repo(cx))),
                    )
                    .child(
                        btn("open-palette", "Command Palette")
                            .large()
                            .trailing(kbd("⌘K", cx))
                            .on_click(move |_, _, cx| {
                                palette_app.update(cx, |m, cx| m.toggle_palette(cx))
                            }),
                    ),
            )
            .child(match error {
                Some(error) => div()
                    .text_size(px(TEXT_S))
                    .text_color(t.danger)
                    .child(error),
                None => div()
                    .text_size(px(TEXT_S))
                    .text_color(t.text3)
                    .child("A plain Git repository works too: Jiji offers to set jj up in it."),
            });

        if !recents.is_empty() {
            let mut list = v_flex()
                .rounded(px(RADIUS_L))
                .border_1()
                .border_color(t.border2)
                .overflow_hidden();
            for (ix, repo) in recents.into_iter().enumerate() {
                let group = format!("recent-{ix}");
                let open_app = self.app.clone();
                let forget_app = self.app.clone();
                let (open_path, forget_path) = (repo.path.clone(), repo.path.clone());
                let mut row = h_flex()
                    .id(("recent", ix))
                    .group(group.clone())
                    .h(px(34.))
                    .pl(px(12.))
                    .pr(px(6.))
                    .gap(px(12.))
                    .cursor_pointer()
                    .hover(|style| style.bg(t.bg_hover))
                    .on_click(move |_, _, cx| {
                        let path = open_path.clone();
                        open_app.update(cx, |m, cx| m.open_repo_at(path, cx))
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .max_w(px(200.))
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(repo.name.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(t.mono.clone())
                            .text_size(px(TEXT_XS))
                            .text_color(t.text3)
                            .child(short_path(&repo.path)),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(TEXT_XS))
                            .text_color(t.text3)
                            .child(from_now(repo.last_opened_at)),
                    )
                    .child(
                        div()
                            .id(("forget", ix))
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .justify_center()
                            .size(px(22.))
                            .rounded(px(RADIUS_S))
                            .opacity(0.)
                            .group_hover(group, |style| style.opacity(1.))
                            .hover(|style| style.bg(t.bg_hover))
                            .tooltip(tooltip("Remove from recent repositories"))
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                forget_app
                                    .update(cx, |m, cx| m.forget_recent_repo(&forget_path, cx))
                            })
                            .child(icon(IconName::Close, 11., t.text3)),
                    );
                if ix > 0 {
                    row = row.border_t_1().border_color(t.border2);
                }
                list = list.child(row);
            }
            stage = stage.child(
                v_flex()
                    .child(
                        div()
                            .pb(px(6.))
                            .text_size(px(TEXT_S))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.text2)
                            .child("Recent"),
                    )
                    .child(list),
            );
        }

        if let Some(status) = cli_offer {
            let mut card = h_flex()
                .gap(px(12.))
                .p(px(12.))
                .rounded(px(RADIUS_L))
                .border_1()
                .border_color(t.border2)
                .bg(t.bg2);
            let badge = |glyph, color| {
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .size(px(28.))
                    .rounded(px(RADIUS_M))
                    .bg(t.bg3)
                    .child(icon(glyph, 15., color))
            };
            let title = |text: gpui::AnyElement| {
                div()
                    .text_size(px(TEXT_S))
                    .font_weight(FontWeight::MEDIUM)
                    .child(text)
            };
            let body = |children: Vec<gpui::AnyElement>| {
                h_flex()
                    .flex_wrap()
                    .gap(px(3.))
                    .text_size(px(TEXT_S))
                    .text_color(t.text2)
                    .children(children)
            };
            if status.installed {
                card = card.child(badge(IconName::Check, t.ok)).child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.))
                        .child(title(
                            h_flex()
                                .gap(px(3.))
                                .child("The")
                                .child(code("jiji"))
                                .child("command is ready")
                                .into_any_element(),
                        ))
                        .child(body(vec![
                            format!("Installed at {} — run", status.path.unwrap_or_default())
                                .into_any_element(),
                            code("jiji").into_any_element(),
                            "inside any repository to open it here.".into_any_element(),
                        ])),
                );
            } else {
                let dismiss_app = self.app.clone();
                let install_app = self.app.clone();
                card = card
                    .child(badge(IconName::Terminal, t.text3))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.))
                            .child(title("Open repos from your terminal".into_any_element()))
                            .child(body(vec![
                                "Install the".into_any_element(),
                                code("jiji").into_any_element(),
                                "command — running".into_any_element(),
                                code("jiji").into_any_element(),
                                "in a repository opens it here.".into_any_element(),
                            ])),
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap(px(6.))
                            .child(btn("cli-not-now", "Not now").ghost().on_click(
                                move |_, _, cx| {
                                    dismiss_app.update(cx, |m, cx| m.dismiss_cli_offer(cx))
                                },
                            ))
                            .child(
                                btn(
                                    "cli-install-offer",
                                    if installing {
                                        "Installing…"
                                    } else {
                                        "Install"
                                    },
                                )
                                .disabled(installing)
                                .on_click(move |_, _, cx| {
                                    install_app.update(cx, |m, cx| {
                                        let task = m.install_cli(cx);
                                        m.report(None, task, cx);
                                    })
                                }),
                            ),
                    );
            }
            stage = stage.child(card);
        }

        let hint = |keys: &'static str, label: &'static str| {
            h_flex()
                .gap(px(6.))
                .text_size(px(TEXT_S))
                .text_color(t.text3)
                .child(kbd(keys, cx))
                .child(label)
        };
        stage = stage.child(
            h_flex()
                .gap(px(16.))
                .child(hint("⌘O", "Open"))
                .child(hint("⌘T", "New tab"))
                .child(hint("⌘⇧T", "Reopen closed tab")),
        );

        div()
            .relative()
            .size_full()
            .text_size(px(TEXT_M))
            .child(
                div()
                    .id("welcome-scroll")
                    .size_full()
                    .scroll_y()
                    .track_scroll(&self.scroll)
                    .child(
                        v_flex()
                            .min_h_full()
                            .items_center()
                            .justify_center()
                            .child(stage),
                    ),
            )
            .child(scrollbar("welcome-scrollbar", &self.scroll))
    }
}
