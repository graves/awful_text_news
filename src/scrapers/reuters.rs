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

static CLIENT: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .user_agent(concat!(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) ",
            "AppleWebKit/537.36 (KHTML, like Gecko) ",
            "Chrome/127.0.0.0 Safari/537.36"
        ))
        // These headers help avoid “bare bot” responses
        .default_headers(
            {
                use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE, REFERER};
                let mut h = HeaderMap::new();
                h.insert(ACCEPT, HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8"));
                h.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.9"));
                h.insert(REFERER, HeaderValue::from_static("https://www.google.com/"));
                h
            }
        )
        .timeout(Duration::from_secs(20))
        .pool_idle_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .expect("failed to build reqwest client")
});

const SECTION_URLS: &[&str] = &[
    "https://www.reuters.com/world/",
    "https://www.reuters.com/sustainability/",
    "https://www.reuters.com/technology/",
];

/// Index Reuters articles (top 10 from each section; de-duped)
#[instrument(level = "info")]
pub async fn index_articles() -> Result<Vec<String>, Box<dyn Error>> {
    let mut all = Vec::<String>::new();

    for section in SECTION_URLS {
        let res = CLIENT.get(*section).send().await?;
        let final_url = res.url().to_string(); // after potential redirects
        let html = res.text().await?;

        let looks_like_shell = is_shell_like(&html);
        if looks_like_shell {
            warn!(section = *section, "Section HTML looks like JS shell / interstitial; using fallbacks.");
        }

        let document = Html::parse_document(&html);

        // 1) Primary selectors commonly present on section pages
        let sel_title_link = Selector::parse(r#"a[data-testid="TitleLink"][href]"#).unwrap();
        let sel_heading_link = Selector::parse(r#"a[data-testid="Heading"][href]"#).unwrap();
        let sel_card_link = Selector::parse(r#"a[data-testid="Link"][href]"#).unwrap();
        let sel_generic_cards = Selector::parse(r#"article a[href]"#).unwrap();

        let mut urls = Vec::<String>::new();

        if !looks_like_shell {
            harvest_selector(&document, &sel_title_link, &mut urls);
            if urls.len() < 10 {
                harvest_selector(&document, &sel_heading_link, &mut urls);
            }
            if urls.len() < 10 {
                harvest_selector(&document, &sel_card_link, &mut urls);
            }
            if urls.len() < 10 {
                harvest_selector(&document, &sel_generic_cards, &mut urls);
            }

            // 2) JSON-LD ItemList fallback (list pages sometimes include this)
            if urls.len() < 10 {
                let mut from_ld = harvest_itemlist_jsonld(&document);
                from_ld.retain(|u| is_target_vertical(u));
                for u in from_ld {
                    if urls.len() >= 10 { break; }
                    if !urls.contains(&u) { urls.push(u); }
                }
            }

            // 3) Regex fallback for article-shaped hrefs (yyyy-mm-dd in slug)
            if urls.len() < 10 {
                let mut from_regex = harvest_regex_fallback(&html);
                from_regex.retain(|u| is_target_vertical(u));
                for u in from_regex {
                    if urls.len() >= 10 { break; }
                    if !urls.contains(&u) { urls.push(u); }
                }
            }

            // 4) Very liberal sweep over any <a href>, requiring a date in path
            if urls.len() < 10 {
                let sel_any_a = Selector::parse(r#"a[href]"#).unwrap();
                let re_date = regex::Regex::new(r"/20\d{2}-\d{2}-\d{2}").unwrap();

                for a in document.select(&sel_any_a) {
                    if let Some(href) = a.value().attr("href") {
                        if let Some(mut u) = normalize_reuters_link(href) {
                            if let Some(i) = u.find(['?', '#']) { u.truncate(i); }
                            if is_target_vertical(&u) && re_date.is_match(&u) && !urls.contains(&u) {
                                urls.push(u);
                                if urls.len() >= 10 { break; }
                            }
                        }
                    }
                }
            }
        }

        // 5) **RSS fallback** — reliable, server-rendered list of stories
        if urls.len() < 10 {
            if let Some(feed_url) = rss_url_for_section(*section) {
                match fetch_rss_links(feed_url).await {
                    Ok(mut feed_links) => {
                        feed_links.retain(|u| is_target_vertical(u));
                        for u in feed_links {
                            if urls.len() >= 10 { break; }
                            if !urls.contains(&u) { urls.push(u); }
                        }
                        info!(section = *section, rss = feed_url, added = urls.len(), "RSS fallback applied");
                    }
                    Err(e) => {
                        warn!(section = *section, error = %e, "RSS fallback failed");
                    }
                }
            } else {
                warn!(section = *section, "No RSS mapping for section; cannot apply RSS fallback");
            }
        }

        if urls.is_empty() {
            dump_section_debug(*section, &document, &html, &final_url);
        }

        info!(section = *section, count = urls.len(), "Indexed Reuters section URLs");
        debug!(?urls, "Section URLs");
        all.extend(urls);
    }

    all.sort();
    all.dedup();
    info!(total = all.len(), "Total indexed Reuters URLs");
    Ok(all)
}

fn is_shell_like(html: &str) -> bool {
    let len = html.len();
    let lc = html.to_lowercase();
    (len < 2000)
        || lc.contains("enable javascript")
        || lc.contains("consent")
        || lc.contains("unusual traffic")
        || lc.contains("pfnext") // platform shell marker sometimes present
        || lc.contains("arc-sw.js")
}

fn harvest_selector(document: &Html, sel: &Selector, urls: &mut Vec<String>) {
    for a in document.select(sel) {
        if urls.len() >= 10 { break; }
        if let Some(href) = a.value().attr("href") {
            if let Some(mut url) = normalize_reuters_link(href) {
                if let Some(i) = url.find(['?', '#']) { url.truncate(i); }
                if is_target_vertical(&url) && !urls.contains(&url) {
                    urls.push(url);
                }
            }
        }
    }
}

/// Parse <script type="application/ld+json"> blocks for ItemList / list pages
fn harvest_itemlist_jsonld(document: &Html) -> Vec<String> {
    let mut out = Vec::<String>::new();
    let Ok(sel) = Selector::parse(r#"script[type="application/ld+json"]"#) else { return out; };
    for script in document.select(&sel) {
        if let Some(js) = script.first_child().and_then(|n| n.value().as_text()).map(|t| t.to_string()) {
            let txt = js.trim();
            if txt.is_empty() { continue; }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(txt) {
                collect_urls_from_ldjson_value(&v, &mut out);
            }
        }
    }
    out = out.into_iter().filter_map(|u| normalize_reuters_link(&u)).collect();
    out.sort();
    out.dedup();
    out.truncate(20);
    out
}

fn collect_urls_from_ldjson_value(v: &serde_json::Value, out: &mut Vec<String>) {
    use serde_json::Value::*;
    match v {
        Array(arr) => {
            for item in arr { collect_urls_from_ldjson_value(item, out); }
        }
        Object(map) => {
            if let Some(t) = map.get("@type").and_then(|x| x.as_str()) {
                if t.eq_ignore_ascii_case("ItemList") {
                    if let Some(items) = map.get("itemListElement") {
                        collect_urls_from_ldjson_value(items, out);
                    }
                }
            }
            if let Some(u) = map.get("url").and_then(|x| x.as_str()) { out.push(u.to_string()); }
            if let Some(id) = map.get("@id").and_then(|x| x.as_str()) { out.push(id.to_string()); }
            if let Some(item) = map.get("item") { collect_urls_from_ldjson_value(item, out); }
        }
        _ => {}
    }
}

/// Regex fallback for article-shaped hrefs (yyyy-mm-dd anywhere in path; no trailing slash required)
fn harvest_regex_fallback(html: &str) -> Vec<String> {
    use regex::Regex;
    let mut out = Vec::<String>::new();

    let re_href = Regex::new(r#""(https?://www\.reuters\.com/[^\s"']+|/[^\s"']+)""#).unwrap();
    let re_date = Regex::new(r"/20\d{2}-\d{2}-\d{2}").unwrap();

    for cap in re_href.captures_iter(html) {
        let mut href = cap.get(1).unwrap().as_str().to_string();
        if let Some(i) = href.find(['?', '#']) { href.truncate(i); }
        if let Some(u) = normalize_reuters_link(&href) {
            if (u.contains("/world/") || u.contains("/sustainability/") || u.contains("/technology/"))
                && re_date.is_match(&u)
            {
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

fn is_target_vertical(url: &str) -> bool {
    url.contains("/world/") || url.contains("/sustainability/") || url.contains("/technology/")
}

fn normalize_reuters_link(href: &str) -> Option<String> {
    if href.starts_with("https://www.reuters.com/") || href.starts_with("http://www.reuters.com/") {
        Some(href.to_string())
    } else if href.starts_with('/') {
        Some(format!("https://www.reuters.com{}", href))
    } else {
        None
    }
}

/// Map section URL → Reuters RSS feed
fn rss_url_for_section(section: &str) -> Option<&'static str> {
    match section {
        "https://www.reuters.com/world/" => Some("https://www.reuters.com/world/rss"),
        "https://www.reuters.com/sustainability/" => Some("https://www.reuters.com/sustainability/rss"),
        "https://www.reuters.com/technology/" => Some("https://www.reuters.com/technology/rss"),
        _ => None,
    }
}

/// Fetch a Reuters RSS feed and return up to 20 article URLs
async fn fetch_rss_links(feed_url: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let xml = CLIENT.get(feed_url).send().await?.text().await?;
    // Very lightweight parse: collect <link>…</link> under <item>
    // (Avoiding new deps; this is robust enough for Reuters)
    let mut out = Vec::<String>::new();
    let mut in_item = false;
    for line in xml.lines() {
        let l = line.trim();
        if l.starts_with("<item") { in_item = true; }
        if in_item && l.starts_with("<link>") && l.ends_with("</link>") {
            let href = l.trim_start_matches("<link>").trim_end_matches("</link>").trim();
            if let Some(u) = normalize_reuters_link(href) {
                out.push(u);
            }
        }
        if l.starts_with("</item>") { in_item = false; }
        if out.len() >= 30 { break; }
    }
    out.sort();
    out.dedup();
    out.truncate(20);
    Ok(out)
}

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

/// Fetch a single Reuters article
#[instrument(level = "info", skip_all, fields(%url))]
async fn fetch_article(url: &str) -> Result<Option<NewsArticle>, Box<dyn Error>> {
    // Basic sanity: only fetch Reuters articles in target verticals
    let parsed = Url::parse(url)?;
    if parsed.domain().unwrap_or_default() != "www.reuters.com" || !is_target_vertical(url) {
        warn!(%url, "Skipping non-target Reuters URL");
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
    info!(bytes = len, "Parsed Reuters article");

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

    // E) Textual fallbacks
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

fn dump_section_debug(section: &str, document: &Html, html: &str, final_url: &str) {
    let any_a = Selector::parse("a[href]").unwrap();
    let title_link = Selector::parse(r#"a[data-testid="TitleLink"][href]"#).unwrap();

    eprintln!("\n--- DEBUG: No URLs for section {section} ---");
    eprintln!("Fetched URL (after redirects): {final_url}");
    eprintln!("HTML length: {}", html.len());

    let count_title = document.select(&title_link).count();
    eprintln!(r#"Found a[data-testid="TitleLink"][href] count = {}"#, count_title);

    eprintln!("First ~40 hrefs:");
    for (i, a) in document.select(&any_a).take(40).enumerate() {
        if let Some(h) = a.value().attr("href") {
            eprintln!("[{i:02}] {h}");
        }
    }
    eprintln!("------------------------------------------\n");
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