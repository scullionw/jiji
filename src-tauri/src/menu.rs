// The native macOS menu bar. Menu items mirror the shell's existing
// actions; each one emits its id to the webview (`menu://action`), where
// AppShell dispatches to the same functions the in-app shortcuts call.
// The webview keeps first crack at key equivalents (its handlers
// preventDefault, which stops the re-dispatch to the menu), so the
// accelerators shown here never double-fire — the menu handles them only
// when focus is outside the web content, or on a mouse click.

use tauri::menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Emitter as _, Runtime};

/// The event AppShell listens on; the payload is the menu item id.
pub const MENU_EVENT: &str = "menu://action";

const REPO_URL: &str = "https://github.com/scullionw/jiji";

pub fn install<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let app_menu = Submenu::with_items(
        app,
        "Jiji",
        true,
        &[
            // Explicit texts: the defaults use the crate name ("jiji"),
            // not the product name.
            &PredefinedMenuItem::about(app, Some("About Jiji"), Some(AboutMetadata::default()))?,
            &MenuItem::with_id(
                app,
                "check-updates",
                "Check for Updates…",
                true,
                None::<&str>,
            )?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, Some("Hide Jiji"))?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, Some("Quit Jiji"))?,
        ],
    )?;

    let file_menu = Submenu::with_items(
        app,
        "File",
        true,
        &[
            &MenuItem::with_id(app, "new-tab", "New Tab", true, Some("Cmd+T"))?,
            &MenuItem::with_id(app, "open-repo", "Open Repository…", true, Some("Cmd+O"))?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "close-tab", "Close Tab", true, Some("Cmd+W"))?,
            &MenuItem::with_id(
                app,
                "reopen-tab",
                "Reopen Closed Tab",
                true,
                Some("Cmd+Shift+T"),
            )?,
        ],
    )?;

    // Standard editing items so text fields keep ⌘X/⌘C/⌘V/⌘A — the
    // default menu provided these; a custom menu must carry them forward.
    let edit_menu = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "find", "Find in Diff", true, Some("Cmd+F"))?,
            &MenuItem::with_id(
                app,
                "search",
                "Search History…",
                true,
                Some("Cmd+Shift+F"),
            )?,
        ],
    )?;

    // One item per sidebar section, numbered like the ⌘1–⌘6 shortcuts.
    let view_menu = Submenu::with_items(
        app,
        "View",
        true,
        &[
            &MenuItem::with_id(app, "palette", "Command Palette…", true, Some("Cmd+K"))?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "section-0", "Workbench", true, Some("Cmd+1"))?,
            &MenuItem::with_id(app, "section-1", "Conflicts", true, Some("Cmd+2"))?,
            &MenuItem::with_id(app, "section-2", "Publish", true, Some("Cmd+3"))?,
            &MenuItem::with_id(app, "section-3", "Bookmarks", true, Some("Cmd+4"))?,
            &MenuItem::with_id(app, "section-4", "Operations", true, Some("Cmd+5"))?,
            &MenuItem::with_id(app, "section-5", "Workspaces", true, Some("Cmd+6"))?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "sidebar", "Toggle Sidebar", true, Some("Cmd+B"))?,
            &MenuItem::with_id(
                app,
                "graph",
                "Toggle Graph Panel",
                true,
                Some("Cmd+Shift+B"),
            )?,
            &MenuItem::with_id(app, "refresh", "Refresh", true, Some("Cmd+R"))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::fullscreen(app, None)?,
        ],
    )?;

    let window_menu = Submenu::with_items(
        app,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::maximize(app, Some("Zoom"))?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(
                app,
                "prev-tab",
                "Show Previous Tab",
                true,
                Some("Cmd+Shift+["),
            )?,
            &MenuItem::with_id(app, "next-tab", "Show Next Tab", true, Some("Cmd+Shift+]"))?,
        ],
    )?;

    let help_menu = Submenu::with_items(
        app,
        "Help",
        true,
        &[&MenuItem::with_id(
            app,
            "help",
            "Jiji Help",
            true,
            None::<&str>,
        )?],
    )?;

    let menu = Menu::with_items(
        app,
        &[
            &app_menu,
            &file_menu,
            &edit_menu,
            &view_menu,
            &window_menu,
            &help_menu,
        ],
    )?;
    app.set_menu(menu)?;
    Ok(())
}

/// Route a chosen menu item: Help opens the repo page right here; every
/// other id crosses to the webview, which owns the corresponding action.
pub fn handle<R: Runtime>(app: &AppHandle<R>, id: &str) {
    if id == "help" {
        if let Err(error) = tauri_plugin_opener::open_url(REPO_URL, None::<&str>) {
            log::warn!("failed to open help url: {error}");
        }
        return;
    }
    if let Err(error) = app.emit(MENU_EVENT, id) {
        log::warn!("failed to forward menu action {id}: {error}");
    }
}
