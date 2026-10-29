use anyhow::Result;
use std::collections::HashSet;
use url::Url;

const MAX_SITEMAP_FILES: usize = 50;

/// Resolve the pages under `start_url`'s path using the site's sitemap(s).
///
/// Looks for `Sitemap:` lines in robots.txt (falling back to /sitemap.xml) and
/// follows sitemap indexes. Only URLs on the same host whose path starts with the
/// start URL's path are returned. The start URL itself is always first.
/// Returns an empty list if no sitemap could be read.
pub async fn resolve(start_url: &str) -> Result<Vec<String>> {
    let start = Url::parse(start_url)?;
    let origin = format!("{}://{}", start.scheme(), start.host_str().unwrap_or(""));
    let prefix = format!("{}/", start.path().trim_end_matches('/'));

    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (compatible; site2pdf)")
        .timeout(std::time::Duration::from_secs(20))
        .build()?;

    let mut pending: Vec<String> = Vec::new();
    if let Ok(resp) = client.get(format!("{origin}/robots.txt")).send().await {
        if let Ok(text) = resp.text().await {
            for line in text.lines() {
                if let Some(rest) = line.trim().strip_prefix_ci("sitemap:") {
                    pending.push(rest.trim().to_string());
                }
            }
        }
    }
    if pending.is_empty() {
        pending.push(format!("{origin}/sitemap.xml"));
    }

    let mut seen_files: HashSet<String> = HashSet::new();
    let mut found: Vec<String> = vec![start_url.to_string()];
    let mut seen_urls: HashSet<String> = HashSet::new();
    seen_urls.insert(norm(start_url));

    while let Some(sm) = pending.pop() {
        if seen_files.len() >= MAX_SITEMAP_FILES || !seen_files.insert(sm.clone()) {
            continue;
        }
        let body = match client.get(&sm).send().await {
            Ok(r) if r.status().is_success() => match r.text().await {
                Ok(t) => t,
                Err(_) => continue,
            },
            _ => continue,
        };

        let is_index = body.contains("<sitemapindex");
        for loc in extract_locs(&body) {
            if is_index {
                pending.push(loc);
                continue;
            }
            let Ok(u) = Url::parse(&loc) else { continue };
            if u.host_str() != start.host_str() || !u.path().starts_with(&prefix) {
                continue;
            }
            if seen_urls.insert(norm(&loc)) {
                found.push(loc);
            }
        }
    }

    if found.len() <= 1 {
        return Ok(vec![]);
    }
    Ok(found)
}

fn norm(url: &str) -> String {
    url.split('#').next().unwrap_or(url).trim_end_matches('/').to_lowercase()
}

/// Pull the text of every `<loc>` element (handles `&amp;` and CDATA).
fn extract_locs(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<loc>") {
        let after = &rest[start + 5..];
        let Some(end) = after.find("</loc>") else { break };
        let raw = after[..end]
            .trim()
            .trim_start_matches("<![CDATA[")
            .trim_end_matches("]]>")
            .trim();
        out.push(raw.replace("&amp;", "&"));
        rest = &after[end + 6..];
    }
    out
}

trait StripPrefixCi {
    fn strip_prefix_ci(&self, prefix: &str) -> Option<&str>;
}

impl StripPrefixCi for str {
    fn strip_prefix_ci(&self, prefix: &str) -> Option<&str> {
        if self.len() >= prefix.len() && self.is_char_boundary(prefix.len())
            && self[..prefix.len()].eq_ignore_ascii_case(prefix)
        {
            Some(&self[prefix.len()..])
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_locs() {
        let xml = "<urlset><url><loc>https://a.se/x?a=1&amp;b=2</loc></url><url><loc> https://a.se/y </loc></url></urlset>";
        assert_eq!(extract_locs(xml), vec!["https://a.se/x?a=1&b=2", "https://a.se/y"]);
    }

    #[tokio::test]
    #[ignore = "network"]
    async fn resolves_rjl() {
        let u = "https://folkhalsaochsjukvard.rjl.se/vardstod/fakta---allmant-kliniskt-kunskapsstod/";
        let list = resolve(u).await.unwrap();
        println!("{} urls, first: {:?}", list.len(), &list[..3.min(list.len())]);
        assert_eq!(list[0], u);
        assert!(list.len() > 20 && list.iter().all(|x| x.starts_with(u)));
    }
}
