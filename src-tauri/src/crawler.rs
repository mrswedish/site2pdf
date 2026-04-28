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
    /// Glob-style patterns (using `*` as wildcard) — matching URLs are skipped.
    pub blocked_patterns: Vec<String>,
    /// If set, Chromium is launched with this user-data-dir to inherit cookies
    /// from a prior headed session (manual cookie banner handling).
    pub user_data_dir: Option<PathBuf>,
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
    let mut browser_builder = BrowserConfig::builder()
        .chrome_executable(&config.chromium_path)
        .arg("--headless")
        .arg("--disable-gpu")
        .arg("--no-sandbox")
        .arg("--disable-dev-shm-usage");

    // macOS 26+ blocks Chrome's internal fork() calls from a multi-threaded
    // process. --single-process eliminates all forking by running renderer,
    // GPU and network in the same OS process.
    #[cfg(target_os = "macos")]
    { browser_builder = browser_builder.arg("--single-process"); }

    if let Some(dir) = &config.user_data_dir {
        browser_builder = browser_builder.arg(format!("--user-data-dir={}", dir.display()));
    }

    let browser_config = browser_builder
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

        // Try up to 2 times: once normally, once after a longer back-off
        let mut pdf_bytes: Option<Vec<u8>> = None;
        let mut page_links: Vec<String> = Vec::new();
        let mut attempt = 0u8;

        'retry: loop {
            attempt += 1;

            // Open page with a 30-second timeout
            let page = match tokio::time::timeout(
                std::time::Duration::from_secs(30),
                browser.new_page(&*url),
            )
            .await
            {
                Ok(Ok(p)) => p,
                Ok(Err(e)) => {
                    eprintln!("Failed to open {url} (attempt {attempt}): {e}");
                    if attempt < 2 {
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        continue 'retry;
                    }
                    break 'retry;
                }
                Err(_) => {
                    eprintln!("Timeout opening {url} (attempt {attempt})");
                    if attempt < 2 {
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        continue 'retry;
                    }
                    break 'retry;
                }
            };

            // Wait for document to be ready
            wait_for_ready(&page).await;

            // Dismiss cookie consent banners before generating PDF
            dismiss_cookie_banners(&page).await;

            // Export page to PDF (30-second timeout)
            match tokio::time::timeout(
                std::time::Duration::from_secs(30),
                page.pdf(PrintToPdfParams::default()),
            )
            .await
            {
                Ok(Ok(bytes)) => pdf_bytes = Some(bytes),
                Ok(Err(e)) => {
                    eprintln!("PDF failed for {url} (attempt {attempt}): {e}");
                    if attempt < 2 {
                        let _ = page.close().await;
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        continue 'retry;
                    }
                }
                Err(_) => {
                    eprintln!("PDF timeout for {url} (attempt {attempt})");
                    if attempt < 2 {
                        let _ = page.close().await;
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        continue 'retry;
                    }
                }
            }

            // Extract links if we haven't hit max depth
            let should_follow = config.max_depth.map_or(true, |max| depth < max);
            if should_follow {
                page_links = extract_links(&page, &start_url, &prefix).await;
            }

            let _ = page.close().await;
            break 'retry;
        }

        if let Some(bytes) = pdf_bytes {
            pdf_pages.push(bytes);
        }

        for link in page_links {
            let normalized = normalize_url(&link);
            if !visited.contains(&normalized) && !is_blocked(&link, &config.blocked_patterns) {
                visited.insert(normalized);
                queue.push_back((link, depth + 1));
            }
        }

        // Paus mellan sidor — minskar risken för rate limiting
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    browser.close().await?;

    if let Some(dir) = &config.user_data_dir {
        std::fs::remove_dir_all(dir).ok();
    }

    Ok(pdf_pages)
}

/// Returns true if `url` matches any of the glob patterns (only `*` wildcard supported).
fn is_blocked(url: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pat| glob_match(url, pat))
}

fn glob_match(url: &str, pattern: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut remaining = url;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        match remaining.find(part) {
            Some(pos) if i == 0 && pos != 0 => return false, // first part must anchor to start
            Some(pos) => remaining = &remaining[pos + part.len()..],
            None => return false,
        }
    }
    // If pattern ends without *, remaining must be empty
    if !pattern.ends_with('*') && !remaining.is_empty() {
        return false;
    }
    true
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

async fn dismiss_cookie_banners(page: &chromiumoxide::Page) {
    // Step 1: try to click accept buttons.
    // Tries known Cookiebot/OneTrust IDs first, then falls back to text matching.
    let click_script = r#"
        (function() {
            // --- Cookiebot (used by e.g. folkhalsomyndigheten.se) ---
            const cybotAccept = document.getElementById(
                'CybotCookiebotDialogBodyLevelButtonLevelOptinAllowAll'
            );
            if (cybotAccept) { cybotAccept.click(); return 'cybot-id'; }

            // --- OneTrust ---
            const otAccept = document.getElementById('onetrust-accept-btn-handler');
            if (otAccept) { otAccept.click(); return 'onetrust-id'; }

            // --- Generic Cookiebot JS API ---
            if (typeof Cookiebot !== 'undefined' && Cookiebot.hide) {
                Cookiebot.hide(); return 'cookiebot-api';
            }

            // --- Text-based fallback ---
            const keywords = [
                'acceptera alla kakor', 'acceptera alla', 'acceptera', 'accept all cookies',
                'accept all', 'accept cookies', 'accept', 'godkänn alla', 'godkänn',
                'tillåt alla', 'tillåt', 'allow all', 'allow cookies', 'allow',
                'agree', 'i agree', 'ok', 'got it', 'förstår', 'jag förstår',
                'fortsätt', 'continue', 'bekräfta', 'confirm'
            ];
            const candidates = document.querySelectorAll(
                'button, [role="button"], input[type="button"], input[type="submit"], a.btn, a.button'
            );
            for (const el of candidates) {
                const text = (el.textContent || el.value || el.getAttribute('aria-label') || '')
                    .trim().toLowerCase();
                if (keywords.some(k => text === k || text.startsWith(k))) {
                    el.click();
                    return 'text-match:' + text;
                }
            }
            return false;
        })()
    "#;
    let _ = page.evaluate(click_script).await;

    // Brief pause so JS can process the click and remove the overlay.
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;

    // Step 2: hide any remaining banners via CSS — covers OneTrust, Cookiebot,
    // Cookie Notice, GDPR Cookie Compliance and generic attribute patterns.
    let hide_script = r#"
        (function() {
            const css = `
                /* OneTrust */
                #onetrust-banner-sdk, #onetrust-consent-sdk,
                .onetrust-pc-dark-filter,
                /* Cookiebot (generic + folkhalsomyndigheten.se variant) */
                #CybotCookiebotDialog, #CybotCookiebotDialogBody,
                .CybotCookiebotFader,
                #CookieBanner, #CookieBannerNotice, #CookieBannerDetails,
                .is-visible-cookie-banner,
                /* Cookie Notice / WP plugins */
                #cookie-notice, .cookie-notice, #cookie-law-info-bar,
                .cookie-law-info-bar, #cookie-popup, .cookie-popup,
                /* GDPR Cookie Compliance */
                .moove-gdpr-info-bar, .moove-gdpr-infobar-allow-all,
                /* Usercentrics */
                #usercentrics-root,
                /* Generic attribute selectors */
                [id*="cookie-banner"], [id*="cookiebanner"],
                [id*="cookie-consent"], [id*="cookieconsent"],
                [id*="cookie-notice"], [id*="gdpr-banner"],
                [class*="cookie-banner"], [class*="cookiebanner"],
                [class*="cookie-consent"], [class*="cookieconsent"],
                [class*="cookie-notice"], [class*="gdpr-banner"],
                [class*="cookie-overlay"], [class*="consent-overlay"],
                /* Overlay/backdrop */
                [class*="cookie-modal"], [id*="cookie-modal"]
                {
                    display: none !important;
                    visibility: hidden !important;
                    opacity: 0 !important;
                }
                /* Re-enable scroll if banner had locked the body */
                body { overflow: auto !important; }
            `;
            const style = document.createElement('style');
            style.id = '__s2pdf_nocookie__';
            if (!document.getElementById('__s2pdf_nocookie__')) {
                style.textContent = css;
                document.head.appendChild(style);
            }
        })()
    "#;
    let _ = page.evaluate(hide_script).await;

    // Let any CSS transitions finish before printing.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
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
