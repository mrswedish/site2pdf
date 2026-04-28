use crate::chromium_manager::{self, DownloadProgress};
use crate::crawler::{crawl, CrawlConfig, Progress};
use crate::pdf::merge_pdfs;
use chromiumoxide::browser::{Browser, BrowserConfig};
use futures::StreamExt;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, Window};
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct CrawlState(Arc<Mutex<Option<CancellationToken>>>);

struct PreviewSession {
    browser: Browser,
    profile_dir: PathBuf,
}

#[derive(Default)]
pub struct PreviewState(Arc<Mutex<Option<PreviewSession>>>);

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

#[tauri::command]
pub async fn read_url_file(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let path = app
        .dialog()
        .file()
        .add_filter("Textfil", &["txt"])
        .blocking_pick_file();
    match path {
        None => Ok(None),
        Some(fp) => {
            let p = fp.into_path().map_err(|e| e.to_string())?;
            let content = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
            Ok(Some(content))
        }
    }
}

// ── Preview browser ───────────────────────────────────────────────────────────

#[tauri::command]
pub async fn open_preview_browser(
    app: AppHandle,
    state: tauri::State<'_, PreviewState>,
    url: String,
) -> Result<(), String> {
    let chromium = chromium_manager::chromium_binary_path(&app).map_err(|e| e.to_string())?;
    if !chromium.exists() {
        return Err("Chromium är inte installerat. Ladda ned det först.".into());
    }

    let profile_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("preview_profile");

    // Remove stale profile from a previous session
    if profile_dir.exists() {
        std::fs::remove_dir_all(&profile_dir).ok();
    }
    std::fs::create_dir_all(&profile_dir).map_err(|e| e.to_string())?;

    let mut preview_builder = BrowserConfig::builder()
        .chrome_executable(&chromium)
        .arg("--no-sandbox")
        .arg("--disable-gpu")
        .arg("--disable-dev-shm-usage")
        .arg(format!("--user-data-dir={}", profile_dir.display()));

    #[cfg(target_os = "macos")]
    { preview_builder = preview_builder.arg("--single-process"); }

    let browser_config = preview_builder
        .build()
        .map_err(|e| format!("BrowserConfig error: {e}"))?;

    let (browser, mut handler) = Browser::launch(browser_config)
        .await
        .map_err(|e| format!("Failed to launch browser: {e}"))?;

    tokio::spawn(async move {
        while let Some(_) = handler.next().await {}
    });

    browser
        .new_page(&*url)
        .await
        .map_err(|e| format!("Failed to open page: {e}"))?;

    *state.0.lock().await = Some(PreviewSession { browser, profile_dir });

    Ok(())
}

#[tauri::command]
pub async fn close_preview_browser(
    state: tauri::State<'_, PreviewState>,
) -> Result<(), String> {
    if let Some(session) = state.0.lock().await.take() {
        drop(session.browser);
        std::fs::remove_dir_all(&session.profile_dir).ok();
    }
    Ok(())
}

// ── Crawl ─────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn start_crawl(
    app: AppHandle,
    window: Window,
    state: tauri::State<'_, CrawlState>,
    preview_state: tauri::State<'_, PreviewState>,
    url: String,
    output_path: String,
    max_depth: Option<u32>,
    blocked_patterns: Vec<String>,
    url_list: Option<Vec<String>>,
) -> Result<(), String> {
    let chromium = chromium_manager::chromium_binary_path(&app).map_err(|e| e.to_string())?;
    if !chromium.exists() {
        return Err("Chromium är inte installerat. Ladda ned det först.".into());
    }

    // Close the preview browser and take the profile dir for cookie persistence
    let user_data_dir = {
        let mut guard = preview_state.0.lock().await;
        guard.take().map(|s| {
            let dir = s.profile_dir.clone();
            drop(s.browser);
            dir
        })
    };

    let config = CrawlConfig {
        url,
        output_path: PathBuf::from(&output_path),
        max_depth,
        chromium_path: chromium,
        blocked_patterns,
        user_data_dir,
        url_list,
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
        // Give Chrome time to flush cookies to the profile dir
        if config.user_data_dir.is_some() {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        }

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
