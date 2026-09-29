// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args_os().nth(1).is_some() {
        std::process::exit(jiji_cli::run(std::env::args_os()));
    }
    jiji_lib::run()
}
