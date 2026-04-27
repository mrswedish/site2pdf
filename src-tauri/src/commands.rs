use crate::chromium_manager::{self, DownloadProgress};
use crate::crawler::{crawl, CrawlConfig, Progress};
use crate::pdf::merge_pdfs;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Window};
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct CrawlState(Arc<Mutex<Option<CancellationToken>>>);

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CompleteInfo {
    pub total: usize,
    pub output_path: String,
    pub file_size: u64,
}

// ── Chromium management ───────────────────────────────────────────────────────

#[tauri::command]
pub fn chromium_ready(app: AppHandle) -> bool {
    chromium_manager::is_chromium_present(&app)
}

#[tauri::command]
pub async fn download_chromium(app: AppHandle, window: Window) -> Result<(), String> {
    let (tx, mut rx) = mpsc::unbounded_channel::<DownloadProgress>();

    let win = window.clone();
    tokio::spawn(async move {
        while let Some(p) = rx.recv().await {
            let _ = win.emit("chromium-download-progress", &p);
        }
    });

    chromium_manager::ensure_chromium(&app, tx)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ── File dialog / open ────────────────────────────────────────────────────────

#[tauri::command]
pub async fn choose_save_path(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let path = app
        .dialog()
        .file()
        .add_filter("PDF", &["pdf"])
        .blocking_save_file();
    Ok(path
        .and_then(|fp| fp.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned()))
}

#[tauri::command]
pub fn open_file(path: String) -> Result<(), String> {
    opener::open(&path).map_err(|e| e.to_string())
}

// ── Crawl ─────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn start_crawl(
    app: AppHandle,
    window: Window,
    state: tauri::State<'_, CrawlState>,
    url: String,
    output_path: String,
    max_depth: Option<u32>,
) -> Result<(), String> {
    let chromium = chromium_manager::chromium_binary_path(&app).map_err(|e| e.to_string())?;
    if !chromium.exists() {
        return Err("Chromium är inte installerat. Ladda ned det först.".into());
    }

    let config = CrawlConfig {
        url,
        output_path: PathBuf::from(&output_path),
        max_depth,
        chromium_path: chromium,
    };

    let token = CancellationToken::new();
    *state.0.lock().await = Some(token.clone());

    let (tx, mut rx) = mpsc::unbounded_channel::<Progress>();

    let win_clone = window.clone();
    tokio::spawn(async move {
        while let Some(p) = rx.recv().await {
            let _ = win_clone.emit("crawl-progress", &p);
        }
    });

    let win = window.clone();
    tokio::spawn(async move {
        match crawl(config.clone(), tx, token).await {
            Ok(pdf_pages) => {
                let total = pdf_pages.len();
                match merge_pdfs(pdf_pages) {
                    Ok(merged) => match std::fs::write(&config.output_path, &merged) {
                        Ok(_) => {
                            let size = std::fs::metadata(&config.output_path)
                                .map(|m| m.len())
                                .unwrap_or(0);
                            let _ = win.emit(
                                "crawl-complete",
                                CompleteInfo {
                                    total,
                                    output_path: config.output_path.to_string_lossy().into_owned(),
                                    file_size: size,
                                },
                            );
                        }
                        Err(e) => {
                            let _ = win.emit("crawl-error", format!("Kunde inte spara PDF: {e}"));
                        }
                    },
                    Err(e) => {
                        let _ = win
                            .emit("crawl-error", format!("PDF-sammanslagning misslyckades: {e}"));
                    }
                }
            }
            Err(e) => {
                let _ = win.emit("crawl-error", format!("Krypning misslyckades: {e}"));
            }
        }
    });

    Ok(())
}

#[tauri::command]
pub async fn cancel_crawl(state: tauri::State<'_, CrawlState>) -> Result<(), String> {
    if let Some(token) = state.0.lock().await.take() {
        token.cancel();
    }
    Ok(())
}
