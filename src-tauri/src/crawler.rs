use anyhow::{Context, Result};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::PrintToPdfParams;
use futures::StreamExt;
use serde::Serialize;
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use url::Url;

#[derive(Clone)]
pub struct CrawlConfig {
    pub url: String,
    pub output_path: PathBuf,
    pub max_depth: Option<u32>,
    pub chromium_path: PathBuf,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub current_url: String,
    pub found: usize,
    pub done: usize,
}

pub async fn crawl(
    config: CrawlConfig,
    progress_tx: mpsc::UnboundedSender<Progress>,
    cancel: CancellationToken,
) -> Result<Vec<Vec<u8>>> {
    let browser_config = BrowserConfig::builder()
        .chrome_executable(&config.chromium_path)
        .arg("--headless")
        .arg("--disable-gpu")
        .arg("--no-sandbox")
        .arg("--disable-dev-shm-usage")
        .build()
        .map_err(|e| anyhow::anyhow!("BrowserConfig error: {e}"))?;

    let (mut browser, mut handler) = Browser::launch(browser_config)
        .await
        .context("Failed to launch Chromium")?;

    tokio::spawn(async move {
        while let Some(_) = handler.next().await {}
    });

    let start_url = Url::parse(&config.url).context("Invalid start URL")?;
    let prefix = build_prefix(&config.url);

    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    queue.push_back((config.url.clone(), 0));

    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(normalize_url(&config.url));

    let mut pdf_pages: Vec<Vec<u8>> = Vec::new();

    while let Some((url, depth)) = queue.pop_front() {
        if cancel.is_cancelled() {
            break;
        }

        let _ = progress_tx.send(Progress {
            current_url: url.clone(),
            found: visited.len(),
            done: pdf_pages.len(),
        });

        let page = match browser.new_page(&*url).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Failed to load {url}: {e}");
                continue;
            }
        };

        // Wait for document to be ready
        wait_for_ready(&page).await;

        // Export page to PDF
        match page.pdf(PrintToPdfParams::default()).await {
            Ok(bytes) => pdf_pages.push(bytes),
            Err(e) => eprintln!("PDF failed for {url}: {e}"),
        }

        // Extract links if we haven't hit max depth
        let should_follow = config.max_depth.map_or(true, |max| depth < max);
        if should_follow {
            let links = extract_links(&page, &start_url, &prefix).await;
            for link in links {
                let normalized = normalize_url(&link);
                if !visited.contains(&normalized) {
                    visited.insert(normalized);
                    queue.push_back((link, depth + 1));
                }
            }
        }

        let _ = page.close().await;
    }

    browser.close().await?;
    Ok(pdf_pages)
}

fn build_prefix(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    format!("{trimmed}/")
}

fn normalize_url(url: &str) -> String {
    // Strip fragment and trailing slash for deduplication
    let without_fragment = url.split('#').next().unwrap_or(url);
    without_fragment.trim_end_matches('/').to_lowercase()
}

async fn wait_for_ready(page: &chromiumoxide::Page) {
    for _ in 0..20 {
        let result = page
            .evaluate("document.readyState === 'complete'")
            .await;
        if let Ok(val) = result {
            if val.into_value::<bool>().unwrap_or(false) {
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    // Extra settle time for JS-heavy pages
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
}

async fn extract_links(
    page: &chromiumoxide::Page,
    start_url: &Url,
    prefix: &str,
) -> Vec<String> {
    let elements = match page.find_elements("a[href]").await {
        Ok(els) => els,
        Err(_) => return vec![],
    };

    let mut links = Vec::new();
    for el in &elements {
        let href = match el.attribute("href").await {
            Ok(Some(h)) if !h.is_empty() => h,
            _ => continue,
        };

        // Resolve relative URLs
        let absolute = match start_url.join(&href) {
            Ok(u) => u,
            Err(_) => continue,
        };

        let abs_str = absolute.as_str();

        // Must be http/https
        if !matches!(absolute.scheme(), "http" | "https") {
            continue;
        }

        // Must match the prefix (same domain + path prefix)
        if abs_str.starts_with(prefix) || abs_str == prefix.trim_end_matches('/') {
            // Strip fragment before adding
            let clean = match absolute.fragment() {
                Some(_) => {
                    let mut u = absolute.clone();
                    u.set_fragment(None);
                    u.to_string()
                }
                None => abs_str.to_owned(),
            };
            links.push(clean);
        }
    }
    links
}
