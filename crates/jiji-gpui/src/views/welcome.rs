//! The welcome screen: an empty tab. Open a repository, pick a recent one,
//! or install the terminal launcher.

use gpui::{
    canvas, div, fill, point, prelude::*, px, quad, BorderStyle, Bounds, Context, Entity,
    FontWeight, Hsla, PathBuilder, Pixels, Point, Render, ScrollHandle, Window,
};

use crate::app::{app, AppModel};
use crate::theme::{
    theme, Theme, RADIUS_L, RADIUS_M, RADIUS_S, STATUSBAR_H, TEXT_L, TEXT_M, TEXT_S, TEXT_XL,
    TEXT_XS, TITLEBAR_H,
};
use crate::ui::ScrollY as _;
use crate::ui::{
    btn, h_flex, icon, kbd, scrollbar, tooltip, truncated, v_flex, IconName, Tabular as _,
};

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

/// The map the app draws, told left to right: a trunk of history, one
/// line that branched and landed, the working line with you on it, and one
/// more under way. Drawn in the palette's own lane hues.
fn transit_map(t: &Theme) -> impl IntoElement {
    const W: f32 = 640.;
    const H: f32 = 132.;
    let trunk = Theme::blend(t.text3, 0.7, t.bg1);
    let (landed, working, other) = (t.lane(1), t.lane(0), t.lane(4));
    let (wc, bg) = (t.working_copy, t.bg1);
    canvas(
        |_, _, _| (),
        move |bounds: Bounds<Pixels>, (), window, _| {
            let o = bounds.origin;
            let at = |x: f32, y: f32| -> Point<Pixels> { point(o.x + px(x), o.y + px(y)) };
            let line = |window: &mut Window, color: Hsla, dashed: bool, points: &[(f32, f32)]| {
                let mut builder = PathBuilder::stroke(px(3.));
                if dashed {
                    builder = builder.dash_array(&[px(3.), px(4.)]);
                }
                builder.move_to(at(points[0].0, points[0].1));
                for pair in points[1..].chunks(1) {
                    builder.line_to(at(pair[0].0, pair[0].1));
                }
                if let Ok(path) = builder.build() {
                    window.paint_path(path, color);
                }
            };
            // An S-bend between two tracks, as the graph draws lane changes.
            let bend = |window: &mut Window, color: Hsla, from: (f32, f32), to: (f32, f32)| {
                let mid = (from.0 + to.0) / 2.;
                let mut builder = PathBuilder::stroke(px(3.));
                builder.move_to(at(from.0, from.1));
                builder.cubic_bezier_to(at(to.0, to.1), at(mid, from.1), at(mid, to.1));
                if let Ok(path) = builder.build() {
                    window.paint_path(path, color);
                }
            };
            let station = |window: &mut Window, color: Hsla, x: f32, y: f32| {
                let r = 6.;
                window.paint_quad(quad(
                    Bounds::from_corners(at(x - r, y - r), at(x + r, y + r)),
                    px(r),
                    bg,
                    px(2.5),
                    color,
                    BorderStyle::Solid,
                ));
            };
            let diamond = |window: &mut Window, x: f32, y: f32| {
                let d = 6.;
                let mut builder = PathBuilder::fill();
                builder.move_to(at(x, y - d));
                builder.line_to(at(x + d, y));
                builder.line_to(at(x, y + d));
                builder.line_to(at(x - d, y));
                builder.close();
                if let Ok(path) = builder.build() {
                    window.paint_path(path, trunk);
                }
            };
            let (base, mid_track, top_track) = (112., 72., 28.);

            // The trunk, its far history and its future dashed.
            line(window, trunk, true, &[(0., base), (36., base)]);
            line(window, trunk, false, &[(36., base), (W - 36., base)]);
            line(window, trunk, true, &[(W - 36., base), (W, base)]);

            // A line that branched and landed.
            bend(window, landed, (104., base), (158., mid_track));
            line(
                window,
                landed,
                false,
                &[(158., mid_track), (290., mid_track)],
            );
            bend(window, landed, (290., mid_track), (344., base));
            // The working line: you are at its head.
            bend(window, working, (236., base), (290., top_track));
            line(
                window,
                working,
                false,
                &[(290., top_track), (484., top_track)],
            );
            // One more under way.
            bend(window, other, (404., base), (458., mid_track));
            line(
                window,
                other,
                false,
                &[(458., mid_track), (530., mid_track)],
            );

            for x in [104., 236., 344., 404., 560.] {
                diamond(window, x, base);
            }
            for x in [184., 226.] {
                station(window, landed, x, mid_track);
            }
            for x in [316., 372., 428.] {
                station(window, working, x, top_track);
            }
            for x in [484., 530.] {
                station(window, other, x, mid_track);
            }
            // You are here.
            let (x, y) = (484., top_track);
            let r = 7.5;
            window.paint_quad(quad(
                Bounds::from_corners(at(x - r, y - r), at(x + r, y + r)),
                px(r),
                bg,
                px(2.5),
                wc,
                BorderStyle::Solid,
            ));
            window.paint_quad(
                fill(
                    Bounds::from_corners(at(x - 3.5, y - 3.5), at(x + 3.5, y + 3.5)),
                    wc,
                )
                .corner_radii(px(3.5)),
            );
        },
    )
    .w(px(W))
    .h(px(H))
    .flex_shrink_0()
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        // The stage sits in the middle of the pane; a scroller cannot ask
        // for that itself, so the pane's height is stated.
        let pane_height = f32::from(window.viewport_size().height) - TITLEBAR_H - STATUSBAR_H - 2.;
        let model = self.app.read(cx);
        let opening = model.tab().opening.is_some();
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
            .w(px(640.))
            .max_w_full()
            .gap(px(24.))
            .px(px(24.))
            .pt(px(24.))
            .pb(px(96.))
            .child(transit_map(&t))
            .child(
                h_flex()
                    .gap(px(16.))
                    .child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .justify_center()
                            .size(px(52.))
                            .rounded(px(13.))
                            .bg(t.bg2)
                            .border_1()
                            .border_color(t.border1)
                            .child(icon(IconName::Logo, 32., t.accent_strong)),
                    )
                    .child(
                        v_flex()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(px(TEXT_XL + 8.))
                                    .font_weight(FontWeight::BOLD)
                                    .line_height(gpui::relative(1.15))
                                    .child("Jiji"),
                            )
                            .child(
                                h_flex()
                                    .gap(px(5.))
                                    .text_size(px(TEXT_L))
                                    .text_color(t.text2)
                                    .child("A workbench for")
                                    .child(
                                        div()
                                            .font_family(t.mono.clone())
                                            .text_size(px(TEXT_M))
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
            // A plain list: rows reach a little past the column on both
            // sides, so their text keeps the column's edge.
            let mut list = v_flex().mx(px(-10.));
            for (ix, repo) in recents.into_iter().enumerate() {
                let group = format!("recent-{ix}");
                let open_app = self.app.clone();
                let forget_app = self.app.clone();
                let (open_path, forget_path) = (repo.path.clone(), repo.path.clone());
                let row = h_flex()
                    .id(("recent", ix))
                    .group(group.clone())
                    .h(px(36.))
                    .pl(px(10.))
                    .pr(px(4.))
                    .gap(px(12.))
                    .rounded(px(RADIUS_M))
                    .cursor_pointer()
                    .hover(|style| style.bg(t.bg_hover))
                    .on_click(move |_, _, cx| {
                        let path = open_path.clone();
                        open_app.update(cx, |m, cx| m.open_repo_at(path, cx))
                    })
                    .child(crate::ui::monogram(
                        &repo.name,
                        &crate::ui::initials(&repo.name)
                            .chars()
                            .take(1)
                            .collect::<String>(),
                        20.,
                        false,
                        cx,
                    ))
                    .child(
                        div()
                            .flex_shrink_0()
                            .w(px(150.))
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(repo.name.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family(t.mono.clone())
                            .text_size(px(TEXT_XS))
                            .text_color(t.text3)
                            .child(truncated(short_path(&repo.path))),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(TEXT_XS))
                            .text_color(t.text3)
                            .tabular()
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
                list = list.child(row);
            }
            stage = stage.child(
                v_flex()
                    .child(
                        div()
                            .pb(px(8.))
                            .mb(px(4.))
                            .border_b_1()
                            .border_color(t.border2)
                            .text_size(px(TEXT_S))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.text2)
                            .child("Recent repositories"),
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
                            .min_h(px(pane_height.max(0.)))
                            .items_center()
                            .justify_center()
                            .child(stage),
                    ),
            )
            .child(scrollbar("welcome-scrollbar", &self.scroll))
    }
}
