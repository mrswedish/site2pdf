use anyhow::{Context, Result};
use futures::StreamExt;
use serde::Deserialize;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc;

const VERSIONS_URL: &str =
    "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json";

#[cfg(target_os = "windows")]
const PLATFORM: &str = "win64";
#[cfg(target_os = "macos")]
const PLATFORM: &str = if cfg!(target_arch = "aarch64") { "mac-arm64" } else { "mac-x64" };
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const PLATFORM: &str = "linux64";

#[cfg(target_os = "windows")]
fn chrome_binary_rel() -> &'static str { "chrome-win64/chrome.exe" }
#[cfg(target_os = "macos")]
fn chrome_binary_rel() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"
    } else {
        "chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"
    }
}
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn chrome_binary_rel() -> &'static str { "chrome-linux64/chrome" }

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub downloaded_mb: f64,
    pub total_mb: f64,
    pub percent: u8,
}

pub fn chromium_dir(app: &AppHandle) -> Result<PathBuf> {
    app.path()
        .app_data_dir()
        .context("Could not resolve app data dir")
        .map(|d| d.join("chromium"))
}

pub fn chromium_binary_path(app: &AppHandle) -> Result<PathBuf> {
    Ok(chromium_dir(app)?.join(chrome_binary_rel()))
}

pub fn is_chromium_present(app: &AppHandle) -> bool {
    chromium_binary_path(app)
        .map(|p| p.exists())
        .unwrap_or(false)
}

/// Ensures Chromium is present; downloads if not.
/// Sends `DownloadProgress` events via the provided channel.
pub async fn ensure_chromium(
    app: &AppHandle,
    progress_tx: mpsc::UnboundedSender<DownloadProgress>,
) -> Result<PathBuf> {
    let binary = chromium_binary_path(app)?;
    if binary.exists() {
        return Ok(binary);
    }

    let dir = chromium_dir(app)?;
    std::fs::create_dir_all(&dir).context("Failed to create chromium dir")?;

    let (version, url) = fetch_download_url().await?;

    let bytes = download_with_progress(&url, progress_tx).await?;

    extract_zip(bytes, &dir)?;

    // Mark executable on unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if binary.exists() {
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).ok();
        }
    }

    std::fs::write(dir.join("version.txt"), &version)
        .context("Failed to write version.txt")?;

    Ok(binary)
}

// ── Internals ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct VersionsJson {
    channels: Channels,
}

#[derive(Deserialize)]
struct Channels {
    #[serde(rename = "Stable")]
    stable: Channel,
}

#[derive(Deserialize)]
struct Channel {
    version: String,
    downloads: Downloads,
}

#[derive(Deserialize)]
struct Downloads {
    chrome: Vec<PlatformAsset>,
}

#[derive(Deserialize)]
struct PlatformAsset {
    platform: String,
    url: String,
}

async fn fetch_download_url() -> Result<(String, String)> {
    let data: VersionsJson = reqwest::get(VERSIONS_URL)
        .await
        .context("Failed to fetch Chrome for Testing versions")?
        .json()
        .await
        .context("Failed to parse versions JSON")?;

    let version = data.channels.stable.version.clone();
    let url = data
        .channels
        .stable
        .downloads
        .chrome
        .into_iter()
        .find(|a| a.platform == PLATFORM)
        .map(|a| a.url)
        .context(format!("No Chrome for Testing asset for platform {PLATFORM}"))?;

    Ok((version, url))
}

async fn download_with_progress(
    url: &str,
    tx: mpsc::UnboundedSender<DownloadProgress>,
) -> Result<Vec<u8>> {
    let resp = reqwest::get(url)
        .await
        .context("HTTP request failed")?;

    let total_bytes = resp.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut buf: Vec<u8> = Vec::with_capacity(total_bytes as usize);

    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("Stream error")?;
        downloaded += chunk.len() as u64;
        buf.extend_from_slice(&chunk);

        let percent = if total_bytes > 0 {
            ((downloaded * 100) / total_bytes) as u8
        } else {
            0
        };
        let _ = tx.send(DownloadProgress {
            downloaded_mb: downloaded as f64 / 1_048_576.0,
            total_mb: total_bytes as f64 / 1_048_576.0,
            percent,
        });
    }

    Ok(buf)
}

fn extract_zip(bytes: Vec<u8>, dest: &PathBuf) -> Result<()> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).context("Failed to open zip")?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).context("Zip entry error")?;
        let target = dest.join(file.name());

        if file.is_dir() {
            std::fs::create_dir_all(&target).ok();
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            let mut out = std::fs::File::create(&target).context("Failed to create file")?;
            std::io::copy(&mut file, &mut out).context("Failed to write file")?;
        }
    }

    Ok(())
}
