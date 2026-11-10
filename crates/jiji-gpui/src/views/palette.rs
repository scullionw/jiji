//! The command palette (⌘K): the keyboard route to every action. Opens
//! and closes instantly — whatever a command opens (a panel, a jumped
//! selection) appears without a lag.

use gpui::{
    actions, div, prelude::*, px, AnyView, App, Context, Entity, FontWeight, KeyBinding,
    MouseButton, ScrollHandle, Subscription, Window,
};

use crate::app::{app, tab_label, tab_titles, AppModel};
use crate::logic::autoland::visible_jobs;
use crate::logic::inspect::find_node;
use crate::logic::menu::MenuAction;
use crate::logic::palette::{
    palette_results, CliState, GlyphTone, OpenTab, PaletteAction, PaletteContext, PaletteItem,
    ThemeChoice,
};
use crate::theme::{theme, theme_defs, Theme, RADIUS_M, TEXT_M, TEXT_S, TEXT_XS};
use crate::ui::{icon, kbd, popover, scrollbar, truncated, IconName, InputEvent, TextInput};
use crate::views::workbench::VIEW_PRESETS;

actions!(palette, [PaletteUp, PaletteDown, PaletteHold]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", PaletteUp, Some("Palette")),
        KeyBinding::new("down", PaletteDown, Some("Palette")),
        // The palette is a single control; focus stays on the input.
        KeyBinding::new("tab", PaletteHold, Some("Palette")),
        KeyBinding::new("shift-tab", PaletteHold, Some("Palette")),
    ]);
}

struct Palette {
    app: Entity<AppModel>,
    input: Entity<TextInput>,
    items: Vec<PaletteItem>,
    active: usize,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl Palette {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let app = app(cx);
        let placeholder = if app.read(cx).snapshot().is_some() {
            "Type a command, change id, or bookmark…"
        } else {
            "Type a command…"
        };
        let input = cx.new(|cx| TextInput::new(window, cx).placeholder(placeholder));
        input.update(cx, |input, _| input.focus(window));
        let subscriptions = vec![
            cx.subscribe_in(
                &input,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => {
                        this.active = 0;
                        this.refresh(cx);
                        this.scroll.set_offset(gpui::Point::default());
                    }
                    InputEvent::Submit => this.run(this.active, cx),
                    _ => {}
                },
            ),
            cx.observe(&app, |this, _, cx| this.refresh(cx)),
        ];
        let mut this = Self {
            app,
            input,
            items: Vec::new(),
            active: 0,
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        this.refresh(cx);
        this
    }

    fn query(&self, cx: &App) -> String {
        self.input.read(cx).text().to_owned()
    }

    /// Rebuild the list for the model as it stands.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx);
        let model = self.app.read(cx);
        let snapshot = model.snapshot().cloned();
        let selected = snapshot
            .as_deref()
            .and_then(|snapshot| find_node(snapshot, model.ui().selected_node.as_deref()?));
        let themes: Vec<ThemeChoice> = theme_defs()
            .into_iter()
            .map(|def| ThemeChoice {
                id: def.id,
                label: def.label,
                scheme: def.scheme,
            })
            .collect();
        let (landable, shippable) = (model.landable_stacks(), model.shippable_stacks());
        let repo_path = model.repo_path();
        let jobs = visible_jobs(&model.autoland.jobs, repo_path.as_deref());
        let entries: Vec<_> = model
            .tabs
            .iter()
            .map(|tab| (tab.id, tab.name.as_deref(), tab.path.as_deref()))
            .collect();
        let open_tabs: Vec<OpenTab> = tab_titles(&entries)
            .into_iter()
            .zip(&model.tabs)
            .map(|((id, title), tab)| OpenTab {
                id,
                label: match title.hint {
                    Some(hint) => format!("{} — {hint}", title.label),
                    None => tab_label(tab.name.as_deref(), tab.path.as_deref()),
                },
                active: id == model.active_id,
            })
            .collect();
        let reopenable = model.reopenable_tab();
        self.items = palette_results(
            &PaletteContext {
                snapshot: snapshot.as_deref(),
                selected,
                recent_repos: &model.recent_repos,
                can_undo: model.can_undo(),
                registered: model.license.registered,
                themes: &themes,
                lenses: &VIEW_PRESETS,
                landable: &landable,
                shippable: &shippable,
                autoland_jobs: &jobs,
                cli: model.cli.status.as_ref().map(|status| CliState {
                    installed: status.installed,
                    upgrade_available: status.upgrade_available,
                    path: status.path.as_deref(),
                    conflict: status.conflict.is_some(),
                }),
                open_tabs: &open_tabs,
                reopenable_tab: reopenable.as_deref(),
            },
            &query,
        );
        self.active = self.active.min(self.items.len().saturating_sub(1));
        cx.notify();
    }

    /// Group headers only make sense for the browsable default list; query
    /// results are one flat ranked list.
    fn grouped(&self, cx: &App) -> bool {
        self.query(cx).trim().is_empty()
    }

    fn travel(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.items.is_empty() {
            return;
        }
        let last = self.items.len() as isize - 1;
        self.active = (self.active as isize + delta).clamp(0, last) as usize;
        // Keep the active row in view: its place among the list's
        // children counts the group labels above it.
        let labels = if self.grouped(cx) {
            (0..=self.active)
                .filter(|&ix| ix == 0 || self.items[ix - 1].group != self.items[ix].group)
                .count()
        } else {
            0
        };
        if self.active == 0 {
            self.scroll.set_offset(gpui::Point::default());
        } else {
            self.scroll.scroll_to_item(self.active + labels);
        }
        cx.notify();
    }

    fn run(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(action) = self.items.get(index).map(|item| item.action.clone()) else {
            return;
        };
        self.app.update(cx, |model, cx| {
            model.close_palette(cx);
            match action {
                PaletteAction::ChooseRepo => model.choose_repo(cx),
                PaletteAction::OpenRecent(path) => model.open_repo_at(path, cx),
                PaletteAction::NewTab => model.new_tab(cx),
                PaletteAction::CloseTab => model.close_active_tab(cx),
                PaletteAction::ReopenTab => model.reopen_closed_tab(cx),
                PaletteAction::SwitchTab(id) => model.activate_tab(id, cx),
                PaletteAction::Refresh => model.refresh_snapshot(cx),
                PaletteAction::FetchUpstream => model.check_upstream(true, cx),
                PaletteAction::Undo => model.undo_last_mutation(cx),
                PaletteAction::NewChild(id) => model.run_action(MenuAction::NewChild(id), cx),
                PaletteAction::Edit(id) => model.run_action(MenuAction::Edit(id), cx),
                PaletteAction::Revert(id) => model.run_action(MenuAction::Revert(id), cx),
                PaletteAction::Intent(intent) => model.send_intent(intent, cx),
                PaletteAction::Section(section) => model.go_to_section(section, cx),
                PaletteAction::Goto(id) => model.jump_to_change(&id, cx),
                PaletteAction::SearchHistory(query) => {
                    model.open_search(query.unwrap_or_default(), cx)
                }
                PaletteAction::Mode(mode) => model.set_theme_mode(mode, cx),
                PaletteAction::Theme(id) => model.select_theme(id, cx),
                PaletteAction::SnapshotView(view) => {
                    let path = model.repo_path();
                    let task = model.set_snapshot_view(view, true, cx);
                    model.report(path, task, cx);
                }
                PaletteAction::ToggleSidebar => model.toggle_sidebar(cx),
                PaletteAction::ToggleGraphPanel => model.toggle_graph_panel(cx),
                PaletteAction::AutolandStop => model.stop_autoland(cx),
                PaletteAction::AutolandResume(bookmark) => {
                    let path = model.repo_path();
                    let task = model.start_autoland(bookmark, None, cx);
                    model.report(path, task, cx);
                }
                PaletteAction::InstallCli => {
                    let path = model.repo_path();
                    let task = model.install_cli(cx);
                    model.report(path, task, cx);
                }
                PaletteAction::UninstallCli => {
                    let path = model.repo_path();
                    let task = model.uninstall_cli(cx);
                    model.report(path, task, cx);
                }
            }
        });
    }

    fn render_row(
        &self,
        item: &PaletteItem,
        index: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let active = index == self.active;
        let lead = match (&item.glyph, item.icon) {
            (Some((glyph, tone)), _) => div()
                .w(px(18.))
                .flex_shrink_0()
                .flex()
                .justify_center()
                .font_family(t.mono.clone())
                .text_size(px(TEXT_S))
                .text_color(match tone {
                    GlyphTone::WorkingCopy => t.working_copy,
                    GlyphTone::Mutable => t.accent,
                    GlyphTone::Immutable => t.text3,
                })
                .child(*glyph),
            (None, glyph) => div()
                .w(px(18.))
                .flex_shrink_0()
                .flex()
                .justify_center()
                .children(glyph.map(|glyph| {
                    let color = match (item.danger, active) {
                        (true, _) => t.danger,
                        (false, true) => t.text1,
                        (false, false) => t.text3,
                    };
                    icon(glyph, 15., color)
                })),
        };
        let mut title = div().flex_shrink_0().max_w(gpui::relative(0.6));
        if item.glyph.is_some() {
            title = title.font_family(t.mono.clone()).text_size(px(TEXT_S));
        }
        if item.danger {
            title = title.text_color(t.danger);
        }
        let mut row = div()
            .id(("palette-row", index))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .h(px(32.))
            .flex_shrink_0()
            .px(px(8.))
            .rounded(px(RADIUS_M))
            .text_size(px(TEXT_M))
            .text_color(t.text1)
            .child(lead)
            .child(title.child(truncated(item.title.clone())));
        if active {
            row = row.bg(if item.danger {
                t.danger_dim
            } else {
                Theme::fade(t.text1, 0.08)
            });
        }
        row = match &item.hint {
            Some(hint) => row.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(TEXT_S))
                    .text_color(t.text3)
                    .child(truncated(hint.clone())),
            ),
            None => row.child(div().flex_1()),
        };
        if let Some(keys) = &item.shortcut {
            row = row.child(kbd(keys.clone(), cx).flex_shrink_0());
        }
        // Pointer travel moves the highlight; a list scrolling under a
        // resting pointer does not.
        row.on_mouse_move(cx.listener(move |this, _, _, cx| {
            if this.active != index {
                this.active = index;
                cx.notify();
            }
        }))
        .on_click(cx.listener(move |this, _, _, cx| this.run(index, cx)))
    }
}

impl Render for Palette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let viewport = window.viewport_size();
        let width = (f32::from(viewport.width) - 96.).min(620.);
        let max_height = (f32::from(viewport.height) * 0.72).min(480.);
        let grouped = self.grouped(cx);

        let mut list = div()
            .id("palette-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .pt(px(4.))
            .px(px(8.))
            .pb(px(8.));
        if self.items.is_empty() {
            list = list.child(
                div()
                    .p(px(20.))
                    .flex()
                    .justify_center()
                    .text_size(px(TEXT_S))
                    .text_color(t.text3)
                    .child("No matching command"),
            );
        }
        for index in 0..self.items.len() {
            let item = &self.items[index];
            if grouped && (index == 0 || self.items[index - 1].group != item.group) {
                list = list.child(
                    div()
                        .flex_shrink_0()
                        .pt(px(if index == 0 { 8. } else { 12. }))
                        .px(px(8.))
                        .pb(px(4.))
                        .text_size(px(TEXT_XS))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.text3)
                        .child(item.group),
                );
            }
            list = list.child(self.render_row(item, index, cx));
        }

        let panel = popover(cx)
            .key_context("Palette")
            .on_action(cx.listener(|this, _: &PaletteUp, _, cx| this.travel(-1, cx)))
            .on_action(cx.listener(|this, _: &PaletteDown, _, cx| this.travel(1, cx)))
            .on_action(|_: &PaletteHold, _, _| {})
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
                    .child(scrollbar("palette-scroll", &self.scroll)),
            );

        let app = self.app.clone();
        div()
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            .items_start()
            .pt(px(f32::from(viewport.height) * 0.12))
            .bg(Theme::fade(t.bg0, 0.3))
            .occlude()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                app.update(cx, |model, cx| model.close_palette(cx));
            })
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(panel)
    }
}

pub fn build(window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| Palette::new(window, cx)).into()
}
