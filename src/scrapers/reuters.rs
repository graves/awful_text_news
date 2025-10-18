use crate::models::NewsArticle;
use futures::stream::{self, StreamExt};
use once_cell::sync::Lazy;
use reqwest::{Client, Url};
use scraper::{ElementRef, Html, Selector};
use std::borrow::Cow;
use std::error::Error;
use std::time::Duration;
use tracing::{debug, error, info, instrument, warn};

// --- New: date parsing helpers
use chrono::{DateTime, FixedOffset};
use serde::Deserialize;

// XML
use quick_xml::events::Event;
use quick_xml::{escape, Reader};

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

const GNEWS_RSS: &[&str] = &[
    "https://news.google.com/rss/search?q=site%3Areuters.com%2Fworld%2F&hl=en-US&gl=US&ceid=US%3Aen",
    "https://news.google.com/rss/search?q=site%3Areuters.com%2Fsustainability%2F&hl=en-US&gl=US&ceid=US%3Aen",
    "https://news.google.com/rss/search?q=site%3Areuters.com%2Ftechnology%2F&hl=en-US&gl=US&ceid=US%3Aen",
];

/// GNews-only indexing (fully drops direct reuters.com scraping)
#[instrument(level = "info")]
pub async fn index_articles() -> Result<Vec<String>, Box<dyn Error>> {
    let mut all = Vec::<String>::new();

    for rss in GNEWS_RSS {
        match fetch_gnews_links_with_debug(rss).await {
            Ok(mut links) => {
                let before = all.len();
                all.append(&mut links);
                let added = all.len() - before;
                info!(rss, added, "GNews-only indexing");
            }
            Err(e) => {
                error!(error = %e, rss, "GNews RSS fetch failed");
            }
        }
    }

    all.sort();
    all.dedup();
    info!(total = all.len(), "Total Reuters URLs (via GNews only)");

    if all.is_empty() {
        warn!("GNews-only path returned 0 URLs — check connectivity to news.google.com and redirect policy.");
    }

    Ok(all)
}

#[instrument(level = "debug", skip_all, fields(rss))]
async fn fetch_gnews_links_with_debug(rss: &str) -> Result<Vec<String>, Box<dyn Error>> {
    // 1) HTTP request with rich debug
    let req = CLIENT.get(rss);
    debug!("Issuing GET to GNews RSS …");
    let res = req.send().await?;

    let status = res.status();
    let final_url = res.url().to_string();
    let ctype = res
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("<none>");
    let clen = res
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("<none>");

    info!(%final_url, %status, %ctype, %clen, "GNews RSS response");

    let bytes = res.bytes().await?;
    let body_preview = String::from_utf8_lossy(&bytes);
    debug!(
        preview = %body_preview.chars().take(600).collect::<String>().replace('\n', " "),
        "GNews RSS body (first 600 chars)"
    );

    // 2) Try strict XML first
    let mut links = parse_gnews_rss_links(&bytes)?;

    // 3) Regex fallback if the strict XML path failed
    if links.is_empty() {
        warn!("XML parse produced 0 links; attempting regex fallback");
        let mut from_rx = harvest_gnews_regex(&body_preview);
        debug!(count = from_rx.len(), "Regex fallback produced links");
        links.append(&mut from_rx);
    }

    // 4) Dedupe & limit
    links.sort();
    links.dedup();
    links.truncate(50);

    // 5) Emit per-link debug
    for (i, u) in links.iter().enumerate() {
        debug!(index = i, url = %u, "GNews candidate URL");
    }

    Ok(links)
}

/// Strict XML parser for GNews RSS (quick-xml 0.38)
fn parse_gnews_rss_links(xml_bytes: &[u8]) -> Result<Vec<String>, Box<dyn Error>> {
    let mut reader = Reader::from_reader(xml_bytes);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::<u8>::new();
    let mut in_item = false;
    let mut in_link = false;

    let mut seen_items = 0usize;
    let mut text_nodes = 0usize;
    let mut links = Vec::<String>::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name().as_ref().to_ascii_lowercase();
                if name.as_slice() == b"item" {
                    in_item = true;
                    seen_items += 1;
                    debug!(seen_items, "RSS: <item> start");
                } else if in_item && name.as_slice() == b"link" {
                    in_link = true;
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name().as_ref().to_ascii_lowercase();
                if name.as_slice() == b"item" {
                    in_item = false;
                } else if name.as_slice() == b"link" {
                    in_link = false;
                }
            }
            Ok(Event::Text(t)) => {
                text_nodes += 1;
                if in_item && in_link {
                    // BytesText -> &str
                    let raw = match std::str::from_utf8(t.as_ref()) {
                        Ok(s) => s,
                        Err(e) => {
                            warn!(error = %e, "UTF-8 decode failed for <link> text");
                            ""
                        }
                    };
                    // Unescape XML entities (&amp; etc.)
                    let unescaped: Cow<'_, str> = match escape::unescape(raw) {
                        Ok(cow) => cow,
                        Err(e) => {
                            warn!(error = %e, "Unescape failed for <link> text");
                            Cow::Borrowed(raw)
                        }
                    };
                    let s = unescaped.trim();

                    // Debug the raw link we saw
                    debug!(link = %s, "RSS: <link> text inside <item>");

                    // GNews “real” article links look like:
                    //   https://news.google.com/rss/articles/...
                    //   https://news.google.com/articles/...
                    // We accept both; fetch step will resolve later if needed.
                    if s.starts_with("https://news.google.com/rss/articles/")
                        || s.starts_with("https://news.google.com/articles/")
                    {
                        links.push(s.to_string());
                    } else {
                        // Sometimes feed presents a /articles/ link in <guid> and a web UI link in <link>.
                        // We'll log what we saw to diagnose.
                        debug!(skipped_link = %s, "RSS <link> did not match expected Google articles pattern");
                    }
                }
            }
            Ok(Event::Eof) => break,
            Ok(other) => {
                // Extra verbosity for debugging parser state
                if matches!(other, Event::Comment(_) | Event::CData(_)) {
                    // keep quiet for chatter
                } else {
                    debug!("RSS: {:?}", other);
                }
            }
            Err(e) => {
                error!(error = %e, offset = reader.buffer_position(), "XML parse error");
                break;
            }
        }
        buf.clear();
    }

    info!(seen_items, text_nodes, collected = links.len(), "GNews RSS parse summary");
    Ok(links)
}

/// Regex fallback: grab <link>…</link> that contains news.google.com/articles
fn harvest_gnews_regex(s: &str) -> Vec<String> {
    let mut out = Vec::<String>::new();
    let re = regex::Regex::new(r#"https://news\.google\.com/(?:rss/)?articles/[A-Za-z0-9_\-?=&#%]+"#).unwrap();
    for m in re.find_iter(s) {
        out.push(m.as_str().to_string());
        if out.len() >= 100 {
            break;
        }
    }
    out.sort();
    out.dedup();
    out
}

/* ======================= FETCHING ARTICLES ======================= */
/* The Google News links ultimately point to Reuters content. We keep the rest of
   your article parsing as-is (modern Reuters selectors + robust date parsing).
   The change here: if we land on news.google.com, we extract the outbound Reuters
   URL from the interstitial HTML and follow it.
*/

/// Fetch all Reuters articles concurrently
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
                    debug!(%url, "Fetched Reuters article");
                    Some(article)
                }
                Ok(None) => {
                    warn!(%url, "Reuters fetch produced no content");
                    None
                }
                Err(e) => {
                    error!(error = %e, %url, "Reuters fetch failed");
                    None
                }
            }
        })
        .collect()
        .await;

    info!(count = articles.len(), "Fetched Reuters article contents");
    articles
}

/// Fetch a single article (accepts Google News article URLs and Reuters URLs)
#[instrument(level = "info", skip_all, fields(%url))]
async fn fetch_article(url: &str) -> Result<Option<NewsArticle>, Box<dyn Error>> {
    // First GET whatever we were given (Google News article or Reuters)
    let resp = CLIENT.get(url).send().await?;
    let final_url = resp.url().to_string();
    let status = resp.status();
    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("<none>");

    info!(%final_url, %status, %ctype, "Article fetch response");

    let body = resp.text().await?;
    let document = Html::parse_document(&body);

    // NEW: If we landed on a GNews interstitial, extract and follow the outbound Reuters URL
    let is_gnews = Url::parse(&final_url)
        .ok()
        .and_then(|u| u.domain().map(|d| d.ends_with("news.google.com")))
        .unwrap_or(false);

    if is_gnews {
        if let Some(out) = extract_reuters_from_gnews(&document, &body) {
            info!(outbound = %out, "Extracted outbound Reuters URL from GNews interstitial");

            // Follow the outbound URL
            let resp2 = CLIENT.get(&out).send().await?;
            let final2 = resp2.url().to_string();
            let status2 = resp2.status();
            let ctype2 = resp2
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("<none>");
            info!(%final2, %status2, %ctype2, "Reuters follow-up fetch response");

            let body2 = resp2.text().await?;
            return parse_reuters_html(&final2, &body2);
        } else {
            warn!("Landed on news.google.com but failed to extract outbound Reuters link");
            debug!(
                preview = %body.chars().take(800).collect::<String>().replace('\n', " "),
                "GNews interstitial HTML preview"
            );
            return Ok(None);
        }
    }

    // If we already ended up on Reuters, parse directly
    if final_url.contains("www.reuters.com") {
        return parse_reuters_html(&final_url, &body);
    } else {
        // Not on Reuters after following GNews link
        warn!(%final_url, "Landed on non-Reuters domain after following GNews link");
        debug!(
            preview = %body.chars().take(800).collect::<String>().replace('\n', " "),
            "Non-Reuters HTML preview"
        );
        Ok(None)
    }
}

/* -------------------- OUTBOUND EXTRACTION HELPERS -------------------- */

fn first_abs_href_on_host(document: &Html, host: &str) -> Option<String> {
    let sel = Selector::parse("a[href]").ok()?;
    for a in document.select(&sel) {
        if let Some(href) = a.value().attr("href") {
            if href.starts_with("http://") || href.starts_with("https://") {
                if let Ok(u) = Url::parse(href) {
                    if u.domain() == Some(host) {
                        return Some(href.to_string());
                    }
                }
            }
        }
    }
    None
}

fn meta_refresh_target(document: &Html) -> Option<String> {
    let sel = Selector::parse(r#"meta[http-equiv="refresh"]"#).ok()?;
    for m in document.select(&sel) {
        if let Some(content) = m.value().attr("content") {
            // e.g. "0;url=https://www.reuters.com/..."
            if let Some(pos) = content.to_lowercase().find("url=") {
                let target = &content[pos + 4..];
                // trim quotes/spaces
                let t = target.trim().trim_matches('"').trim_matches('\'');
                if t.starts_with("http://") || t.starts_with("https://") {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}

fn og_url(document: &Html) -> Option<String> {
    let sel = Selector::parse(r#"meta[property="og:url"]"#).ok()?;
    document
        .select(&sel)
        .next()
        .and_then(|n| n.value().attr("content"))
        .map(|s| s.to_string())
}

fn extract_reuters_from_gnews(document: &Html, raw_html: &str) -> Option<String> {
    // 1) meta refresh wins
    if let Some(u) = meta_refresh_target(document) {
        if Url::parse(&u).ok()?.domain() == Some("www.reuters.com") {
            return Some(u);
        }
    }
    // 2) <a href="https://www.reuters.com/...">
    if let Some(u) = first_abs_href_on_host(document, "www.reuters.com") {
        return Some(u);
    }
    // 3) og:url sometimes points to the article
    if let Some(u) = og_url(document) {
        if Url::parse(&u).ok()?.domain() == Some("www.reuters.com") {
            return Some(u);
        }
    }
    // 4) Regex fallback anywhere in HTML
    let re = regex::Regex::new(r#"https://www\.reuters\.com/[^\s"']+"#).ok()?;
    if let Some(m) = re.find(raw_html) {
        return Some(m.as_str().to_string());
    }
    None
}

/* -------------------- REUTERS PARSER (reused) -------------------- */

fn parse_reuters_html(final_url: &str, body: &str) -> Result<Option<NewsArticle>, Box<dyn std::error::Error>> {
    let document = Html::parse_document(body);

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
        .or_else(|| text_of_first(&document, "h1"))
        .unwrap_or_default();

    // ----- CONTENT EXTRACTION -----
    let candidates = [
        r#"div[data-testid="article-body"] p"#,
        r#"article p[data-testid^="paragraph-"]"#,
        r#"article p"#,
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

    if !title.is_empty() {
        content = format!("Title: {}\n\n{}", title, content);
    }
    if let Some(dt) = published_dt {
        content = format!("Published: {}\n\n{}", dt.to_rfc3339(), content);
    } else if let Some(raw) = published_raw {
        content = format!("Published(raw): {}\n\n{}", raw, content);
    }

    let len = content.len();
    info!(%final_url, bytes = len, "Parsed Reuters article");

    if found && len > 0 {
        Ok(Some(NewsArticle { source: final_url.to_string(), content }))
    } else {
        debug!(
            preview = %body.chars().take(800).collect::<String>().replace('\n', " "),
            "No article content parsed; HTML preview"
        );
        Ok(None)
    }
}

/* -------------------- DATE HELPERS (unchanged) -------------------- */

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
    // JSON-LD blocks
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

    // <meta property="article:published_time">
    if let Some((raw, _)) = first_meta(document, r#"meta[property="article:published_time"]"#, "content") {
        let raw = clean(&raw);
        if !looks_like_placeholder(&raw) {
            if let Some(dt) = parse_rfc3339(&raw) {
                return (Some(dt), Some(raw), "og:article:published_time");
            }
        }
    }

    // Other common meta fallbacks
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

    // <time datetime="...">
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

    // Textual fallback
    if let Ok(sel) = Selector::parse(".ArticleHeader-date, .Page-datePublished, time") {
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