use crate::models::NewsArticle;
use futures::stream::{self, StreamExt};
use once_cell::sync::Lazy;
use reqwest::{Client, Url};
use scraper::{ElementRef, Html, Selector};
use std::error::Error;
use std::time::Duration;
use tracing::{debug, error, info, instrument, warn};

// --- New: date parsing helpers
use chrono::{DateTime, FixedOffset};
use serde::Deserialize;

// Global HTTP client with realistic UA + timeouts
static CLIENT: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .user_agent(concat!(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) ",
            "AppleWebKit/537.36 (KHTML, like Gecko) ",
            "Chrome/127.0.0.0 Safari/537.36"
        ))
        .timeout(Duration::from_secs(20))
        .pool_idle_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .expect("failed to build reqwest client")
});

/// Index AP News articles via Google search (last 24 hours)
#[instrument(level = "info")]
pub async fn index_articles() -> Result<Vec<String>, Box<dyn Error>> {
    // Use News vertical (tbm=nws) + last 24h (qdr:d) + more results to dedupe later
    let google_search_url = "https://www.google.com/search?q=site%3Aapnews.com+inurl%3Aarticle&hl=en&gl=us&tbm=nws&tbs=qdr:d&num=50";

    let html = CLIENT.get(google_search_url).send().await?.text().await?;
    let document = Html::parse_document(&html);

    if html.contains("consent.google.com")
        || html.contains("unusual traffic from your computer network")
    {
        warn!("Google interstitial/antibot detected; results may be incomplete.");
    }

    // Prefer explicit '/url?q=' wrappers, but also accept direct apnews links.
    let link_selector = Selector::parse("a[href]").unwrap();

    let mut article_urls = Vec::<String>::new();
    for element in document.select(&link_selector) {
        if let Some(href) = element.value().attr("href") {
            if let Some(url) = extract_apnews_url(href) {
                if !article_urls.contains(&url) {
                    article_urls.push(url);
                }
            }
        }
        if article_urls.len() >= 20 {
            break;
        }
    }

    info!(
        count = article_urls.len(),
        source = "Google search (tbm=nws, qdr:d)",
        "Indexed AP News article URLs"
    );
    debug!(urls = ?article_urls, "AP News URLs");

    Ok(article_urls)
}

/// Extract a clean https://apnews.com/article/... from a Google link or direct href.
fn extract_apnews_url(href: &str) -> Option<String> {
    if href.starts_with("/url?q=") {
        let raw = href.trim_start_matches("/url?q=");
        let main = raw.split('&').next().unwrap_or("");
        if main.contains("apnews.com/article/") {
            urlencoding::decode(main).ok().map(|s| s.to_string())
        } else {
            None
        }
    } else if href.starts_with("https://apnews.com/article/")
        || href.starts_with("http://apnews.com/article/")
    {
        Some(href.to_string())
    } else {
        None
    }
}

/// Fetch all AP News articles concurrently
#[instrument(level = "info", skip_all)]
pub async fn fetch_articles(urls: Vec<String>) -> Vec<NewsArticle> {
    let concurrency = 8usize;

    let articles: Vec<NewsArticle> = stream::iter(urls.into_iter())
        // produce futures
        .map(|url| async move {
            let res = fetch_article(&url).await;
            (url, res)
        })
        // run up to `concurrency` futures at a time
        .buffer_unordered(concurrency)
        // keep only successful parses, with logging
        .filter_map(|(url, res)| async move {
            match res {
                Ok(Some(article)) => {
                    debug!(%url, "Fetched AP News article");
                    Some(article)
                }
                Ok(None) => {
                    warn!(%url, "AP News fetch produced no content");
                    None
                }
                Err(e) => {
                    error!(error = %e, %url, "AP News fetch failed");
                    None
                }
            }
        })
        .collect()
        .await;

    info!(count = articles.len(), "Fetched AP News article contents");
    articles
}

/// Fetch a single AP News article
#[instrument(level = "info", skip_all, fields(%url))]
async fn fetch_article(url: &str) -> Result<Option<NewsArticle>, Box<dyn Error>> {
    // Basic sanity check: only fetch apnews.com/article/ links
    let parsed = Url::parse(url)?;
    if parsed.domain().unwrap_or_default().ends_with("apnews.com") == false
        || !parsed.path().contains("/article/")
    {
        warn!(%url, "Skipping non-article or non-apnews domain");
        return Ok(None);
    }

    let body = CLIENT.get(url).send().await?.text().await?;
    let document = Html::parse_document(&body);

    // ----- PUBLISHED AT (robust) -----
    let (published_dt, published_raw, published_src) = extract_published_at(&document);
    if let Some(ref raw) = published_raw {
        info!(
            source = published_src,
            raw = %raw,
            iso = published_dt
                .as_ref()
                .map(|d| d.to_rfc3339())
                .unwrap_or_else(|| "n/a".into()),
            "Published-at parsed"
        );
    } else {
        info!("Published-at parsed source=none");
    }

    // ----- CONTENT EXTRACTION -----
    // Try multiple body containers AP has used historically.
    let candidates = [
        ".RichTextStoryBody",
        ".RichTextBody",
        "div[data-t=\"article-body\"]",
        "article[role=\"main\"]",
        "article",
    ];

    let mut content = String::new();
    let mut found = false;

    for sel in candidates.iter().filter_map(|s| Selector::parse(s).ok()) {
        for node in document.select(&sel) {
            let text = extract_clean_text(&node);
            if !text.trim().is_empty() {
                if !content.is_empty() {
                    content.push_str("\n\n");
                }
                content.push_str(text.trim());
                found = true;
            }
        }
        if found {
            break;
        }
    }

    // Prepend date info
    if let Some(dt) = published_dt {
        content = format!("Published: {}\n\n{}", dt.to_rfc3339(), content);
    } else if let Some(raw) = published_raw {
        content = format!("Published(raw): {}\n\n{}", raw, content);
    }

    let len = content.len();
    info!(bytes = len, "Parsed AP News article");

    if found && len > 0 {
        Ok(Some(NewsArticle {
            source: url.to_string(),
            content,
        }))
    } else {
        // Dump a small slice of HTML to help debug selector drift
        debug!(
            preview = %body.chars().take(600).collect::<String>().replace('\n', " "),
            "No article content parsed; HTML preview"
        );
        Ok(None)
    }
}

/* -------------------- TEXT SANITIZATION HELPERS -------------------- */

/// Extract clean text from an HTML element, excluding script and style tags
fn extract_clean_text(element: &ElementRef) -> String {
    let script_sel = Selector::parse("script").unwrap();
    let style_sel = Selector::parse("style").unwrap();
    
    let mut text_parts = Vec::new();
    
    for node in element.descendants() {
        // Collect text nodes, but only if they're not inside script/style tags
        if let Some(text) = node.value().as_text() {
            let content = text.trim();
            if !content.is_empty() {
                // Check if any ancestor is a script or style tag
                let mut current = node.parent();
                let mut in_excluded_tag = false;
                
                while let Some(ancestor) = current {
                    if let Some(elem) = ElementRef::wrap(ancestor) {
                        if script_sel.matches(&elem) || style_sel.matches(&elem) {
                            in_excluded_tag = true;
                            break;
                        }
                    }
                    current = ancestor.parent();
                }
                
                if !in_excluded_tag {
                    text_parts.push(content);
                }
            }
        }
    }
    
    text_parts.join(" ")
}

/* -------------------- DATE HELPERS -------------------- */

fn looks_like_placeholder(s: &str) -> bool {
    let t = s.trim();
    t.contains('[') && t.contains(']')
}

fn clean(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LdArticle {
    #[serde(default)]
    date_published: Option<String>,
    #[serde(default)]
    date_modified: Option<String>,
}

fn parse_rfc3339(s: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(s).ok()
}

/// Extract (published_iso, raw_string, source_hint)
fn extract_published_at(document: &Html) -> (Option<DateTime<FixedOffset>>, Option<String>, &'static str) {
    // A) JSON-LD blocks
    if let Ok(sel) = Selector::parse(r#"script[type="application/ld+json"]"#) {
        for script in document.select(&sel) {
            if let Some(js) = script
                .first_child()
                .and_then(|n| n.value().as_text())
                .map(|t| t.to_string())
            {
                let txt = js.trim();
                // Try array or single object
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(txt) {
                    if let Some((dt, raw)) = scan_jsonld_value(&v) {
                        let raw_clean = clean(&raw);
                        if !looks_like_placeholder(&raw_clean) {
                            if let Some(dt) = parse_rfc3339(&dt) {
                                return (Some(dt), Some(raw_clean), "jsonld");
                            }
                        }
                    }
                }
            }
        }
    }

    // B) <meta property="article:published_time">
    if let Some((raw, _)) = first_meta(document, r#"meta[property="article:published_time"]"#, "content") {
        let raw = clean(&raw);
        if !looks_like_placeholder(&raw) {
            if let Some(dt) = parse_rfc3339(&raw) {
                return (Some(dt), Some(raw), "og:article:published_time");
            }
        }
    }

    // C) Other common meta fallbacks
    for css in &[
        r#"meta[itemprop="datePublished"]"#,
        r#"meta[name="date"]"#,
        r#"meta[property="og:updated_time"]"#,
    ] {
        if let Some((raw, _)) = first_meta(document, css, "content") {
            let raw = clean(&raw);
            if !looks_like_placeholder(&raw) {
                if let Some(dt) = parse_rfc3339(&raw) {
                    return (Some(dt), Some(raw), css);
                }
            }
        }
    }

    // D) <time datetime="...">
    if let Ok(sel) = Selector::parse(r#"time[datetime]"#) {
        if let Some(t) = document.select(&sel).next() {
            if let Some(raw) = t.value().attr("datetime").map(|s| clean(s)) {
                if !looks_like_placeholder(&raw) {
                    if let Some(dt) = parse_rfc3339(&raw) {
                        return (Some(dt), Some(raw), "time[datetime]");
                    }
                }
            }
        }
    }

    // E) Textual fallbacks (often placeholders — keep as raw only)
    if let Ok(sel) = Selector::parse(".Page-dateModified, .Page-datePublished, time") {
        if let Some(el) = document.select(&sel).next() {
            let raw = clean(&el.text().collect::<String>());
            if !looks_like_placeholder(&raw) && !raw.is_empty() {
                return (None, Some(raw), "textual");
            }
        }
    }

    (None, None, "none")
}

fn first_meta<'a>(document: &'a Html, css: &str, attr: &str) -> Option<(String, ElementRef<'a>)> {
    let sel = Selector::parse(css).ok()?;
    let n = document.select(&sel).next()?;
    let v = n.value().attr(attr)?.to_string();
    Some((v, n))
}

fn scan_jsonld_value(v: &serde_json::Value) -> Option<(String, String)> {
    match v {
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let Some(p) = pick_date_from_ld(item) {
                    return Some(p);
                }
            }
            None
        }
        _ => pick_date_from_ld(v),
    }
}

fn pick_date_from_ld(v: &serde_json::Value) -> Option<(String, String)> {
    // Prefer Article-ish types but don’t require @type
    let is_article = v
        .get("@type")
        .and_then(|t| t.as_str())
        .map(|t| matches!(t, "NewsArticle" | "Article" | "Report" | "BlogPosting"))
        .unwrap_or(true);

    if is_article {
        if let Some(raw) = v.get("datePublished").and_then(|x| x.as_str()).map(|s| s.to_string()) {
            return Some((raw.clone(), raw));
        }
        if let Some(raw) = v.get("dateModified").and_then(|x| x.as_str()).map(|s| s.to_string()) {
            return Some((raw.clone(), raw));
        }
    }
    if let Some(obj) = v.get("article") {
        if let Some(raw) = obj.get("datePublished").and_then(|x| x.as_str()).map(|s| s.to_string())
        {
            return Some((raw.clone(), raw));
        }
    }
    None
}
