mod commands;
mod forge;

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
            forge::forge_status,
            forge::forge_verify,
            forge::forge_login,
            forge::forge_logout,
            forge::forge_prs,
            forge::submit_plan,
            forge::submit_stack
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
