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
            commands::choose_save_path,
            commands::start_crawl,
            commands::cancel_crawl,
            commands::open_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
