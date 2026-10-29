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
    /// If set, crawl exactly these URLs in order — no BFS link discovery.
    pub url_list: Option<Vec<String>>,
    /// Discover subpages via sitemap.xml (under the start URL's path) instead of
    /// following links. Falls back to link-following if no sitemap is usable.
    pub use_sitemap: bool,
    /// Strip menus, sidebars, filters etc. and keep only the main content.
    pub main_only: bool,
    /// Glob-style patterns (`*` wildcard, case-insensitive). Same-host links in each
    /// page's main content matching any of these are fetched as document pages.
    /// Empty = off. Documents are never crawled further.
    pub document_patterns: Vec<String>,
}

/// Depth marker for document pages fetched via `document_patterns`.
const DOC_DEPTH: u32 = u32::MAX - 1;

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
    let mut config = config;
    if config.use_sitemap && config.url_list.is_none() {
        match crate::sitemap::resolve(&config.url).await {
            Ok(list) if !list.is_empty() => {
                let list: Vec<String> = list
                    .into_iter()
                    .filter(|u| !is_blocked(u, &config.blocked_patterns))
                    .collect();
                eprintln!("Sitemap: {} pages under {}", list.len(), config.url);
                config.url_list = Some(list);
            }
            Ok(_) => eprintln!("Sitemap: nothing usable, falling back to link-following"),
            Err(e) => eprintln!("Sitemap failed ({e}), falling back to link-following"),
        }
    }

    let mut browser = launch_browser(&config.chromium_path, config.user_data_dir.as_deref()).await?;

    let (start_url, prefix) = if config.url_list.is_none() {
        let su = Url::parse(&config.url).context("Invalid start URL")?;
        let pf = build_prefix(&config.url);
        (su, pf)
    } else {
        (Url::parse("http://localhost").unwrap(), String::new())
    };

    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    let mut visited: HashSet<String> = HashSet::new();

    if let Some(list) = &config.url_list {
        for url in list {
            let norm = normalize_url(url);
            if visited.insert(norm) {
                queue.push_back((url.clone(), u32::MAX));
            }
        }
    } else {
        queue.push_back((config.url.clone(), 0));
        visited.insert(normalize_url(&config.url));
    }

    let mut pdf_pages: Vec<Vec<u8>> = Vec::new();

    // A single persistent page navigated to each URL in turn.
    // This avoids Target.createTarget (new tab) which hangs under --single-process on macOS.
    let page = match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        browser.new_page("about:blank"),
    )
    .await
    {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => return Err(anyhow::anyhow!("Failed to create initial page: {e}")),
        Err(_) => return Err(anyhow::anyhow!("Timeout creating initial page")),
    };

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
        let mut doc_links: Vec<String> = Vec::new();
        let mut attempt = 0u8;

        'retry: loop {
            attempt += 1;

            // Navigate the persistent page to the target URL
            match tokio::time::timeout(
                std::time::Duration::from_secs(30),
                page.goto(url.as_str()),
            )
            .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    eprintln!("Failed to navigate to {url} (attempt {attempt}): {e}");
                    if attempt < 2 {
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        continue 'retry;
                    }
                    break 'retry;
                }
                Err(_) => {
                    eprintln!("Timeout navigating to {url} (attempt {attempt})");
                    if attempt < 2 {
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        continue 'retry;
                    }
                    break 'retry;
                }
            }

            // Wait for document to be ready
            wait_for_ready(&page).await;

            // Dismiss cookie consent banners before generating PDF
            dismiss_cookie_banners(&page).await;

            // Expand collapsed sections and (optionally) keep only the main content
            expand_collapsed(&page).await;
            if config.main_only {
                isolate_main_content(&page).await;
            }

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
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        continue 'retry;
                    }
                }
                Err(_) => {
                    eprintln!("PDF timeout for {url} (attempt {attempt})");
                    if attempt < 2 {
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        continue 'retry;
                    }
                }
            }

            // Extract links if we haven't hit max depth (never in url_list mode)
            let should_follow = config.url_list.is_none()
                && depth != DOC_DEPTH
                && config.max_depth.map_or(true, |max| depth < max);
            if should_follow {
                page_links = extract_links(&page, &start_url, &prefix).await;
            }
            if !config.document_patterns.is_empty() && depth != DOC_DEPTH {
                doc_links = extract_document_links(&page, &config.document_patterns).await;
            }

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

        for link in doc_links {
            // Same document is linked with varying query strings (pageId/blockId):
            // dedupe on host + path only.
            let key = normalize_url(link.split('?').next().unwrap_or(&link));
            if !is_blocked(&link, &config.blocked_patterns) && visited.insert(key) {
                queue.push_back((link, DOC_DEPTH));
            }
        }

        // Paus mellan sidor — minskar risken för rate limiting
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    let _ = page.close().await;
    browser.close().await?;

    if let Some(dir) = &config.user_data_dir {
        std::fs::remove_dir_all(dir).ok();
    }

    Ok(pdf_pages)
}

/// Launch headless Chromium and drive its event handler in the background.
async fn launch_browser(
    chromium_path: &std::path::Path,
    user_data_dir: Option<&std::path::Path>,
) -> Result<Browser> {
    let mut browser_builder = BrowserConfig::builder()
        .chrome_executable(chromium_path)
        .arg("--headless")
        .arg("--disable-gpu")
        .arg("--no-sandbox")
        .arg("--disable-dev-shm-usage");

    // macOS 26+ blocks Chrome's internal fork() calls from a multi-threaded
    // process. --single-process eliminates all forking by running renderer,
    // GPU and network in the same OS process.
    #[cfg(target_os = "macos")]
    {
        // Escape hatch: a regular installed Chrome can hang on page creation with this flag.
        if std::env::var_os("S2P_NO_SINGLE").is_none() {
            browser_builder = browser_builder.arg("--single-process");
        }
    }

    if let Some(dir) = user_data_dir {
        browser_builder = browser_builder.arg(format!("--user-data-dir={}", dir.display()));
    }

    let browser_config = browser_builder
        .build()
        .map_err(|e| anyhow::anyhow!("BrowserConfig error: {e}"))?;

    let (browser, mut handler) = Browser::launch(browser_config)
        .await
        .context("Failed to launch Chromium")?;

    tokio::spawn(async move {
        while let Some(_) = handler.next().await {}
    });

    Ok(browser)
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
    // Consent platforms (Cookie Information, Cookiebot, ...) inject their banner
    // asynchronously after load, so wait briefly for it before trying to accept.
    for _ in 0..12 {
        let present = page
            .evaluate(
                r#"(function() {
                    return !!(window.CookieInformation || window.Cookiebot || window.OneTrust
                        || window.UC_UI || window.klaro
                        || document.getElementById('coiOverlay')
                        || document.getElementById('CybotCookiebotDialog')
                        || document.getElementById('onetrust-banner-sdk')
                        || document.getElementById('usercentrics-root'));
                })()"#,
            )
            .await
            .ok()
            .and_then(|v| v.into_value::<bool>().ok())
            .unwrap_or(false);
        if present {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    // Step 1: accept via the platform's JS API or accept buttons.
    // Tries known platform APIs/IDs first, then falls back to text matching.
    let click_script = r#"
        (function() {
            // --- Cookie Information (used by e.g. rjl.se) ---
            if (window.CookieInformation && typeof CookieInformation.submitAllCategories === 'function') {
                try { CookieInformation.submitAllCategories(); return 'cookieinformation-api'; } catch (e) {}
            }

            // --- Cookiebot (used by e.g. folkhalsomyndigheten.se) ---
            const cybotAccept = document.getElementById(
                'CybotCookiebotDialogBodyLevelButtonLevelOptinAllowAll'
            );
            if (cybotAccept) { cybotAccept.click(); return 'cybot-id'; }

            // --- OneTrust ---
            const otAccept = document.getElementById('onetrust-accept-btn-handler');
            if (otAccept) { otAccept.click(); return 'onetrust-id'; }

            // --- Usercentrics ---
            if (window.UC_UI && typeof UC_UI.acceptAllConsents === 'function') {
                try { UC_UI.acceptAllConsents(); return 'usercentrics-api'; } catch (e) {}
            }

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
                /* Cookie Information (rjl.se etc.) */
                #coiOverlay, #cookie-information-template-wrapper,
                #coi-banner-wrapper, .coi-banner__wrapper,
                [id^="coi-"], [class^="coi-banner"],
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

/// Open accordions / "details" so their content ends up in the PDF.
async fn expand_collapsed(page: &chromiumoxide::Page) {
    let script = r#"
        (async function() {
            document.querySelectorAll('details:not([open])').forEach(d => d.setAttribute('open', ''));
            const heads = document.querySelectorAll(
                '[aria-expanded="false"]:not(nav *):not(aside *):not([aria-haspopup])'
            );
            heads.forEach(h => { try { h.click(); } catch (e) {} });
            return heads.length;
        })()
    "#;
    if let Ok(v) = page.evaluate(script).await {
        if v.into_value::<i64>().unwrap_or(0) > 0 {
            // Accordion content is sometimes rendered/loaded lazily after the click.
            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        }
    }
}

/// Hide everything except the page's main content element (`<main>`, `[role=main]`,
/// `#main`, `#content` or `<article>`), and drop interactive chrome inside it
/// (sidebars, navigation, filters, buttons). No-op if no main element is found.
async fn isolate_main_content(page: &chromiumoxide::Page) {
    let script = r#"
        (function() {
            const main = document.querySelector('main, [role="main"], #main, #content, article');
            if (!main) return false;
            const css = [];
            // Hide all siblings along the path body -> main
            let node = main;
            while (node && node !== document.body) {
                const parent = node.parentElement;
                if (!parent) break;
                for (const sib of parent.children) {
                    if (sib !== node && !/^(SCRIPT|STYLE|LINK)$/.test(sib.tagName)) {
                        sib.setAttribute('data-s2pdf-hide', '');
                    }
                }
                node = parent;
            }
            // Chrome inside main: always hide structural/interactive elements...
            main.querySelectorAll('aside, nav, form, button, [id="printIcon"]')
                .forEach(el => el.setAttribute('data-s2pdf-hide', ''));
            // ...and class-matched widgets, unless they wrap real content (e.g. accordions
            // are styled "filter-item-container").
            main.querySelectorAll(
                '[class*="filter"], [class*="breadcrumb"], [class*="feedback"], ' +
                '[class*="related"], [class*="share"]'
            ).forEach(el => {
                if (!el.querySelector('article, h2, h3, table') && el.tagName !== 'ARTICLE') {
                    el.setAttribute('data-s2pdf-hide', '');
                }
            });
            const style = document.createElement('style');
            style.textContent =
                '[data-s2pdf-hide]{display:none !important}' +
                'main, #main{margin:0 !important;padding:0 !important;max-width:none !important}';
            document.head.appendChild(style);
            return true;
        })()
    "#;
    let _ = page.evaluate(script).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
}

/// All same-host http(s) links inside the main content (not sidebars/menus/headers/footers).
async fn extract_main_links(page: &chromiumoxide::Page) -> Vec<String> {
    let script = r#"
        (function() {
            const root = document.querySelector('main, [role="main"], #main, #content') || document.body;
            return JSON.stringify([...root.querySelectorAll('a[href]')]
                .filter(a => !a.closest('aside, nav, header, footer'))
                .map(a => a.href)
                .filter(h => { try {
                    const u = new URL(h);
                    return (u.protocol === 'http:' || u.protocol === 'https:') && u.host === location.host;
                } catch (e) { return false; } }));
        })()
    "#;
    page.evaluate(script)
        .await
        .ok()
        .and_then(|v| v.into_value::<String>().ok())
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default()
}

/// Main-content links matching one of the document `patterns` (case-insensitive globs).
async fn extract_document_links(page: &chromiumoxide::Page, patterns: &[String]) -> Vec<String> {
    extract_main_links(page)
        .await
        .into_iter()
        .filter(|l| matches_any_ci(l, patterns))
        .collect()
}

fn matches_any_ci(url: &str, patterns: &[String]) -> bool {
    let url = url.to_lowercase();
    patterns.iter().any(|p| glob_match(&url, &p.to_lowercase()))
}

/// A candidate group of same-host links found while previewing, e.g. `*/dokument/*`.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LinkGroup {
    pub pattern: String,
    pub count: usize,
    pub samples: Vec<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PreviewProgress {
    pub done: usize,
    pub total: usize,
}

const PREVIEW_MAX_PAGES: usize = 9; // start page + 8 sampled pages

/// Scan the start page plus a spread sample of its sitemap subpages and group the
/// same-host links in their main content by first path segment, so the user can pick
/// which kinds of links should be treated as documents.
pub async fn preview_links(
    chromium_path: PathBuf,
    url: String,
    use_sitemap: bool,
    progress: impl Fn(PreviewProgress),
) -> Result<Vec<LinkGroup>> {
    let start = Url::parse(&url).context("Invalid start URL")?;
    let known: Vec<String> = if use_sitemap {
        crate::sitemap::resolve(&url).await.unwrap_or_default()
    } else {
        vec![]
    };

    // Start page + evenly spread sample of the other sitemap pages
    let mut scan: Vec<String> = vec![url.clone()];
    let others: Vec<&String> = known.iter().skip(1).collect();
    let want = PREVIEW_MAX_PAGES.saturating_sub(1).min(others.len());
    for i in 0..want {
        scan.push(others[i * others.len() / want].clone());
    }

    let mut browser = launch_browser(&chromium_path, None).await?;
    let page = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        browser.new_page("about:blank"),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Timeout creating initial page"))?
    .map_err(|e| anyhow::anyhow!("Failed to create initial page: {e}"))?;

    let prefix = build_prefix(&url);
    let skip: HashSet<String> = known.iter().map(|u| normalize_url(u)).collect();
    let mut by_path: std::collections::BTreeMap<String, (String, String)> = Default::default(); // key -> (group, url)

    for (i, u) in scan.iter().enumerate() {
        progress(PreviewProgress { done: i, total: scan.len() });
        let nav = tokio::time::timeout(std::time::Duration::from_secs(30), page.goto(u.as_str())).await;
        if !matches!(nav, Ok(Ok(_))) {
            continue;
        }
        wait_for_ready(&page).await;
        dismiss_cookie_banners(&page).await;
        expand_collapsed(&page).await;

        for link in extract_main_links(&page).await {
            let Ok(lu) = Url::parse(&link) else { continue };
            if lu.host_str() != start.host_str() {
                continue;
            }
            let norm = normalize_url(&link);
            // Already part of the crawl (start page, sitemap pages, or under the start path)
            if skip.contains(&norm) || (link.starts_with(&prefix) && use_sitemap) || norm == normalize_url(&url) {
                continue;
            }
            let Some(seg) = lu.path_segments().and_then(|mut s| s.next()).filter(|s| !s.is_empty()) else {
                continue;
            };
            // Dedupe on host+path (queries like pageId/blockId vary for the same document)
            let key = normalize_url(link.split('?').next().unwrap_or(&link));
            by_path.entry(key).or_insert((format!("*/{}/*", seg.to_lowercase()), link));
        }
    }
    progress(PreviewProgress { done: scan.len(), total: scan.len() });

    let _ = page.close().await;
    let _ = browser.close().await;

    let mut groups: std::collections::BTreeMap<String, LinkGroup> = Default::default();
    for (_, (pattern, link)) in by_path {
        let g = groups.entry(pattern.clone()).or_insert(LinkGroup { pattern, count: 0, samples: vec![] });
        g.count += 1;
        if g.samples.len() < 3 {
            g.samples.push(link);
        }
    }
    let mut out: Vec<LinkGroup> = groups.into_values().collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then(a.pattern.cmp(&b.pattern)));
    Ok(out)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_patterns_match_case_insensitively() {
        let p = vec!["*/dokument/*".to_string(), "*/files/*.pdf".to_string()];
        assert!(matches_any_ci("https://a.se/Dokument/evo/1?x=1", &p));
        assert!(matches_any_ci("https://a.se/files/rutin.PDF", &p));
        assert!(!matches_any_ci("https://a.se/files/rutin.docx", &p));
        assert!(!matches_any_ci("https://a.se/nyheter/1", &p));
        assert!(!matches_any_ci("https://a.se/dokument/1", &[]));
    }

    #[tokio::test]
    #[ignore = "network + local Chromium (set S2P_CHROME)"]
    async fn preview_rjl() {
        let chrome = std::env::var("S2P_CHROME").expect("S2P_CHROME");
        let u = "https://folkhalsaochsjukvard.rjl.se/vardstod/fakta---allmant-kliniskt-kunskapsstod/";
        let groups = preview_links(chrome.into(), u.into(), true, |p| eprintln!("{}/{}", p.done, p.total))
            .await
            .unwrap();
        println!("{groups:#?}");
        assert!(groups.iter().any(|g| g.pattern == "*/dokument/*"));
    }
}
