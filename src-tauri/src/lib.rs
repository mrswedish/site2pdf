mod chromium_manager;
mod commands;
mod crawler;
mod pdf;

use commands::CrawlState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::default().build())
        .plugin(tauri_plugin_dialog::init())
        .manage(CrawlState::default())
        .invoke_handler(tauri::generate_handler![
            commands::chromium_ready,
            commands::download_chromium,
            commands::choose_save_path,
            commands::open_file,
            commands::start_crawl,
            commands::cancel_crawl,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
