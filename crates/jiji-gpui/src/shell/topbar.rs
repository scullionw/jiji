//! The title bar: the window's one horizontal strip of chrome. Repo tabs
//! on the left (after the traffic lights), where trunk stands and when the
//! remote was last checked, then the window's controls and their popovers
//! (registration, settings, theme).

use gpui::{
    div, prelude::*, px, AnyElement, Context, Corner, FontWeight, MouseButton, SharedString, Window,
};
use jiji_core::snapshot::SyncState;

use super::Shell;
use crate::app::{tab_titles, Section, ThemeMode, UPSTREAM_INTERVALS};
use crate::host::license::{PERSONAL_CHECKOUT_URL, PERSONAL_PRICE, SOLO_CHECKOUT_URL, SOLO_PRICE};
use crate::theme::{
    theme, theme_defs, Scheme, RADIUS_M, RADIUS_S, TEXT_M, TEXT_S, TEXT_XS, TITLEBAR_H,
};
use crate::ui::{
    btn, h_flex, icon, icon_btn, input::field, menu_label, popover, popover_layer, segmented,
    tooltip, v_flex, IconName,
};

/// "just now", "3m ago", "2h ago" — minutes-granular like the chip's
/// heartbeat; clamped so clock skew never reads negative.
pub fn checked_age_label(last_checked: u64, now: u64) -> String {
    let minutes = now.saturating_sub(last_checked) / 60_000;
    if minutes < 1 {
        "just now".into()
    } else if minutes < 60 {
        format!("{minutes}m ago")
    } else {
        format!("{}h ago", minutes / 60)
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum UpstreamTone {
    Quiet,
    Busy,
    Error,
}

/// The chip for the current check state: its label, tone and the full
/// story for the tooltip. `None` when the repo has no remotes — no
/// upstream, nothing to check, no dead affordance.
pub fn upstream_chip(
    checking: bool,
    last_checked: Option<u64>,
    error: Option<&str>,
    remote_names: &[&str],
    now: u64,
) -> Option<(String, UpstreamTone, String)> {
    let remotes = match remote_names {
        [] => return None,
        [only] => (*only).to_owned(),
        many => format!("{} remotes", many.len()),
    };
    Some(if checking {
        (
            "Checking…".into(),
            UpstreamTone::Busy,
            format!("Checking {remotes} for new changes"),
        )
    } else if let Some(error) = error {
        (
            "Fetch failed".into(),
            UpstreamTone::Error,
            format!("Could not fetch from {remotes}: {error} — click to retry"),
        )
    } else if let Some(at) = last_checked {
        let age = checked_age_label(at, now);
        (
            format!("Checked {age}"),
            UpstreamTone::Quiet,
            format!("Upstream last checked {age} — click to fetch from {remotes} now"),
        )
    } else {
        (
            "Fetch".into(),
            UpstreamTone::Quiet,
            format!("Fetch from {remotes}"),
        )
    })
}

fn sync_text(sync: SyncState) -> &'static str {
    match sync {
        SyncState::Synced => "up to date",
        SyncState::Ahead => "ahead",
        SyncState::Behind => "behind",
        SyncState::Diverged => "diverged",
        SyncState::LocalOnly => "local only",
        SyncState::RemoteOnly => "remote only",
    }
}

impl Shell {
    pub(super) fn render_topbar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let width: f32 = window.viewport_size().width.into();
        // Native fullscreen hides the traffic lights, so the bar gives
        // their clearance back.
        let lights = if window.is_fullscreen() { 8. } else { 76. };
        let model = self.app.read(cx);
        let has_repo = model.snapshot().is_some();
        let workbench = has_repo && model.section() == Section::Workbench;
        let graph_collapsed = model.graph_collapsed;

        let mut bar = h_flex()
            .id("titlebar")
            .h(px(TITLEBAR_H))
            .flex_shrink_0()
            .gap(px(12.))
            .pr(px(8.))
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count == 2 {
                    window.titlebar_double_click();
                }
            })
            .child(div().w(px(lights)).flex_shrink_0())
            .child(self.render_tabs(cx));

        if width > 1020. {
            bar = bar.children(self.render_sync(width > 1180., cx));
        }

        let app = self.app.clone();
        let mut actions = h_flex().flex_shrink_0().gap(px(2.));
        if workbench {
            // The graph pane's toggle lives up here so it stays reachable
            // while the pane itself is off screen.
            let app = app.clone();
            actions = actions.child(
                icon_btn(
                    "toggle-graph",
                    if graph_collapsed {
                        IconName::PanelOpen
                    } else {
                        IconName::PanelClose
                    },
                )
                .size(24., 15.)
                .tooltip(format!(
                    "{} graph panel (⌘⇧B)",
                    if graph_collapsed { "Show" } else { "Hide" }
                ))
                .on_click(move |_, _, cx| app.update(cx, |m, cx| m.toggle_graph_panel(cx))),
            );
        }
        if has_repo {
            let app = app.clone();
            actions = actions.child(
                icon_btn("search-history", IconName::Search)
                    .size(24., 15.)
                    .tooltip("Search history (⌘⇧F)")
                    .on_click(move |_, _, cx| app.update(cx, |m, cx| m.open_search("", cx))),
            );
        }
        actions = actions
            .child({
                let app = app.clone();
                icon_btn("palette", IconName::Command)
                    .size(24., 15.)
                    .tooltip("Command palette (⌘K)")
                    .on_click(move |_, _, cx| app.update(cx, |m, cx| m.toggle_palette(cx)))
            })
            .child(div().w(px(1.)).h(px(14.)).mx(px(6.)).bg(t.border1))
            .child(self.render_license(window, cx))
            .child(self.render_settings(cx))
            .child(self.render_theme_menu(cx));
        bar.child(actions)
    }

    /// The open repos, one tab each — living in the title bar's flexible
    /// middle.
    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let model = self.app.read(cx);
        let active_id = model.active_id;
        let entries: Vec<(u64, Option<&str>, Option<&str>)> = model
            .tabs
            .iter()
            .map(|tab| (tab.id, tab.name.as_deref(), tab.path.as_deref()))
            .collect();
        let titles = tab_titles(&entries);
        let paths: Vec<Option<String>> = model.tabs.iter().map(|tab| tab.path.clone()).collect();

        let mut strip = h_flex()
            .id("tab-strip")
            .h_full()
            .max_w_full()
            .overflow_x_scroll();
        for ((id, title), path) in titles.into_iter().zip(paths) {
            let active = id == active_id;
            let group = SharedString::from(format!("tab-{id}"));
            let app = self.app.clone();
            let close_app = self.app.clone();
            let middle_app = self.app.clone();
            let mut label = h_flex()
                .relative()
                .h_full()
                .min_w_0()
                .max_w(px(180.))
                .child(div().truncate().child(title.label.clone()).when_some(
                    title.hint,
                    |el, hint| {
                        el.child(
                            div()
                                .ml(px(5.))
                                .font_weight(FontWeight::NORMAL)
                                .text_color(t.text3)
                                .child(hint),
                        )
                    },
                ));
            label = label.flex().flex_row();
            if active {
                label = label.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .h(px(2.))
                        .bg(t.accent),
                );
            }
            // A tab is its repo's name; the one on screen reads in full ink
            // over the accent bar, on the line the panes hang from.
            let mut tab = h_flex()
                .id(("tab", id))
                .group(group.clone())
                .relative()
                .flex_shrink()
                .min_w_0()
                .h_full()
                .pl(px(10.))
                .pr(px(4.))
                .text_size(px(TEXT_S))
                .font_weight(FontWeight::MEDIUM)
                .whitespace_nowrap()
                .cursor_pointer()
                .tooltip(tooltip(path.unwrap_or_else(|| "New tab".into())))
                .on_click(move |_, _, cx| app.update(cx, |m, cx| m.activate_tab(id, cx)))
                .on_mouse_down(MouseButton::Middle, move |_, _, cx| {
                    middle_app.update(cx, |m, cx| m.close_tab(id, cx));
                })
                .child(label);
            tab = if active {
                tab.text_color(t.text1)
            } else {
                tab.text_color(t.text3)
                    .hover(|style| style.text_color(t.text1))
            };
            // The close affordance keeps its footprint (no layout shift)
            // and only surfaces under the pointer.
            let close = div()
                .id(("tab-close", id))
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .size(px(18.))
                .ml(px(4.))
                .rounded(px(RADIUS_S))
                .opacity(0.)
                .group_hover(group, |style| style.opacity(1.))
                .hover(|style| style.bg(t.bg_hover))
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    close_app.update(cx, |m, cx| m.close_tab(id, cx));
                })
                .child(icon(IconName::Close, 10., t.text3));
            strip = strip.child(tab.child(close));
        }
        let app = self.app.clone();
        strip = strip.child(
            div().ml(px(4.)).child(
                icon_btn("new-tab", IconName::Plus)
                    .size(22., 13.)
                    .tooltip("New tab (⌘T)")
                    .on_click(move |_, _, cx| app.update(cx, |m, cx| m.new_tab(cx))),
            ),
        );
        h_flex().flex_1().min_w_0().h_full().child(strip)
    }

    /// Where trunk stands and when the remote was last checked.
    fn render_sync(&self, show_trunk: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = theme(cx);
        let model = self.app.read(cx);
        let snapshot = model.snapshot()?;
        let trunk = snapshot.bookmarks.iter().find(|b| b.is_trunk);
        let remotes: Vec<&str> = snapshot
            .git_remotes
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        let chip = upstream_chip(
            model.upstream.checking,
            model.upstream.last_checked,
            model.upstream.error.as_deref(),
            &remotes,
            model.upstream.now,
        );
        if trunk.is_none() && chip.is_none() {
            return None;
        }
        let mut row = h_flex()
            .flex_shrink_0()
            .gap(px(12.))
            .text_size(px(TEXT_S))
            .text_color(t.text3);
        if let Some(trunk) = trunk.filter(|_| show_trunk) {
            let name = match &trunk.remote {
                Some(remote) => format!("{}@{remote}", trunk.name),
                None => trunk.name.clone(),
            };
            let state = sync_text(trunk.sync);
            row = row.child(
                h_flex()
                    .id("trunk")
                    .gap(px(5.))
                    .tooltip(tooltip(format!("Trunk: {name} — {state}")))
                    .child(icon(IconName::Branch, 12., t.text3))
                    .child(
                        div()
                            .font_family(t.mono.clone())
                            .text_size(px(TEXT_XS))
                            .text_color(t.text2)
                            .child(name),
                    )
                    .child(
                        div()
                            .when(trunk.sync != SyncState::Synced, |el| el.text_color(t.warn))
                            .child(state),
                    ),
            );
        }
        if let Some((label, tone, title)) = chip {
            let busy = tone == UpstreamTone::Busy;
            let app = self.app.clone();
            let color = if tone == UpstreamTone::Error {
                t.warn
            } else {
                t.text3
            };
            // The upstream check: always visible, never loud.
            let mut el = h_flex()
                .id("upstream-chip")
                .gap(px(5.))
                .h(px(22.))
                .px(px(6.))
                .rounded(px(RADIUS_M))
                .text_color(color)
                .tooltip(tooltip(title))
                .child(icon(IconName::Refresh, 11., color))
                .child(label);
            if !busy {
                el = el
                    .cursor_pointer()
                    .hover(|style| style.bg(t.bg_hover).text_color(t.text1))
                    .on_click(move |_, _, cx| app.update(cx, |m, cx| m.check_upstream(true, cx)));
            }
            row = row.child(el);
        }
        Some(row.into_any_element())
    }

    // ── Registration ──

    fn render_license(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let model = self.app.read(cx);
        let registered = model.license.registered;
        let open = model.registration_open;
        let plan = match model.license.plan.as_str() {
            "solo" => "Solo",
            "personal" => "Personal",
            _ => "Licensed",
        };
        let display_key = model.license.display_key.clone();
        let app = self.app.clone();
        let toggle = move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
            app.update(cx, |m, cx| {
                if m.registration_open {
                    m.close_registration(cx)
                } else {
                    m.open_registration(cx)
                }
            });
        };
        let trigger: AnyElement = if registered {
            icon_btn("license", IconName::BadgeCheck)
                .size(24., 15.)
                .on(open)
                .tooltip("License")
                .on_click(toggle)
                .into_any_element()
        } else {
            btn("register", "Register")
                .ghost()
                .icon(IconName::Key)
                .on(open)
                .tooltip("Register Jiji")
                .on_click(toggle)
                .into_any_element()
        };
        let anchor = div().relative().child(trigger);
        if !open {
            return anchor;
        }

        let mut panel = popover(cx)
            .w(px(320.))
            .p(px(14.))
            .flex()
            .flex_col()
            .gap(px(10.));
        let title = |text: &'static str| {
            div()
                .text_size(px(TEXT_M))
                .font_weight(FontWeight::SEMIBOLD)
                .child(text)
        };
        let link = |id: &'static str, text: &'static str| {
            div()
                .id(id)
                .text_size(px(TEXT_S))
                .text_color(t.accent_strong)
                .cursor_pointer()
                .hover(|style| style.text_color(t.text1))
                .child(text)
        };
        if registered && !self.license_replacing {
            panel = panel
                .child(
                    h_flex()
                        .gap(px(10.))
                        .items_start()
                        .child(icon(IconName::BadgeCheck, 16., t.ok))
                        .child(
                            v_flex()
                                .gap(px(2.))
                                .child(title("Thanks for supporting Jiji"))
                                .child(
                                    h_flex()
                                        .gap(px(4.))
                                        .text_size(px(TEXT_S))
                                        .text_color(t.text2)
                                        .child(format!("{plan} license ·"))
                                        .child(
                                            div().font_family(t.mono.clone()).child(display_key),
                                        ),
                                ),
                        ),
                )
                .child(
                    link("license-replace", "Replace license key").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.license_replacing = true;
                            cx.notify();
                        },
                    )),
                );
        } else {
            let busy = self.license_busy;
            let empty = self.license_input.read(cx).text().trim().is_empty();
            panel =
                panel
                    .child(title(if registered {
                        "Replace your key"
                    } else {
                        "Support Jiji"
                    }))
                    .child(
                        div()
                            .text_size(px(TEXT_S))
                            .line_height(gpui::relative(1.5))
                            .text_color(t.text2)
                            .child(
                                "Jiji and every jj feature are free. A one-time license unlocks \
                             all ten themes and funds development.",
                            ),
                    )
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .child(
                                field(&self.license_input, window, cx)
                                    .flex_1()
                                    .font_family(t.mono.clone()),
                            )
                            .child(
                                btn(
                                    "license-submit",
                                    if busy { "Checking…" } else { "Register" },
                                )
                                .primary()
                                .disabled(busy || empty)
                                .on_click(cx.listener(
                                    |this, _, window, cx| this.activate_license(window, cx),
                                )),
                            ),
                    );
            if let Some(error) = &self.license_error {
                panel = panel.child(
                    div()
                        .text_size(px(TEXT_S))
                        .text_color(t.danger)
                        .child(error.clone()),
                );
            }
            if registered {
                panel = panel.child(link("license-cancel", "Cancel").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.license_replacing = false;
                        this.license_error = None;
                        cx.notify();
                    },
                )));
            } else {
                let buy =
                    |id: &'static str, name: &'static str, price: String, url: &'static str| {
                        v_flex()
                            .id(id)
                            .flex_1()
                            .gap(px(1.))
                            .px(px(10.))
                            .py(px(7.))
                            .rounded(px(RADIUS_M))
                            .border_1()
                            .border_color(t.border1)
                            .bg(t.btn_bg)
                            .cursor_pointer()
                            .hover(|style| style.bg(t.btn_hover))
                            .on_click(move |_, _, cx| cx.open_url(url))
                            .child(
                                div()
                                    .text_size(px(TEXT_S))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(name),
                            )
                            .child(
                                div()
                                    .text_size(px(TEXT_XS))
                                    .text_color(t.text3)
                                    .child(price),
                            )
                    };
                panel = panel
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .text_size(px(TEXT_XS))
                            .text_color(t.text3)
                            .child(div().flex_1().h(px(1.)).bg(t.border2))
                            .child("No key yet?")
                            .child(div().flex_1().h(px(1.)).bg(t.border2)),
                    )
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(buy(
                                "buy-solo",
                                "Solo",
                                format!("{SOLO_PRICE} · 1 device"),
                                SOLO_CHECKOUT_URL,
                            ))
                            .child(buy(
                                "buy-personal",
                                "Personal",
                                format!("{PERSONAL_PRICE} · 5 devices"),
                                PERSONAL_CHECKOUT_URL,
                            )),
                    )
                    .child(
                        div()
                            .text_size(px(TEXT_XS))
                            .text_color(t.text3)
                            .child("Secure checkout via Polar. Your key arrives by email."),
                    );
            }
        }
        let app = self.app.clone();
        anchor.child(popover_layer(
            Corner::TopRight,
            30.,
            panel,
            cx.listener(move |this, _, _, cx| {
                this.license_error = None;
                this.license_replacing = false;
                app.update(cx, |m, cx| m.close_registration(cx));
            }),
        ))
    }

    // ── Settings ──

    /// App-level preferences, popover-sized: the machine integrations
    /// (which editor and terminal menus launch, the jiji command) and the
    /// upstream check cadence.
    fn render_settings(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let open = self.settings_menu.is_open();
        let anchor = div().relative().child(
            icon_btn("settings", IconName::Settings)
                .size(24., 15.)
                .on(open)
                .tooltip("Settings")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.settings_menu.toggle();
                    this.cli_error = None;
                    cx.notify();
                })),
        );
        if !open {
            return anchor;
        }
        let model = self.app.read(cx);
        let preferred_editor = model.preferred_editor();
        let last_used = model.editor_choices().first().cloned();
        let preferred_terminal = model.preferred_terminal();
        let active_terminal = model
            .terminals
            .iter()
            .find(|terminal| Some(terminal.id) == preferred_terminal.as_deref())
            .or(model.terminals.first())
            .map(|terminal| terminal.id);
        let editors = model.editors.clone();
        let terminals = model.terminals.clone();
        let interval = model.upstream.interval_minutes;
        let cli = model.cli.status.clone();
        let installing = model.cli.installing;

        let row =
            |id: gpui::ElementId, label: SharedString, hint: Option<SharedString>, on: bool| {
                let mut el = h_flex()
                    .id(id)
                    .gap(px(8.))
                    .h(px(26.))
                    .px(px(8.))
                    .rounded(px(RADIUS_S))
                    .text_size(px(TEXT_S))
                    .cursor_pointer()
                    .hover(|style| style.bg(t.bg_hover))
                    .child(div().flex_1().truncate().child(label));
                if let Some(hint) = hint {
                    el = el.child(div().text_size(px(TEXT_XS)).text_color(t.text3).child(hint));
                }
                el.child(div().w(px(14.)).when(on, |el| {
                    el.child(icon(IconName::Check, 13., t.accent_strong))
                }))
            };
        let note = |text: String| {
            div()
                .px(px(8.))
                .pb(px(4.))
                .text_size(px(TEXT_XS))
                .line_height(gpui::relative(1.5))
                .text_color(t.text3)
                .child(text)
        };

        let mut panel = popover(cx)
            .w(px(264.))
            .p(px(4.))
            .flex()
            .flex_col()
            .child(menu_label("Preferred editor", cx));
        if editors.is_empty() {
            panel = panel.child(note(
                "No editor command-line tools found on this machine.".into(),
            ));
        } else {
            let app = self.app.clone();
            panel = panel.child(
                row(
                    "editor-last".into(),
                    "Last used".into(),
                    last_used.map(|editor| editor.label.into()),
                    preferred_editor.is_none(),
                )
                .on_click(move |_, _, cx| app.update(cx, |m, cx| m.set_preferred_editor(None, cx))),
            );
            for (ix, editor) in editors.iter().enumerate() {
                let app = self.app.clone();
                let id = editor.id;
                panel = panel.child(
                    row(
                        ("editor", ix).into(),
                        editor.label.into(),
                        None,
                        preferred_editor.as_deref() == Some(id),
                    )
                    .on_click(move |_, _, cx| {
                        app.update(cx, |m, cx| m.set_preferred_editor(Some(id), cx))
                    }),
                );
            }
        }
        if !terminals.is_empty() {
            panel = panel.child(menu_label("Terminal", cx));
            for (ix, terminal) in terminals.iter().enumerate() {
                let app = self.app.clone();
                let id = terminal.id;
                panel = panel.child(
                    row(
                        ("terminal", ix).into(),
                        terminal.label.into(),
                        None,
                        active_terminal == Some(id),
                    )
                    .on_click(move |_, _, cx| {
                        app.update(cx, |m, cx| m.set_preferred_terminal(Some(id), cx))
                    }),
                );
            }
        }
        let app = self.app.clone();
        panel = panel.child(menu_label("Check upstream", cx)).child(
            div().px(px(8.)).pb(px(6.)).child(segmented(
                "upstream-interval",
                UPSTREAM_INTERVALS
                    .iter()
                    .map(|&(minutes, label)| (minutes, SharedString::from(label), None))
                    .collect(),
                &interval,
                cx,
                move |minutes, _, cx| {
                    let minutes = *minutes;
                    app.update(cx, |m, cx| m.set_upstream_interval(minutes, cx));
                },
            )),
        );
        if let Some(cli) = cli {
            panel = panel.child(menu_label("Command line", cx));
            if cli.installed {
                let path = cli.path.unwrap_or_default();
                panel =
                    panel.child(
                        h_flex()
                            .gap(px(8.))
                            .px(px(8.))
                            .pb(px(6.))
                            .child(
                                div()
                                    .flex_1()
                                    .truncate()
                                    .text_size(px(TEXT_XS))
                                    .text_color(t.text3)
                                    .child(format!("Installed at {path}")),
                            )
                            .child(btn("cli-uninstall", "Uninstall").danger().on_click(
                                cx.listener(|this, _, _, cx| {
                                    let task = this.app.update(cx, |m, cx| m.uninstall_cli(cx));
                                    this.run_cli(task, cx);
                                }),
                            )),
                    );
            } else if let Some(conflict) = cli.conflict {
                panel = panel.child(note(format!("Another jiji occupies {conflict}.")));
            } else {
                panel = panel.child(
                    div().px(px(8.)).pb(px(6.)).flex().flex_col().child(
                        btn(
                            "cli-install",
                            if installing {
                                "Installing…"
                            } else {
                                "Install jiji command"
                            },
                        )
                        .disabled(installing)
                        .on_click(cx.listener(|this, _, _, cx| {
                            let task = this.app.update(cx, |m, cx| m.install_cli(cx));
                            this.run_cli(task, cx);
                        })),
                    ),
                );
            }
            if let Some(error) = &self.cli_error {
                panel = panel.child(
                    div()
                        .px(px(8.))
                        .pb(px(6.))
                        .text_size(px(TEXT_XS))
                        .text_color(t.danger)
                        .child(error.clone()),
                );
            }
        }
        panel = panel.child(
            h_flex()
                .gap(px(4.))
                .px(px(8.))
                .py(px(6.))
                .border_t_1()
                .border_color(t.border2)
                .text_size(px(TEXT_XS))
                .text_color(t.text3)
                .child("Jiji")
                .child(
                    div()
                        .font_family(t.mono.clone())
                        .child(crate::host::update::CURRENT_VERSION),
                ),
        );
        anchor.child(popover_layer(
            Corner::TopRight,
            30.,
            panel,
            cx.listener(|this, _, _, cx| {
                this.settings_menu.close();
                cx.notify();
            }),
        ))
    }

    fn run_cli(&mut self, task: gpui::Task<crate::host::HostResult<()>>, cx: &mut Context<Self>) {
        self.cli_error = None;
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                let _ = this.update(cx, |this, cx| {
                    this.cli_error = Some(error.message);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    // ── Theme ──

    fn render_theme_menu(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let open = self.theme_menu.is_open();
        let model = self.app.read(cx);
        let mode = model.theme_mode();
        let registered = model.license.registered;
        let pref = model.theme_pref();
        let glyph = match mode {
            ThemeMode::System => IconName::SunMoon,
            ThemeMode::Light => IconName::Sun,
            ThemeMode::Dark => IconName::Moon,
        };
        let anchor = div().relative().child(
            icon_btn("theme", glyph)
                .size(24., 15.)
                .on(open)
                .tooltip("Theme")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.theme_menu.toggle();
                    cx.notify();
                })),
        );
        if !open {
            return anchor;
        }
        let mut panel = popover(cx).w(px(252.)).p(px(8.)).flex().flex_col();
        if registered {
            let app = self.app.clone();
            panel = panel.child(h_flex().child(segmented(
                "theme-mode",
                vec![
                    (ThemeMode::System, "System".into(), None),
                    (ThemeMode::Light, "Light".into(), None),
                    (ThemeMode::Dark, "Dark".into(), None),
                ],
                &mode,
                cx,
                move |mode, _, cx| {
                    let mode = *mode;
                    app.update(cx, |m, cx| m.set_theme_mode(mode, cx));
                },
            )));
            let defs = theme_defs();
            for (label, scheme, current) in [
                ("Dark", Scheme::Dark, pref.dark.as_str()),
                ("Light", Scheme::Light, pref.light.as_str()),
            ] {
                let mut swatches = h_flex().flex_wrap().gap(px(6.));
                for def in defs.iter().filter(|def| def.scheme == scheme) {
                    let selected = def.id == current;
                    let app = self.app.clone();
                    let id = def.id;
                    swatches = swatches.child(
                        v_flex()
                            .id(SharedString::from(format!("swatch-{id}")))
                            .w(px(42.))
                            .gap(px(3.))
                            .items_center()
                            .cursor_pointer()
                            .on_click(move |_, _, cx| {
                                app.update(cx, |m, cx| m.select_theme(id, cx))
                            })
                            .child(
                                div()
                                    .w(px(40.))
                                    .h(px(26.))
                                    .flex()
                                    .items_end()
                                    .p(px(4.))
                                    .rounded(px(RADIUS_S))
                                    .bg(def.bg)
                                    .border_1()
                                    .border_color(if selected { t.accent } else { t.border1 })
                                    .child(
                                        div().w(px(14.)).h(px(3.)).rounded(px(2.)).bg(def.accent),
                                    ),
                            )
                            .child(
                                div()
                                    .max_w_full()
                                    .truncate()
                                    .text_size(px(10.))
                                    .text_color(if selected { t.text1 } else { t.text3 })
                                    .child(def.label),
                            ),
                    );
                }
                panel = panel
                    .child(menu_label(label, cx).px(px(2.)))
                    .child(swatches);
            }
        } else {
            let app = self.app.clone();
            panel = panel.child(
                v_flex()
                    .items_center()
                    .gap(px(6.))
                    .p(px(10.))
                    .child(icon(IconName::Lock, 16., t.text3))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Themes come with a license"),
                    )
                    .child(
                        div()
                            .text_size(px(TEXT_S))
                            .line_height(gpui::relative(1.5))
                            .text_color(t.text2)
                            .text_center()
                            .child(
                                "Jiji ships in ten palettes. A one-time license unlocks them \
                                 all and keeps development going.",
                            ),
                    )
                    .child(
                        div().mt(px(4.)).child(
                            btn("unlock-themes", "Unlock themes")
                                .primary()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.theme_menu.close();
                                    app.update(cx, |m, cx| m.open_registration(cx));
                                    cx.notify();
                                })),
                        ),
                    ),
            );
        }
        anchor.child(popover_layer(
            Corner::TopRight,
            30.,
            panel,
            cx.listener(|this, _, _, cx| {
                this.theme_menu.close();
                cx.notify();
            }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_upstream_chip_tells_each_state_apart() {
        let now = 10 * 60_000;
        assert_eq!(upstream_chip(false, None, None, &[], now), None);
        let (label, tone, title) = upstream_chip(true, None, None, &["origin"], now).unwrap();
        assert_eq!((label.as_str(), tone), ("Checking…", UpstreamTone::Busy));
        assert!(title.contains("origin"));
        let (label, tone, _) =
            upstream_chip(false, None, Some("offline"), &["a", "b"], now).unwrap();
        assert_eq!(
            (label.as_str(), tone),
            ("Fetch failed", UpstreamTone::Error)
        );
        let (label, _, title) =
            upstream_chip(false, Some(7 * 60_000), None, &["a", "b"], now).unwrap();
        assert_eq!(label, "Checked 3m ago");
        assert!(title.contains("2 remotes"));
        assert_eq!(
            upstream_chip(false, None, None, &["origin"], now)
                .unwrap()
                .0,
            "Fetch"
        );
    }

    #[test]
    fn ages_are_minute_granular_and_never_negative() {
        assert_eq!(checked_age_label(1000, 500), "just now");
        assert_eq!(checked_age_label(0, 59_999), "just now");
        assert_eq!(checked_age_label(0, 61 * 60_000), "1h ago");
    }
}
