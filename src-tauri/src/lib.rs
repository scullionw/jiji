mod autoland;
mod commands;
mod forge;

use autoland::AutoLandHost;
use commands::AppState;
use forge::ForgeState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
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
        .manage(AppState::new())
        .manage(ForgeState::new())
        .manage(AutoLandHost::new())
        .setup(|app| {
            // A job record that survived the last session loads before any
            // surface asks, so the shell can offer "interrupted — resume?".
            use tauri::Manager as _;
            app.state::<AutoLandHost>().load_persisted(app);
            Ok(())
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
            commands::refresh_snapshot,
            commands::current_snapshot,
            commands::change_detail,
            commands::change_diff,
            commands::compare_diff,
            commands::describe_change,
            commands::new_change,
            commands::edit_change,
            commands::abandon_change,
            commands::squash_change,
            commands::split_change,
            commands::squash_into,
            commands::rebase_change,
            commands::move_change,
            commands::create_bookmark,
            commands::move_bookmark,
            commands::rename_bookmark,
            commands::delete_bookmark,
            commands::revert_operation,
            commands::restore_operation,
            commands::resolve_conflict,
            commands::update_stale_workspace,
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
            forge::land_stack,
            forge::ship_plan,
            forge::ship_stack,
            autoland::autoland_start,
            autoland::autoland_stop,
            autoland::autoland_state,
            autoland::autoland_dismiss
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
