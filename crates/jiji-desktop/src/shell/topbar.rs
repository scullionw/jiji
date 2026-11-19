//! The title bar: the window's one strip of chrome. Repo tabs on the left
//! (after the traffic lights), the command field on the window's center
//! line, then where trunk stands, when the remote was last checked, and
//! the window's one Settings control.

use gpui::{
    div, prelude::*, px, Anchor, AnyElement, Context, Div, FontWeight, MouseButton, Role,
    SharedString, Window,
};
use jiji_core::snapshot::SyncState;

use super::Shell;
use crate::app::{tab_titles, Section, ThemeMode, UPSTREAM_INTERVALS};
use crate::host::license::{PERSONAL_CHECKOUT_URL, PERSONAL_PRICE, SOLO_CHECKOUT_URL, SOLO_PRICE};
use crate::theme::{
    theme, theme_defs, Scheme, RADIUS_M, RADIUS_S, TEXT_M, TEXT_S, TEXT_XS, TITLEBAR_H,
};
use crate::ui::{
    btn, h_flex, icon, icon_btn, input::field, kbd, menu_label, popover, popover_layer, ring,
    segmented, tooltip, v_flex, IconName, NodeExt as _, Segment, Tabbable as _, Tabular as _,
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

/// The air above the tab band. Tabs, the command field and the controls
/// all center in the band below it; the open tab runs down into the work
/// area it belongs to.
const BAND_TOP: f32 = 6.;
const BAND_H: f32 = TITLEBAR_H - BAND_TOP;

/// The command field's width at a window width: it gives way before the
/// tabs and the controls on either side of it do.
fn command_field_width(window_width: f32) -> f32 {
    if window_width >= 1280. {
        360.
    } else if window_width >= 1120. {
        300.
    } else {
        240.
    }
}

impl Shell {
    pub(super) fn render_topbar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let width: f32 = window.viewport_size().width.into();
        // Native fullscreen hides the traffic lights, so the bar gives
        // their clearance back.
        let lights = if window.is_fullscreen() { 12. } else { 78. };
        let model = self.app.read(cx);
        let has_repo = model.snapshot().is_some();
        let workbench = has_repo && model.section() == Section::Workbench;
        let graph_collapsed = model.graph_collapsed;
        let registered = model.license.registered;
        // The hairline that closes the frame runs over the work area only:
        // the rail and the title bar are one surface.
        let rail = if has_repo && !model.sidebar_collapsed {
            crate::theme::RAIL_W - 1.
        } else {
            0.
        };
        let t = theme(cx);

        // The command field rides the window's center line, whatever the
        // tabs and controls beside it measure; the tabs stop short of it.
        let field_w = command_field_width(width);
        let field_left = ((width - field_w) / 2.).round();
        let tabs_room = (field_left - lights - 20.).max(96.);

        let app = self.app.clone();
        let mut controls = h_flex().flex_shrink_0().gap(px(2.));
        if let Some(sync) = self.render_sync(width >= 1180., cx) {
            controls = controls.child(sync).child(div().w(px(6.)));
        }
        if workbench {
            // The graph pane's toggle lives up here so it stays reachable
            // while the pane itself is off screen.
            let app = app.clone();
            controls = controls.child(
                icon_btn(
                    "toggle-graph",
                    if graph_collapsed {
                        IconName::PanelOpen
                    } else {
                        IconName::PanelClose
                    },
                )
                .size(26., 15.)
                .tooltip(format!(
                    "{} graph panel (⌘⇧B)",
                    if graph_collapsed { "Show" } else { "Hide" }
                ))
                .expanded(!graph_collapsed)
                .on_click(move |_, _, cx| app.update(cx, |m, cx| m.toggle_graph_panel(cx))),
            );
        }
        if !registered {
            controls = controls.child(self.render_license(window, cx));
        }
        controls = controls.child(self.render_settings(window, cx));

        h_flex()
            .id("titlebar")
            .relative()
            .h(px(TITLEBAR_H))
            .flex_shrink_0()
            .pt(px(BAND_TOP))
            .pr(px(8.))
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count == 2 {
                    window.titlebar_double_click();
                }
            })
            // First, so the open tab paints over it.
            .child(
                div()
                    .absolute()
                    .left(px(rail))
                    .right_0()
                    .bottom_0()
                    .h(px(1.))
                    .bg(t.border2),
            )
            .child(div().w(px(lights)).flex_shrink_0())
            .child(
                div()
                    .h(px(BAND_H))
                    .min_w_0()
                    .max_w(px(tabs_room))
                    .child(self.render_tabs(cx)),
            )
            .child(div().flex_1())
            // Ahead of the controls, so Tab reads the bar left to right.
            .child(self.render_command_field(field_left, field_w, has_repo, cx))
            .child(controls)
    }

    /// The one way into everything the app can do or find: it opens the
    /// command palette, and wears its shortcut.
    fn render_command_field(
        &self,
        left: f32,
        width: f32,
        has_repo: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let open = self.app.read(cx).palette_open;
        let app = self.app.clone();
        let label = if has_repo {
            "Search or run a command"
        } else {
            "Run a command"
        };
        h_flex()
            .id("command-field")
            .role(Role::Button)
            .aria_label("Command palette")
            .aria_description(label)
            .aria_keyshortcuts("⌘K")
            .aria_expanded(open)
            .tabbable(cx)
            .absolute()
            .top(px(BAND_TOP + ((BAND_H - 26.) / 2.).floor()))
            .left(px(left))
            .w(px(width))
            .h(px(26.))
            .gap(px(8.))
            .pl(px(10.))
            .pr(px(4.))
            .rounded(px(RADIUS_M))
            .border_1()
            .border_color(if open { t.accent } else { t.border2 })
            .bg(t.btn_bg)
            .text_size(px(TEXT_S))
            .text_color(t.text3)
            .cursor_pointer()
            .hover(|style| style.bg(t.btn_hover).border_color(t.border1))
            // Its presses are its own: a double click here is two
            // presses on a control, not a title bar zoom.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, _, cx| app.update(cx, |m, cx| m.toggle_palette(cx)))
            .child(icon(IconName::Search, 13., t.text3))
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(kbd("⌘K", cx))
    }

    /// The open repos, one tab each. The open one is cut from the work
    /// area's own surface and runs down into it; the rest are names on the
    /// frame.
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
        let names: Vec<Option<String>> = model.tabs.iter().map(|tab| tab.name.clone()).collect();
        let paths: Vec<Option<String>> = model.tabs.iter().map(|tab| tab.path.clone()).collect();

        let mut strip = h_flex()
            .id("tab-strip")
            .role(Role::TabList)
            .aria_label("Open repositories")
            .h_full()
            .max_w_full()
            .gap(px(2.))
            .overflow_x_scroll();
        for (((id, title), path), name) in titles.into_iter().zip(paths).zip(names) {
            let active = id == active_id;
            let group = SharedString::from(format!("tab-{id}"));
            let app = self.app.clone();
            let close_app = self.app.clone();
            let middle_app = self.app.clone();
            let name_text = match &title.hint {
                Some(hint) => format!("{} ({hint})", title.label),
                None => title.label.to_string(),
            };
            let label = h_flex()
                .min_w_0()
                .max_w(px(200.))
                .child(div().truncate().child(title.label.clone()))
                .when_some(title.hint, |el, hint| {
                    el.child(
                        div()
                            .ml(px(5.))
                            .flex_shrink_0()
                            .font_weight(FontWeight::NORMAL)
                            .text_color(t.text3)
                            .child(hint),
                    )
                });
            let mut tab = h_flex()
                .id(("tab", id))
                .role(Role::Tab)
                .aria_label(name_text.clone())
                .aria_selected(active)
                .tabbable(cx)
                .group(group.clone())
                .relative()
                .flex_shrink_1()
                .min_w_0()
                .h(px(BAND_H))
                .gap(px(7.))
                .pl(px(10.))
                .pr(px(5.))
                .rounded_t(px(8.))
                .text_size(px(TEXT_M))
                .font_weight(if active {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                })
                .whitespace_nowrap()
                .cursor_pointer()
                .when_some(path.clone(), |tab, path| tab.aria_description(path))
                .tooltip(tooltip(path.unwrap_or_else(|| "New tab".into())))
                .on_click(move |_, _, cx| app.update(cx, |m, cx| m.activate_tab(id, cx)))
                .on_mouse_down(MouseButton::Middle, move |_, _, cx| {
                    middle_app.update(cx, |m, cx| m.close_tab(id, cx));
                })
                .children(name.map(|name| {
                    let letter = crate::ui::initials(&name)
                        .chars()
                        .take(1)
                        .collect::<String>();
                    crate::ui::monogram(&name, &letter, 16., false, cx)
                }))
                .child(label);
            tab = if active {
                tab.text_color(t.text1)
                    .bg(t.bg1)
                    .border_t_1()
                    .border_l_1()
                    .border_r_1()
                    .border_color(t.border2)
            } else {
                tab.text_color(t.text3)
                    .hover(|style| style.text_color(t.text1))
            };
            // The close affordance keeps its footprint (no layout shift)
            // and only surfaces under the pointer, or with the keys on it.
            let accent = t.accent;
            let close = div()
                .id(("tab-close", id))
                .role(Role::Button)
                .aria_label(format!("Close {name_text}"))
                .tabbable_styled(move |style| style.opacity(1.).shadow(ring(accent)))
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .size(px(18.))
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
            div().ml(px(2.)).child(
                icon_btn("new-tab", IconName::Plus)
                    .size(22., 13.)
                    .tooltip("New tab (⌘T)")
                    .on_click(move |_, _, cx| app.update(cx, |m, cx| m.new_tab(cx))),
            ),
        );
        h_flex().min_w_0().h_full().child(strip)
    }

    /// Where trunk stands and when the remote was last checked: two quiet
    /// readouts, the full story on hover, a fetch one click away.
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
            .gap(px(2.))
            .text_size(px(TEXT_S))
            .text_color(t.text3);
        if let Some(trunk) = trunk.filter(|_| show_trunk) {
            let name = match &trunk.remote {
                Some(remote) => format!("{}@{remote}", trunk.name),
                None => trunk.name.clone(),
            };
            let state = sync_text(trunk.sync);
            let synced = trunk.sync == SyncState::Synced;
            row = row.child(
                h_flex()
                    .id("trunk")
                    .role(Role::Label)
                    .aria_value(format!("Trunk: {name} — {state}"))
                    .gap(px(6.))
                    .h(px(24.))
                    .px(px(6.))
                    .tooltip(tooltip(format!("Trunk: {name} — {state}")))
                    .child(icon(IconName::Branch, 13., t.text3))
                    .child(
                        div()
                            .font_family(t.mono.clone())
                            .text_size(px(TEXT_XS))
                            .text_color(t.text2)
                            .child(trunk.name.clone()),
                    )
                    .child(if synced {
                        icon(IconName::Check, 12., t.ok).into_any_element()
                    } else {
                        div().text_color(t.warn).child(state).into_any_element()
                    }),
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
            // The checked age stands alone: the icon says what was checked.
            let label = label
                .strip_prefix("Checked ")
                .map(str::to_owned)
                .unwrap_or(label);
            let mut el = h_flex()
                .id("upstream-chip")
                .aria_description(title.clone())
                .gap(px(5.))
                .h(px(24.))
                .px(px(6.))
                .rounded(px(RADIUS_M))
                .text_color(color)
                .tabular()
                .tooltip(tooltip(title))
                .child(icon(IconName::Refresh, 12., color))
                .child(label.clone());
            el = if busy {
                el.role(Role::Label).aria_value(label)
            } else {
                el.role(Role::Button).aria_label(label).tabbable(cx)
            };
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

    /// The unregistered copy's standing offer: a quiet Register control
    /// with its panel. A registered copy keeps its licence in Settings.
    fn render_license(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.app.read(cx).registration_open;
        let app = self.app.clone();
        let anchor = div().relative().child(
            btn("register", "Register")
                .ghost()
                .icon(IconName::Key)
                .on(open)
                .expanded(open)
                .tooltip("Register Jiji")
                .on_click(move |_, _, cx| {
                    app.update(cx, |m, cx| {
                        if m.registration_open {
                            m.close_registration(cx)
                        } else {
                            m.open_registration(cx)
                        }
                    });
                }),
        );
        if !open {
            return anchor;
        }
        let panel = self.license_panel(window, cx);
        anchor.child(self.license_layer(panel, cx))
    }

    fn license_layer(&self, panel: Div, cx: &mut Context<Self>) -> impl IntoElement {
        let app = self.app.clone();
        let registered = self.app.read(cx).license.registered;
        let name = if registered {
            "Replace your key"
        } else {
            "Register Jiji"
        };
        popover_layer(
            Anchor::TopRight,
            30.,
            self.register_trap.hold(
                panel
                    .id("register-panel")
                    .role(Role::Dialog)
                    .aria_label(name),
            ),
            cx.listener(move |this, _, _, cx| {
                this.license_error = None;
                app.update(cx, |m, cx| m.close_registration(cx));
            }),
        )
    }

    /// The registration panel: the key field and where to buy one, or —
    /// for a registered copy replacing its key — the field alone.
    fn license_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let t = theme(cx);
        let registered = self.app.read(cx).license.registered;
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
        let busy = self.license_busy;
        let empty = self.license_input.read(cx).text().trim().is_empty();
        panel = panel
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
                        .on_click(
                            cx.listener(|this, _, window, cx| this.activate_license(window, cx)),
                        ),
                    ),
            );
        if let Some(error) = &self.license_error {
            panel = panel.child(
                div()
                    .id("license-error")
                    .alert(error.clone())
                    .text_size(px(TEXT_S))
                    .text_color(t.danger)
                    .child(error.clone()),
            );
        }
        if registered {
            let app = self.app.clone();
            return panel.child(
                div()
                    .id("license-cancel")
                    .role(Role::Button)
                    .aria_label("Cancel")
                    .tabbable(cx)
                    .text_size(px(TEXT_S))
                    .text_color(t.accent_strong)
                    .cursor_pointer()
                    .hover(|style| style.text_color(t.text1))
                    .child("Cancel")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.license_error = None;
                        app.update(cx, |m, cx| m.close_registration(cx));
                    })),
            );
        }
        let buy = |id: &'static str, name: &'static str, price: String, url: &'static str| {
            v_flex()
                .id(id)
                .role(Role::Link)
                .aria_label(format!("Buy {name}, {price}"))
                .tabbable(cx)
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
        panel
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
            )
    }

    // ── Settings ──

    /// Everything the app remembers per device, in one panel: how it looks,
    /// what its menus launch, how often it checks upstream, the `jiji`
    /// command, and the licence.
    fn render_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let open = self.settings_menu.is_open();
        let model = self.app.read(cx);
        // A registered copy replacing its key does so from here.
        let replacing = model.license.registered && model.registration_open;
        let anchor = div()
            .relative()
            .debug_selector(|| "settings-button".into())
            .child(
                icon_btn("settings", IconName::Settings)
                    .size(26., 15.)
                    .on(open || replacing)
                    .expanded(open || replacing)
                    .tooltip("Settings")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.settings_menu.toggle(window, cx);
                        this.cli_error = None;
                        cx.notify();
                    })),
            );
        if replacing {
            let panel = self.license_panel(window, cx);
            return anchor.child(self.license_layer(panel, cx));
        }
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
        let (registered, plan, display_key) = (
            model.license.registered,
            model.license.plan.clone(),
            model.license.display_key.clone(),
        );

        // A choice among several: a radio button, ticked when it is the one.
        let row =
            |id: gpui::ElementId, label: SharedString, hint: Option<SharedString>, on: bool| {
                let mut el = h_flex()
                    .id(id)
                    .role(Role::RadioButton)
                    .aria_label(match &hint {
                        Some(hint) => SharedString::from(format!("{label}, {hint}")),
                        None => label.clone(),
                    })
                    .aria_toggled(if on {
                        gpui::Toggled::True
                    } else {
                        gpui::Toggled::False
                    })
                    .tabbable_in(t.accent)
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
        let section = || v_flex().p(px(4.)).border_t_1().border_color(t.border2);

        let mut panel = popover(cx).w(px(288.)).flex().flex_col();
        panel = panel.child(self.render_appearance(cx));

        let mut launch = section().child(menu_label("Preferred editor", cx));
        let choices = |id: &'static str, name: &'static str| {
            v_flex().id(id).role(Role::RadioGroup).aria_label(name)
        };
        if editors.is_empty() {
            launch = launch.child(note(
                "No editor command-line tools found on this machine.".into(),
            ));
        } else {
            let app = self.app.clone();
            let mut group = choices("editor-choices", "Preferred editor").child(
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
                group = group.child(
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
            launch = launch.child(group);
        }
        if !terminals.is_empty() {
            launch = launch.child(menu_label("Terminal", cx));
            let mut group = choices("terminal-choices", "Terminal");
            for (ix, terminal) in terminals.iter().enumerate() {
                let app = self.app.clone();
                let id = terminal.id;
                group = group.child(
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
            launch = launch.child(group);
        }
        panel = panel.child(launch);

        let app = self.app.clone();
        let mut machine = section().child(menu_label("Check upstream", cx)).child(
            div().px(px(8.)).pb(px(6.)).child(segmented(
                "upstream-interval",
                "Check upstream",
                UPSTREAM_INTERVALS
                    .iter()
                    .map(|&(minutes, label)| Segment::text(minutes, label))
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
            machine = machine.child(menu_label("Command line", cx));
            if cli.installed {
                let path = cli.path.unwrap_or_default();
                machine = machine.child(
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
                        .child(btn("cli-uninstall", "Uninstall").small().danger().on_click(
                            cx.listener(|this, _, _, cx| {
                                let task = this.app.update(cx, |m, cx| m.uninstall_cli(cx));
                                this.run_cli(task, cx);
                            }),
                        )),
                );
            } else if let Some(conflict) = cli.conflict {
                machine = machine.child(note(format!("Another jiji occupies {conflict}.")));
            } else {
                machine = machine.child(
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
                machine = machine.child(
                    div()
                        .id("cli-error")
                        .alert(error.clone())
                        .px(px(8.))
                        .pb(px(6.))
                        .text_size(px(TEXT_XS))
                        .text_color(t.danger)
                        .child(error.clone()),
                );
            }
        }
        panel = panel.child(machine);

        // The footer: who this copy belongs to, and which build it is.
        let mut footer = h_flex()
            .gap(px(6.))
            .h(px(34.))
            .px(px(12.))
            .border_t_1()
            .border_color(t.border2)
            .text_size(px(TEXT_XS))
            .text_color(t.text3);
        if registered {
            let plan = match plan.as_str() {
                "solo" => "Solo license",
                "personal" => "Personal license",
                _ => "Licensed",
            };
            let app = self.app.clone();
            footer = footer
                .child(icon(IconName::BadgeCheck, 13., t.ok))
                .child(div().text_color(t.text2).child(plan))
                .child(
                    div()
                        .id("license-replace")
                        .role(Role::Button)
                        .aria_label("Replace key")
                        .tabbable(cx)
                        .text_color(t.accent_strong)
                        .cursor_pointer()
                        .hover(|style| style.text_color(t.text1))
                        // A licence file that cannot be read right now
                        // keeps the registration but not the key.
                        .when(!display_key.is_empty(), |el| {
                            el.tooltip(tooltip(format!("Key {display_key}")))
                        })
                        .child("Replace key…")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.settings_menu.close(window, cx);
                            app.update(cx, |m, cx| m.open_registration(cx));
                        })),
                );
        }
        footer = footer.child(div().flex_1()).child(
            h_flex().gap(px(4.)).child("Jiji").child(
                div()
                    .font_family(t.mono.clone())
                    .child(crate::host::update::CURRENT_VERSION),
            ),
        );
        panel = panel.child(footer);

        anchor.child(popover_layer(
            Anchor::TopRight,
            30.,
            self.settings_menu
                .panel("settings-panel", Role::Dialog, "Settings", panel),
            cx.listener(|this, _, window, cx| {
                this.settings_menu.close(window, cx);
                cx.notify();
            }),
        ))
    }

    /// Settings' first section: the mode, and a swatch per palette — or,
    /// before registration, what unlocks them.
    fn render_appearance(&mut self, cx: &mut Context<Self>) -> Div {
        let t = theme(cx);
        let model = self.app.read(cx);
        let mode = model.theme_mode();
        let registered = model.license.registered;
        let pref = model.theme_pref();
        let mut section = v_flex().p(px(4.)).child(menu_label("Appearance", cx));
        if !registered {
            let app = self.app.clone();
            return section.child(
                h_flex()
                    .gap(px(10.))
                    .px(px(8.))
                    .pb(px(8.))
                    .child(icon(IconName::Lock, 14., t.text3))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(TEXT_S))
                            .line_height(gpui::relative(1.4))
                            .text_color(t.text2)
                            .child("Ten palettes come with a license."),
                    )
                    .child(btn("unlock-themes", "Unlock").small().on_click(cx.listener(
                        move |this, _, window, cx| {
                            this.settings_menu.close(window, cx);
                            app.update(cx, |m, cx| m.open_registration(cx));
                            cx.notify();
                        },
                    ))),
            );
        }
        let app = self.app.clone();
        section = section.child(div().px(px(8.)).pb(px(4.)).child(segmented(
            "theme-mode",
            "Appearance",
            vec![
                Segment::text(ThemeMode::System, "System").with_icon(IconName::SunMoon),
                Segment::text(ThemeMode::Light, "Light").with_icon(IconName::Sun),
                Segment::text(ThemeMode::Dark, "Dark").with_icon(IconName::Moon),
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
            let mut swatches = h_flex()
                .id(SharedString::from(format!("swatches-{label}")))
                .role(Role::RadioGroup)
                .aria_label(format!("{label} palette"))
                .gap(px(6.));
            for def in defs.iter().filter(|def| def.scheme == scheme) {
                let selected = def.id == current;
                let app = self.app.clone();
                let id = def.id;
                swatches = swatches.child(
                    div()
                        .id(SharedString::from(format!("swatch-{id}")))
                        .role(Role::RadioButton)
                        .aria_label(def.label)
                        .aria_toggled(if selected {
                            gpui::Toggled::True
                        } else {
                            gpui::Toggled::False
                        })
                        .tabbable(cx)
                        .flex()
                        .items_end()
                        .w(px(40.))
                        .h(px(26.))
                        .p(px(4.))
                        .rounded(px(RADIUS_S))
                        .bg(def.bg)
                        .border_1()
                        .border_color(if selected { t.accent } else { t.border1 })
                        .when(selected, |el| el.border_2())
                        .cursor_pointer()
                        .tooltip(tooltip(def.label))
                        .on_click(move |_, _, cx| app.update(cx, |m, cx| m.select_theme(id, cx)))
                        .child(div().w(px(14.)).h(px(3.)).rounded(px(2.)).bg(def.accent)),
                );
            }
            section = section.child(
                h_flex()
                    .gap(px(10.))
                    .px(px(8.))
                    .py(px(4.))
                    .child(
                        div()
                            .w(px(34.))
                            .flex_shrink_0()
                            .text_size(px(TEXT_XS))
                            .text_color(t.text3)
                            .child(label),
                    )
                    .child(swatches),
            );
        }
        section.child(div().h(px(4.)))
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

    #[test]
    fn the_command_field_narrows_before_anything_else() {
        assert!(command_field_width(1440.) > command_field_width(1200.));
        assert!(command_field_width(1200.) > command_field_width(980.));
    }
}
