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

// (Optional) You can add default headers here if needed; UA + timeouts are already set.
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

const SECTION_URLS: &[&str] = &[
    // BBC News homepage as the single “section” to pull ~20 article URLs
    "https://www.bbc.com/news",
];

/// Index BBC News articles from the homepage (target ~20; de-dup)
#[instrument(level = "info")]
pub async fn index_articles() -> Result<Vec<String>, Box<dyn Error>> {
    let mut all = Vec::<String>::new();

    for section in SECTION_URLS {
        let res = CLIENT.get(*section).send().await?;
        let final_url = res.url().to_string();
        let html = res.text().await?;
        let document = Html::parse_document(&html);

        // Primary: the anchors shown in your snippet
        let sel_internal = Selector::parse(r#"a[data-testid="internal-link"][href]"#).unwrap();
        // Fallback: any anchors
        let sel_any_a = Selector::parse(r#"a[href]"#).unwrap();

        let mut urls = Vec::<String>::new();

        // 1) Strict selector first
        harvest_selector_bbc(&document, &sel_internal, &mut urls);

        // 2) Fallback: any anchors that look like BBC /news/articles/<id>
        if urls.len() < 20 {
            for a in document.select(&sel_any_a) {
                if let Some(href) = a.value().attr("href") {
                    if let Some(u) = normalize_bbc_link(href) {
                        if is_bbc_article_url(&u) && !urls.contains(&u) {
                            urls.push(u);
                            if urls.len() >= 20 { break; }
                        }
                    }
                }
            }
        }

        // 3) Regex fallback from raw HTML
        if urls.len() < 20 {
            let mut more = harvest_regex_fallback_bbc(&html);
            for u in more.drain(..) {
                if !urls.contains(&u) {
                    urls.push(u);
                    if urls.len() >= 20 { break; }
                }
            }
        }

        if urls.is_empty() {
            dump_bbc_debug(*section, &document, &html, &final_url);
        }

        info!(section = *section, count = urls.len(), "Indexed BBC section URLs");
        debug!(?urls, "BBC URLs");
        all.extend(urls);
    }

    all.sort();
    all.dedup();
    info!(total = all.len(), "Total indexed BBC URLs");
    Ok(all)
}

fn harvest_selector(document: &Html, sel: &Selector, urls: &mut Vec<String>) {
    // kept to satisfy the shared API surface; Reuters-specific, unused here
    harvest_selector_bbc(document, sel, urls)
}

fn harvest_selector_bbc(document: &Html, sel: &Selector, urls: &mut Vec<String>) {
    for a in document.select(sel) {
        if urls.len() >= 20 {
            break;
        }
        if let Some(href) = a.value().attr("href") {
            if let Some(url) = normalize_bbc_link(href) {
                if is_bbc_article_url(&url) && !urls.contains(&url) {
                    urls.push(url);
                }
            }
        }
    }
}

/// Regex fallback to find /news/articles/<id> links in raw HTML
fn harvest_regex_fallback(html: &str) -> Vec<String> {
    // kept to satisfy the shared API surface; Reuters-specific, unused here
    harvest_regex_fallback_bbc(html)
}

fn harvest_regex_fallback_bbc(html: &str) -> Vec<String> {
    let re = regex::Regex::new(r#""(https?://www\.bbc\.com/news/articles/[a-zA-Z0-9]+|/news/articles/[a-zA-Z0-9]+)""#).unwrap();
    let mut out = Vec::<String>::new();
    for cap in re.captures_iter(html) {
        let href = cap.get(1).unwrap().as_str();
        if let Some(u) = normalize_bbc_link(href) {
            if is_bbc_article_url(&u) {
                out.push(u);
            }
        }
        if out.len() >= 50 { break; }
    }
    out.sort();
    out.dedup();
    out.truncate(20);
    out
}

fn is_target_vertical(_url: &str) -> bool {
    // kept to satisfy the shared API surface; Reuters-specific, unused here
    true
}

fn normalize_reuters_link(href: &str) -> Option<String> {
    // kept to satisfy the shared API surface; Reuters-specific, unused here
    if href.starts_with('/') {
        Some(format!("https://www.reuters.com{}", href))
    } else {
        Some(href.to_string())
    }
}

fn normalize_bbc_link(href: &str) -> Option<String> {
    if href.starts_with("https://www.bbc.com/") || href.starts_with("http://www.bbc.com/") {
        Some(href.to_string())
    } else if href.starts_with('/') {
        Some(format!("https://www.bbc.com{}", href))
    } else {
        None
    }
}

fn is_bbc_article_url(u: &str) -> bool {
    u.starts_with("https://www.bbc.com/news/articles/")
}

/// Fetch all BBC articles concurrently
#[instrument(level = "info", skip_all)]
pub async fn fetch_articles(urls: Vec<String>) -> Vec<NewsArticle> {
    let concurrency = 8usize;

    let articles: Vec<NewsArticle> = stream::iter(urls.into_iter())
        .map(|url| async move {
            let res = fetch_article(&url).await;
            (url, res)
        })
        .buffer_unordered(concurrency)
        .filter_map(|(url, res)| async move {
            match res {
                Ok(Some(article)) => {
                    debug!(%url, "Fetched BBC article");
                    Some(article)
                }
                Ok(None) => {
                    warn!(%url, "BBC fetch produced no content");
                    None
                }
                Err(e) => {
                    error!(error = %e, %url, "BBC fetch failed");
                    None
                }
            }
        })
        .collect()
        .await;

    info!(count = articles.len(), "Fetched BBC article contents");
    articles
}

/// Fetch a single BBC article
#[instrument(level = "info", skip_all, fields(%url))]
async fn fetch_article(url: &str) -> Result<Option<NewsArticle>, Box<dyn Error>> {
    // Basic sanity: only fetch BBC /news/articles/* pages
    let parsed = Url::parse(url)?;
    if parsed.domain().unwrap_or_default() != "www.bbc.com" || !is_bbc_article_url(url) {
        warn!(%url, "Skipping non-target BBC URL");
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

    // ----- TITLE -----
    let title = meta_content(&document, r#"meta[property="og:title"]"#, "content")
        .or_else(|| text_of_first(&document, r#"h1[data-testid="headline"]"#))
        .or_else(|| text_of_first(&document, "h1"))
        .unwrap_or_default();

    // ----- CONTENT EXTRACTION -----
    let candidates = [
        r#"main div[data-component="text-block"] p"#,
        r#"article div[data-component="text-block"] p"#,
        r#"article p"#,
        r#"main p"#,
    ];

    let mut content = String::new();
    let mut found = false;

    for sel in candidates.iter().filter_map(|s| Selector::parse(s).ok()) {
        let mut parts = Vec::<String>::new();
        for node in document.select(&sel) {
            let text = node.text().collect::<Vec<_>>().join(" ").trim().to_string();
            if !text.is_empty() {
                parts.push(text);
            }
        }
        if !parts.is_empty() {
            content = parts.join("\n\n");
            found = true;
            break;
        }
    }

    // Prepend Title + Date
    if !title.is_empty() {
        content = format!("Title: {}\n\n{}", title, content);
    }
    if let Some(dt) = published_dt {
        content = format!("Published: {}\n\n{}", dt.to_rfc3339(), content);
    } else if let Some(raw) = published_raw {
        content = format!("Published(raw): {}\n\n{}", raw, content);
    }

    let len = content.len();
    info!(bytes = len, "Parsed BBC article");

    if found && len > 0 {
        Ok(Some(NewsArticle {
            source: url.to_string(),
            content,
        }))
    } else {
        debug!(
            preview = %body.chars().take(600).collect::<String>().replace('\n', " "),
            "No article content parsed; HTML preview"
        );
        Ok(None)
    }
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

    // B) Meta properties
    for css in &[
        r#"meta[property="article:published_time"]"#,
        r#"meta[name="OriginalPublicationDate"]"#,
        r#"meta[itemprop="datePublished"]"#,
        r#"meta[property="og:updated_time"]"#,
        r#"meta[name="Last-Modified"]"#,
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

    // C) <time datetime="...">
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

    // D) Textual fallback
    if let Ok(sel) = Selector::parse(r#"[data-testid="timestamp"], time"#) {
        if let Some(el) = document.select(&sel).next() {
            let raw = clean(&el.text().collect::<String>());
            if !raw.is_empty() && !looks_like_placeholder(&raw) {
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

/* -------------------- DEBUG (optional) -------------------- */

fn dump_bbc_debug(section: &str, document: &Html, html: &str, final_url: &str) {
    let any_a = Selector::parse("a[href]").unwrap();
    let internal = Selector::parse(r#"a[data-testid="internal-link"][href]"#).unwrap();

    eprintln!("\n--- BBC NEWS DEBUG: 0 URLs @ {section} ---");
    eprintln!("Fetched URL (after redirects): {final_url}");
    eprintln!("HTML length: {}", html.len());

    let count_internal = document.select(&internal).count();
    eprintln!(r#"Found a[data-testid="internal-link"][href] count = {}"#, count_internal);

    eprintln!("First ~40 hrefs:");
    for (i, a) in document.select(&any_a).take(40).enumerate() {
        if let Some(h) = a.value().attr("href") {
            eprintln!("[{i:02}] {h}");
        }
    }
    eprintln!("--------------------------------\n");
}

/* -------------------- MISC HELPERS -------------------- */

fn text_of_first(document: &Html, css: &str) -> Option<String> {
    let sel = Selector::parse(css).ok()?;
    let n = document.select(&sel).next()?;
    Some(n.text().collect::<Vec<_>>().join(" ").trim().to_string())
}

fn meta_content(document: &Html, css: &str, attr: &str) -> Option<String> {
    let sel = Selector::parse(css).ok()?;
    let n = document.select(&sel).next()?;
    n.value().attr(attr).map(|s| s.to_string())
}