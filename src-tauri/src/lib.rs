mod autoland;
mod cli;
mod commands;
mod forge;
mod integrations;
#[cfg(target_os = "macos")]
mod menu;

use autoland::AutoLandHost;
use commands::AppState;
use forge::ForgeState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_log::Builder::default()
                .level(log::LevelFilter::Info)
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        // Window geometry survives restarts. VISIBLE is excluded so the
        // window always shows on launch, whatever state it closed in.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::all()
                        - tauri_plugin_window_state::StateFlags::VISIBLE,
                )
                .build(),
        )
        .manage(AppState::new())
        .manage(ForgeState::new())
        .manage(AutoLandHost::new())
        .manage(cli::PendingOpen::default())
        .setup(|app| {
            // A job record that survived the last session loads before any
            // surface asks, so the shell can offer "interrupted — resume?".
            use tauri::Manager as _;
            app.state::<AutoLandHost>().load_persisted(app);
            #[cfg(target_os = "macos")]
            menu::install(app.handle())?;
            Ok(())
        })
        .on_menu_event(|app, event| {
            #[cfg(target_os = "macos")]
            menu::handle(app, event.id().as_ref());
            #[cfg(not(target_os = "macos"))]
            let _ = (app, event);
        })
        .on_window_event(|window, event| {
            // Coming back to the app is the moment stale job state hurts:
            // nudge a watching auto-land job to poll now rather than
            // finish dozing through its interval.
            if matches!(event, tauri::WindowEvent::Focused(true)) {
                use tauri::Manager as _;
                window.state::<AutoLandHost>().poke_active();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::open_repo,
            commands::init_jj_repo,
            commands::close_repo,
            commands::refresh_snapshot,
            commands::set_snapshot_view,
            commands::current_snapshot,
            commands::change_detail,
            commands::change_diff,
            commands::compare_diff,
            commands::change_evolog,
            commands::file_annotate,
            commands::search_history,
            commands::describe_change,
            commands::new_change,
            commands::edit_change,
            commands::abandon_change,
            commands::squash_change,
            commands::restore_file,
            commands::restore_change_version,
            commands::duplicate_change,
            commands::revert_change,
            commands::absorb_change,
            commands::split_change,
            commands::squash_into,
            commands::rebase_change,
            commands::move_change,
            commands::create_bookmark,
            commands::move_bookmark,
            commands::rename_bookmark,
            commands::delete_bookmark,
            commands::track_bookmark,
            commands::untrack_bookmark,
            commands::push_bookmarks,
            commands::delete_remote_bookmark,
            commands::revert_operation,
            commands::restore_operation,
            commands::resolve_conflict,
            commands::resolve_conflict_take,
            commands::update_stale_workspace,
            commands::set_background_color,
            commands::git_fetch,
            commands::fetch_pr,
            forge::forge_status,
            forge::forge_verify,
            forge::forge_login,
            forge::forge_logout,
            forge::forge_prs,
            forge::forge_pr,
            forge::rerun_failed_ci,
            forge::submit_plan,
            forge::submit_stack,
            forge::land_plan,
            forge::publish_snapshot,
            forge::publish_stacks,
            forge::land_stack,
            forge::ship_plan,
            forge::ship_stack,
            autoland::autoland_start,
            autoland::autoland_stop,
            autoland::autoland_state,
            autoland::autoland_dismiss,
            cli::take_pending_open,
            cli::cli_status,
            cli::install_cli,
            cli::uninstall_cli,
            integrations::detect_editors,
            integrations::open_in_editor,
            integrations::detect_terminals,
            integrations::open_in_terminal
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // Folders arriving through Launch Services — the `jiji` CLI
            // launcher's `open -b`, or a drop on the Dock icon.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Opened { urls } = event {
                cli::handle_opened(app, urls);
            }
            #[cfg(not(target_os = "macos"))]
            let _ = (app, &event);
        });
}
