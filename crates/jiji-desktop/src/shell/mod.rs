//! The frame: a title bar across the top, the section rail down the left,
//! a status line along the bottom — one L of bg0, a clear step darker —
//! around one full-bleed working area on bg1. The open tab is cut from the
//! working area's surface and runs down into it. Hairlines are the joinery.

pub mod placement;
mod statusbar;
pub mod topbar;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    actions, div, prelude::*, px, AnyView, App, Context, Entity, ExternalPaths, FocusHandle,
    Focusable, FontWeight, KeyBinding, MouseButton, MouseDownEvent, Orientation, Role,
    Subscription, Task, Window,
};

use crate::app::{app, AppEvent, AppModel, Section, UiIntent, SECTIONS};
use crate::theme::{theme, FONT_UI, RADIUS_M, RAIL_W, TEXT_M};
use crate::ui::focus::{FocusNext, FocusPrev, Return, Trap};
use crate::ui::{icon, popover, tooltip, v_flex, IconName, PopoverState, Tabbable as _};

actions!(
    jiji,
    [
        TogglePalette,
        OpenRepo,
        FindInDiff,
        SearchHistory,
        Refresh,
        ToggleSidebar,
        ToggleGraph,
        NewTab,
        ReopenTab,
        CloseTab,
        NextTab,
        PrevTab,
        Section1,
        Section2,
        Section3,
        Section4,
        Section5,
        Section6,
        CheckUpdates,
        About,
        Help,
        Quit,
        Hide,
        HideOthers,
        ShowAll,
        Minimize,
        Zoom,
        ToggleFullScreen,
        /// Escape: close whatever floats on top.
        Dismiss,
    ]
);

const REPO_URL: &str = "https://github.com/scullionw/jiji";
/// The mutation breadcrumb takes over the status line briefly, then the
/// bar falls back to the snapshot's latest operation.
const BREADCRUMB_LIFE: Duration = Duration::from_secs(8);

/// Shortcuts that work from anywhere in the window.
pub fn bind_keys(cx: &mut App) {
    crate::ui::focus::bind_keys(cx);
    cx.bind_keys([
        KeyBinding::new("cmd-k", TogglePalette, None),
        KeyBinding::new("cmd-o", OpenRepo, None),
        // ⌘F finds within the diff on screen; ⌘⇧F searches all of history.
        // Typed while writing text (a describe editor) it is left alone —
        // except in the find bar's own input, which re-selects its query.
        KeyBinding::new("cmd-f", FindInDiff, Some("!TextInput")),
        KeyBinding::new("cmd-f", FindInDiff, Some("FindBar")),
        KeyBinding::new("cmd-shift-f", SearchHistory, None),
        KeyBinding::new("cmd-r", Refresh, None),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
        // ⌘⇧B pairs with ⌘B: the graph pane joins the rail out of the
        // way, leaving the diff the whole window.
        KeyBinding::new("cmd-shift-b", ToggleGraph, None),
        // Tabs, like every browser.
        KeyBinding::new("cmd-t", NewTab, None),
        KeyBinding::new("cmd-shift-t", ReopenTab, None),
        KeyBinding::new("cmd-w", CloseTab, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PrevTab, None),
        KeyBinding::new("cmd-shift-]", NextTab, None),
        KeyBinding::new("cmd-shift-[", PrevTab, None),
        KeyBinding::new("cmd-}", NextTab, None),
        KeyBinding::new("cmd-{", PrevTab, None),
        KeyBinding::new("cmd-1", Section1, None),
        KeyBinding::new("cmd-2", Section2, None),
        KeyBinding::new("cmd-3", Section3, None),
        KeyBinding::new("cmd-4", Section4, None),
        KeyBinding::new("cmd-5", Section5, None),
        KeyBinding::new("cmd-6", Section6, None),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
        KeyBinding::new("escape", Dismiss, None),
    ]);
}

/// App-level actions: they act with or without a window in front.
pub fn register_app_actions(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Help, cx| cx.open_url(REPO_URL));
    cx.on_action(|_: &CheckUpdates, cx| {
        app(cx).update(cx, |model, cx| model.check_for_update_now(cx));
    });
    cx.on_action(|_: &OpenRepo, cx| {
        app(cx).update(cx, |model, cx| model.choose_repo(cx));
    });
}

/// The native menu bar. Items mirror the shell's actions and show the same
/// shortcuts.
pub fn menus() -> Vec<gpui::Menu> {
    use crate::ui::input;
    use gpui::{Menu, MenuItem, OsAction, SystemMenuType};
    vec![
        Menu {
            name: "Jiji".into(),
            disabled: false,
            items: vec![
                MenuItem::action("About Jiji", About),
                MenuItem::action("Check for Updates…", CheckUpdates),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide Jiji", Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action("Quit Jiji", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            disabled: false,
            items: vec![
                MenuItem::action("New Tab", NewTab),
                MenuItem::action("Open Repository…", OpenRepo),
                MenuItem::separator(),
                MenuItem::action("Close Tab", CloseTab),
                MenuItem::action("Reopen Closed Tab", ReopenTab),
            ],
        },
        Menu {
            name: "Edit".into(),
            disabled: false,
            items: vec![
                MenuItem::os_action("Undo", input::Undo, OsAction::Undo),
                MenuItem::os_action("Redo", input::Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
                MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
                MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
                MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
                MenuItem::separator(),
                MenuItem::action("Find in Diff", FindInDiff),
                MenuItem::action("Search History…", SearchHistory),
            ],
        },
        Menu {
            name: "View".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Command Palette…", TogglePalette),
                MenuItem::separator(),
                MenuItem::action("Workbench", Section1),
                MenuItem::action("Conflicts", Section2),
                MenuItem::action("Publish", Section3),
                MenuItem::action("Bookmarks", Section4),
                MenuItem::action("Operations", Section5),
                MenuItem::action("Workspaces", Section6),
                MenuItem::separator(),
                MenuItem::action("Toggle Sidebar", ToggleSidebar),
                MenuItem::action("Toggle Graph Panel", ToggleGraph),
                MenuItem::action("Refresh", Refresh),
                MenuItem::separator(),
                MenuItem::action("Enter Full Screen", ToggleFullScreen),
            ],
        },
        Menu {
            name: "Window".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
                MenuItem::separator(),
                MenuItem::action("Show Previous Tab", PrevTab),
                MenuItem::action("Show Next Tab", NextTab),
            ],
        },
        Menu {
            name: "Help".into(),
            disabled: false,
            items: vec![MenuItem::action("Jiji Help", Help)],
        },
    ]
}

pub struct Shell {
    app: Entity<AppModel>,
    focus: FocusHandle,
    /// The section view on screen and what it was built for.
    content: Option<((u64, Option<Section>), AnyView)>,
    /// Where keyboard focus rests while no overlay or field has it: the
    /// section view, so its bare-key shortcuts work without a click.
    content_focus: Option<FocusHandle>,
    /// Folders a Finder drag hovering the window would open.
    drop_paths: Rc<RefCell<Vec<PathBuf>>>,
    undoing: bool,
    breadcrumb_at: Option<Instant>,
    breadcrumb_timer: Option<Task<()>>,
    settings_menu: PopoverState,
    license_error: Option<String>,
    license_busy: bool,
    cli_error: Option<String>,
    license_input: Entity<crate::ui::TextInput>,
    palette: Option<AnyView>,
    search: Option<AnyView>,
    /// Where focus goes back to when the palette, history search or the
    /// registration panel closes.
    palette_back: Return,
    search_back: Return,
    register_back: Return,
    /// The registration panel was open at the last sync.
    registering: bool,
    register_trap: Trap,
    /// The context menu on screen, by the opening it was built for.
    menu: Option<(u64, Entity<crate::views::menu::ContextMenu>)>,
    /// Waiting for the window to hold still before keeping where it is.
    window_settle: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for Shell {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Shell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = app(cx);
        model.update(cx, |model, _| model.window = Some(window.window_handle()));
        let license_input = cx
            .new(|cx| crate::ui::TextInput::new(window, cx).placeholder("Paste your license key"));
        let subscriptions = vec![
            cx.observe_in(&model, window, |this, _, window, cx| {
                this.sync(window, cx);
                cx.notify();
            }),
            cx.subscribe_in(&model, window, |this, _, event: &AppEvent, window, cx| {
                if *event == AppEvent::Navigation {
                    this.content = None;
                    this.sync(window, cx);
                }
            }),
            // Following the OS switches between the user's dark and light
            // palettes.
            cx.observe_window_appearance(window, |this, _, cx| {
                this.app.update(cx, |model, cx| model.apply_theme(cx));
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.app.update(cx, |model, _| model.window_activated());
                }
            }),
            // Where the window was left survives restarts, once it holds
            // still: every change starts the wait again.
            cx.observe_window_bounds(window, |this, window, cx| {
                this.window_settle = Some(cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(placement::SETTLE).await;
                    let _ = this.update_in(cx, |this, window, cx| this.keep_window(window, cx));
                }));
            }),
            cx.subscribe_in(
                &license_input,
                window,
                |this, _, event: &crate::ui::InputEvent, window, cx| {
                    if *event == crate::ui::InputEvent::Submit {
                        this.activate_license(window, cx);
                    }
                },
            ),
            // The focused control went away (its row was deleted, the panel
            // it sat in closed): the keys go to the nearest thing still on
            // screen that takes them, the section rather than the window.
            cx.on_focus_lost(window, |this, window, cx| {
                match window
                    .focus_lost_restore_target(cx)
                    .filter(|target| *target != this.focus)
                {
                    Some(target) => window.focus(&target, cx),
                    None => this.rest_focus(window, cx),
                }
            }),
        ];
        let mut this = Self {
            app: model,
            focus: cx.focus_handle(),
            content: None,
            content_focus: None,
            drop_paths: Rc::default(),
            undoing: false,
            breadcrumb_at: None,
            breadcrumb_timer: None,
            settings_menu: PopoverState::new(cx),
            license_error: None,
            license_busy: false,
            cli_error: None,
            license_input,
            palette: None,
            search: None,
            palette_back: Return::default(),
            search_back: Return::default(),
            register_back: Return::default(),
            registering: false,
            register_trap: Trap::new(cx),
            menu: None,
            window_settle: None,
            _subscriptions: subscriptions,
        };
        this.sync(window, cx);
        this.rest_focus(window, cx);
        this
    }

    fn keep_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let seen = placement::seen(window, cx);
        self.app.update(cx, |model, _| {
            let state = placement::settle(model.store.window().as_ref(), seen);
            model.store.set_window(&state);
        });
    }

    /// Keep what the shell derives from the model in step: the section
    /// view, the window's name, the breadcrumb's lifetime, the overlays.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let model = self.app.read(cx);
        let key = model.view_key();
        // The window's name in Mission Control, the Dock and ⌘-Tab follows
        // the repo on screen (the title bar itself never shows it).
        let title = match model.snapshot() {
            Some(snapshot) => format!("{} — Jiji", snapshot.repo_name),
            None => "Jiji".to_owned(),
        };
        let breadcrumb = model.ui().last_mutation.as_ref().map(|crumb| crumb.at);
        let (palette_open, search_open) = (model.palette_open, model.search_open);
        let registering = model.registration_open;
        let menu_seq = model.menu.as_ref().map(|menu| menu.seq);
        window.set_window_title(&title);

        if self.content.as_ref().map(|(built, _)| *built) != Some(key) {
            let (view, focus) = crate::views::build(key.1, window, cx);
            self.content = Some((key, view));
            self.content_focus = focus;
            if !palette_open && !search_open {
                self.rest_focus(window, cx);
            }
        }

        if breadcrumb != self.breadcrumb_at {
            self.breadcrumb_at = breadcrumb;
            self.breadcrumb_timer = breadcrumb.map(|at| {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(BREADCRUMB_LIFE).await;
                    let _ = this.update(cx, |this, cx| {
                        if this.undoing {
                            return;
                        }
                        this.app.update(cx, |model, cx| {
                            let current = model.ui().last_mutation.as_ref().map(|c| c.at);
                            if current == Some(at) {
                                model.dismiss_breadcrumb(cx);
                            }
                        });
                    });
                })
            });
        }

        if palette_open && self.palette.is_none() {
            self.palette_back.take(window, cx);
            self.palette = Some(crate::views::palette::build(window, cx));
        } else if !palette_open && self.palette.take().is_some() {
            self.rest_focus(window, cx);
            self.palette_back.give_back(window, cx);
        }
        if self.menu.as_ref().map(|(seq, _)| *seq) != menu_seq {
            self.menu = crate::views::menu::build(&self.app, window, cx);
        }
        if search_open && self.search.is_none() {
            self.search_back.take(window, cx);
            self.search = Some(crate::views::search::build(window, cx));
        } else if !search_open && self.search.take().is_some() {
            self.rest_focus(window, cx);
            self.search_back.give_back(window, cx);
        }
        // The registration panel opens with its key field ready to paste
        // into, and gives focus back when it closes.
        if registering != std::mem::replace(&mut self.registering, registering) {
            if registering {
                self.register_back.take(window, cx);
                self.license_input
                    .update(cx, |input, cx| input.focus(window, cx));
            } else {
                self.register_back.give_back(window, cx);
            }
        }
    }

    /// The section view on screen, when it is a `V`.
    #[cfg(test)]
    pub fn section<V: 'static>(&self) -> Option<Entity<V>> {
        let (_, view) = self.content.as_ref()?;
        view.clone().downcast().ok()
    }

    fn rest_focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(self.content_focus.as_ref().unwrap_or(&self.focus), cx);
    }

    fn go_to(&mut self, index: usize, cx: &mut Context<Self>) {
        self.app
            .update(cx, |model, cx| model.go_to_section(SECTIONS[index], cx));
    }

    fn activate_license(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.license_busy {
            return;
        }
        let key = self.license_input.read(cx).text().to_owned();
        self.license_busy = true;
        self.license_error = None;
        cx.notify();
        let task = self
            .app
            .update(cx, |model, cx| model.activate_license(key, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.license_busy = false;
                match result {
                    Ok(()) => {
                        this.license_input
                            .update(cx, |input, cx| input.set_text("", cx));
                        // The key took: Settings opens in the panel's
                        // place, its footer naming the licence and its
                        // palettes now unlocked.
                        this.app.update(cx, |m, cx| m.close_registration(cx));
                        this.settings_menu.open(window, cx);
                    }
                    Err(message) => this.license_error = Some(message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        if self.undoing {
            return;
        }
        self.undoing = true;
        self.app
            .update(cx, |model, cx| model.undo_last_mutation(cx));
        // The undo's own breadcrumb replaces this one when it lands.
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(600))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.undoing = false;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_menu.is_open() {
            self.settings_menu.close(window, cx);
            cx.notify();
            return;
        }
        let handled = self.app.update(cx, |model, cx| {
            if model.registration_open {
                model.close_registration(cx);
                true
            } else if model.palette_open {
                model.close_palette(cx);
                true
            } else if model.search_open {
                model.close_search(cx);
                true
            } else {
                false
            }
        });
        if !handled {
            cx.propagate();
        }
    }

    fn render_rail(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let t = theme(cx);
        let model = self.app.read(cx);
        // No repo, no sections: the welcome screen gets the whole window.
        let snapshot = model.snapshot()?;
        if model.sidebar_collapsed {
            return None;
        }
        let active = model.section();
        // The inbox count rides the rail so conflict state is never
        // hidden behind a section switch.
        let conflicts = snapshot.conflicts.len();
        let items = SECTIONS.map(|section| (section, section.glyph()));
        // To assistive technology the rail is what it does: a vertical tab
        // list whose tabs switch the section filling the window.
        let mut rail = v_flex()
            .id("rail")
            .role(Role::TabList)
            .aria_label("Sections")
            .aria_orientation(Orientation::Vertical)
            .w(px(RAIL_W))
            .flex_shrink_0()
            .items_center()
            .gap(px(4.))
            .pt(px(8.))
            .border_r_1()
            .border_color(t.border2);
        for (ix, (section, glyph)) in items.into_iter().enumerate() {
            let is_active = section == active;
            let group = format!("rail-{ix}");
            let shortcut = format!("⌘{}", ix + 1);
            let mut item = div()
                .id(("rail", ix))
                .role(Role::Tab)
                .aria_label(section.label())
                .aria_selected(is_active)
                .aria_keyshortcuts(shortcut.clone())
                .tabbable(cx)
                .group(group.clone())
                .relative()
                .flex()
                .items_center()
                .justify_center()
                .size(px(32.))
                .rounded(px(RADIUS_M + 1.))
                .cursor_pointer()
                .tooltip(tooltip(format!("{} ({shortcut})", section.label())))
                .on_click(cx.listener(move |this, _, _, cx| this.go_to(ix, cx)));
            item = if is_active {
                // The current section: its glyph in its own hue on a tint
                // of it, the one colored tile on the rail.
                let hue = section.hue(&t);
                item.bg(crate::theme::Theme::fade(
                    hue,
                    if t.is_dark() { 0.16 } else { 0.12 },
                ))
                .child(icon(glyph, 17., hue))
            } else {
                item.hover(|style| style.bg(t.bg_hover)).child(
                    icon(glyph, 17., t.text3).group_hover(group, |style| style.text_color(t.text1)),
                )
            };
            if section == Section::Conflicts && conflicts > 0 {
                item = item.aria_description(crate::logic::text::counted(conflicts, "conflict"));
                item = item.child(
                    div()
                        .absolute()
                        .top(px(3.))
                        .right(px(2.))
                        .min_w(px(14.))
                        .h(px(14.))
                        .px(px(3.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(7.))
                        .bg(t.danger)
                        .border_2()
                        .border_color(t.bg0)
                        .text_color(gpui::white())
                        .text_size(px(9.))
                        .font_weight(FontWeight::BOLD)
                        .child(if conflicts > 9 {
                            "9+".to_owned()
                        } else {
                            conflicts.to_string()
                        }),
                );
            }
            rail = rail.child(item);
        }
        Some(rail)
    }

    fn render_drop_overlay(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let paths = self.drop_paths.borrow();
        let label = match paths.as_slice() {
            [] => "Open in Jiji".to_owned(),
            [only] => format!(
                "Open \u{201c}{}\u{201d}",
                only.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
            many => format!("Open {} folders", many.len()),
        };
        let recorded = self.drop_paths.clone();
        let (scrim, accent) = (t.scrim, t.accent);
        // Finder drags open repos on drop, like the Dock icon does. The
        // overlay only answers the drag; it never takes pointer events.
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .invisible()
            .drag_over::<ExternalPaths>(move |style, paths, _, _| {
                *recorded.borrow_mut() = paths.paths().to_vec();
                style.visible().bg(scrim).border_2().border_color(accent)
            })
            .child(
                popover(cx)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .max_w(px(420.))
                    .px(px(16.))
                    .py(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .child(icon(IconName::Folder, 16., t.text3))
                    .child(div().truncate().child(label)),
            )
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let content = self.content.as_ref().map(|(_, view)| view.clone());
        div()
            .id("shell")
            .key_context("Shell")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg0)
            .text_color(t.text1)
            .text_size(px(TEXT_M))
            .line_height(gpui::relative(1.45))
            .font_family(FONT_UI)
            .on_action(cx.listener(|this, _: &TogglePalette, _, cx| {
                this.app.update(cx, |model, cx| model.toggle_palette(cx));
            }))
            .on_action(cx.listener(|this, _: &FindInDiff, _, cx| {
                this.app
                    .update(cx, |model, cx| model.send_intent(UiIntent::Find, cx));
            }))
            .on_action(cx.listener(|this, _: &SearchHistory, _, cx| {
                this.app.update(cx, |model, cx| model.open_search("", cx));
            }))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| {
                this.app.update(cx, |model, cx| model.refresh_snapshot(cx));
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.app.update(cx, |model, cx| model.toggle_sidebar(cx));
            }))
            .on_action(cx.listener(|this, _: &ToggleGraph, _, cx| {
                this.app
                    .update(cx, |model, cx| model.toggle_graph_panel(cx));
            }))
            .on_action(cx.listener(|this, _: &NewTab, _, cx| {
                this.app.update(cx, |model, cx| model.new_tab(cx));
            }))
            .on_action(cx.listener(|this, _: &ReopenTab, _, cx| {
                this.app.update(cx, |model, cx| model.reopen_closed_tab(cx));
            }))
            .on_action(cx.listener(|this, _: &CloseTab, _, cx| {
                this.app.update(cx, |model, cx| model.close_active_tab(cx));
            }))
            .on_action(cx.listener(|this, _: &NextTab, _, cx| {
                this.app.update(cx, |model, cx| model.cycle_tab(1, cx));
            }))
            .on_action(cx.listener(|this, _: &PrevTab, _, cx| {
                this.app.update(cx, |model, cx| model.cycle_tab(-1, cx));
            }))
            .on_action(cx.listener(|this, _: &Section1, _, cx| this.go_to(0, cx)))
            .on_action(cx.listener(|this, _: &Section2, _, cx| this.go_to(1, cx)))
            .on_action(cx.listener(|this, _: &Section3, _, cx| this.go_to(2, cx)))
            .on_action(cx.listener(|this, _: &Section4, _, cx| this.go_to(3, cx)))
            .on_action(cx.listener(|this, _: &Section5, _, cx| this.go_to(4, cx)))
            .on_action(cx.listener(|this, _: &Section6, _, cx| this.go_to(5, cx)))
            .on_action(|_: &Minimize, window, _| window.minimize_window())
            .on_action(|_: &Zoom, window, _| window.zoom_window())
            .on_action(|_: &ToggleFullScreen, window, _| window.toggle_fullscreen())
            .on_action(|_: &About, window, cx| {
                drop(window.prompt(
                    gpui::PromptLevel::Info,
                    "Jiji",
                    Some(&format!(
                        "Version {}\nA workbench for jj repositories.",
                        crate::host::update::CURRENT_VERSION
                    )),
                    &["OK"],
                    cx,
                ));
            })
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss(window, cx)))
            .on_action(|_: &FocusNext, window, cx| window.focus_next(cx))
            .on_action(|_: &FocusPrev, window, cx| window.focus_prev(cx))
            // A right-click nothing more specific answered.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.app.update(cx, |model, cx| {
                        model.open_surface_menu(event.position, "", cx)
                    });
                }),
            )
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.drop_paths.borrow_mut().clear();
                let paths = paths.paths().to_vec();
                this.app.update(cx, |model, cx| model.open_paths(paths, cx));
            }))
            .child(crate::ui::tooltip_guard())
            .child(self.render_topbar(window, cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .children(self.render_rail(cx))
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .bg(t.bg1)
                            .overflow_hidden()
                            .children(content),
                    ),
            )
            .child(self.render_statusbar(cx))
            .children(self.palette.clone())
            .children(self.search.clone())
            .children(self.menu.as_ref().map(|(_, menu)| menu.clone()))
            .child(self.render_drop_overlay(cx))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use gpui::{TestAppContext, WindowHandle};

    use super::Shell;
    use crate::app::{AppModel, ThemeMode};
    use crate::testing::press;

    fn mock_shell(
        name: &str,
        cx: &mut TestAppContext,
    ) -> (
        std::path::PathBuf,
        gpui::Entity<AppModel>,
        WindowHandle<Shell>,
    ) {
        let dir = crate::testing::scratch(name);
        let (model, repo) = crate::testing::install_mock(&dir, cx);
        let snapshot = jiji_core::mock::mock_snapshot(Path::new(&repo));
        model.update(cx, |model, cx| model.plant_repo(snapshot, cx));
        let window = cx.add_window(Shell::new);
        cx.run_until_parked();
        (dir, model, window)
    }

    #[gpui::test]
    fn settings_open_change_and_close_from_the_keyboard(cx: &mut TestAppContext) {
        let (dir, model, window) = mock_shell("keyboard-settings", cx);
        let keys = window.into();
        let focused = |cx: &mut TestAppContext| {
            window
                .update(cx, |_, window, cx| window.focused(cx))
                .unwrap()
        };

        // From the section, back through the rail's six tabs to the title
        // bar's last control: Settings, ringed, since the keyboard took it.
        press(
            keys,
            "shift-tab shift-tab shift-tab shift-tab shift-tab shift-tab",
            cx,
        );
        let rail_first = focused(cx);
        press(keys, "shift-tab", cx);
        let settings = focused(cx).expect("Settings has the keys");
        assert_ne!(Some(&settings), rail_first.as_ref());
        window
            .update(cx, |_, window, _| assert!(window.last_input_was_keyboard()))
            .unwrap();

        // Space opens it, and the panel takes the keys.
        press(keys, "space", cx);
        let open = |cx: &mut TestAppContext| {
            window
                .update(cx, |shell, window, cx| {
                    let menu = &shell.settings_menu;
                    (menu.is_open(), menu.holds_focus(window, cx))
                })
                .unwrap()
        };
        assert_eq!(open(cx), (true, true));

        // Tab enters at Appearance's first choice; the second is Light.
        press(keys, "tab tab space", cx);
        model.read_with(cx, |model, _| {
            assert_eq!(model.theme_mode(), ThemeMode::Light)
        });

        // Tab and Shift-Tab come round inside the open panel, never past it.
        for key in ["tab", "shift-tab"] {
            for _ in 0..24 {
                press(keys, key, cx);
                assert_eq!(open(cx), (true, true), "after {key}");
            }
        }

        // Escape closes it, and the keys go back to Settings.
        press(keys, "escape", cx);
        assert!(!open(cx).0);
        assert_eq!(focused(cx), Some(settings));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[gpui::test]
    fn a_click_presses_a_control_without_taking_the_keys(cx: &mut TestAppContext) {
        use gpui::{Modifiers, VisualTestContext};

        let (dir, _model, window) = mock_shell("click-keeps-focus", cx);
        let before = window
            .update(cx, |_, window, cx| window.focused(cx))
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let settings = cx
            .debug_bounds("settings-button")
            .expect("Settings is on screen");
        cx.simulate_click(settings.center(), Modifiers::none());
        let (open, after, keyboard) = window
            .update(&mut cx, |shell, window, cx| {
                (
                    shell.settings_menu.is_open(),
                    window.focused(cx),
                    window.last_input_was_keyboard(),
                )
            })
            .unwrap();
        // It opened; the panel has the keys (so Tab enters it), and with
        // the mouse in hand nothing wears the ring.
        assert!(open);
        assert_ne!(after, before);
        assert!(!keyboard);

        // Escape gives the keys back to the section, not to the button.
        cx.simulate_keystrokes("escape");
        let back = window
            .update(&mut cx, |_, window, cx| window.focused(cx))
            .unwrap();
        assert_eq!(back, before);
        let _ = std::fs::remove_dir_all(dir);
    }
}
