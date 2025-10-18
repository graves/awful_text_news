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

/// Scrape up to 60 articles total (20 per section)
const SECTION_URLS: &[&str] = &[
    "https://www.aljazeera.com/climate-crisis",
    "https://www.aljazeera.com/tag/science-and-technology/",
    "https://www.aljazeera.com/news/",
];

/// Index Al Jazeera articles (top 20 from each section; de-duped)
#[instrument(level = "info")]
pub async fn index_articles() -> Result<Vec<String>, Box<dyn Error>> {
    let mut all = Vec::<String>::new();

    for section in SECTION_URLS {
        let res = CLIENT.get(*section).send().await?;
        let final_url = res.url().to_string(); // after potential redirects
        let html = res.text().await?;
        let document = Html::parse_document(&html);

        // 1) Primary selectors commonly present on AJ list pages
        //    Example you shared: <a class="u-clickable-card__link article-card__link" href="/news/...">
        let sel_card_link = Selector::parse(r#"a.u-clickable-card__link.article-card__link[href]"#).unwrap();
        // Also collect any obvious article-card titles that wrap anchors
        let sel_title_link = Selector::parse(r#"h3.article-card__title"#).unwrap();
        // Generic anchor fallback on list cards
        let sel_any_a = Selector::parse(r#"article a[href], div a[href]"#).unwrap();

        let mut urls = Vec::<String>::new();

        // Prefer explicit clickable-card links
        harvest_selector(&document, &sel_card_link, &mut urls);
        if urls.len() < 20 {
            // Some pages put the <h3> and the link on the same anchor; walk up to <a>
            for title in document.select(&sel_title_link) {
                if let Some(parent) = title.parent() {
                    if let Some(el) = ElementRef::wrap(parent) {
                        if el.value().name() == "a" {
                            if let Some(href) = el.value().attr("href") {
                                if let Some(url) = normalize_aljazeera_link(href) {
                                    if is_target_vertical(&url) && !urls.contains(&url) {
                                        urls.push(url);
                                    }
                                }
                            }
                        }
                    }
                }
                if urls.len() >= 20 { break; }
            }
        }
        if urls.len() < 20 {
            harvest_selector(&document, &sel_any_a, &mut urls);
        }

        // 2) JSON-LD ItemList fallback (when present)
        if urls.len() < 20 {
            let mut from_ld = harvest_itemlist_jsonld(&document);
            from_ld.retain(|u| is_target_vertical(u));
            for u in from_ld {
                if urls.len() >= 20 { break; }
                if !urls.contains(&u) {
                    urls.push(u);
                }
            }
        }

        // 3) Regex fallback for article-shaped hrefs
        if urls.len() < 20 {
            let mut from_regex = harvest_regex_fallback(&html);
            from_regex.retain(|u| is_target_vertical(u));
            for u in from_regex {
                if urls.len() >= 20 { break; }
                if !urls.contains(&u) {
                    urls.push(u);
                }
            }
        }

        if urls.is_empty() {
            dump_section_debug(*section, &document, &html, &final_url);
        }

        info!(section = *section, count = urls.len(), "Indexed Al Jazeera section URLs");
        debug!(?urls, "Section URLs");
        all.extend(urls);
    }

    all.sort();
    all.dedup();
    // Cap at 60 just in case
    if all.len() > 60 {
        all.truncate(60);
    }
    info!(total = all.len(), "Total indexed Al Jazeera URLs");
    Ok(all)
}

fn harvest_selector(document: &Html, sel: &Selector, urls: &mut Vec<String>) {
    for a in document.select(sel) {
        if urls.len() >= 20 {
            break;
        }
        if let Some(href) = a.value().attr("href") {
            if let Some(url) = normalize_aljazeera_link(href) {
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
    // normalize, dedupe
    out = out.into_iter().filter_map(|u| normalize_aljazeera_link(&u)).collect();
    out.sort();
    out.dedup();
    out.truncate(30);
    out
}

/// Recursively walk JSON-LD to pull URLs from ItemList/itemListElement entries
fn collect_urls_from_ldjson_value(v: &serde_json::Value, out: &mut Vec<String>) {
    use serde_json::Value::*;
    match v {
        Array(arr) => {
            for item in arr {
                collect_urls_from_ldjson_value(item, out);
            }
        }
        Object(map) => {
            if let Some(t) = map.get("@type").and_then(|x| x.as_str()) {
                if t.eq_ignore_ascii_case("ItemList") {
                    if let Some(items) = map.get("itemListElement") {
                        collect_urls_from_ldjson_value(items, out);
                    }
                }
            }
            if let Some(u) = map.get("url").and_then(|x| x.as_str()) {
                out.push(u.to_string());
            }
            if let Some(id) = map.get("@id").and_then(|x| x.as_str()) {
                out.push(id.to_string());
            }
            if let Some(item) = map.get("item") {
                collect_urls_from_ldjson_value(item, out);
            }
        }
        _ => {}
    }
}

/// Regex fallback for article-looking hrefs like “…/news/YYYY/MM/DD/slug…”
fn harvest_regex_fallback(html: &str) -> Vec<String> {
    let mut out = Vec::<String>::new();
    // Capture relative or absolute links under /news/ or climate/tag pages that contain a date structure
    let re = regex::Regex::new(
        r#""(https?://www\.aljazeera\.com/[^\s"']+|/[^\s"']+)""#
    ).unwrap();

    for cap in re.captures_iter(html) {
        let href = cap.get(1).unwrap().as_str();
        if href.contains("/news/20") || href.contains("/news/202") {
            if let Some(u) = normalize_aljazeera_link(href) {
                out.push(u);
            }
        }
        if out.len() >= 50 { break; }
    }
    out.sort();
    out.dedup();
    out.truncate(30);
    out
}

fn is_target_vertical(url: &str) -> bool {
    // Most article pages ultimately live under /news/YYYY/MM/DD/...; still, keep section roots
    url.starts_with("https://www.aljazeera.com/news/")
        || url.starts_with("https://www.aljazeera.com/climate-crisis/")
        || url.starts_with("https://www.aljazeera.com/tag/science-and-technology/")
        || (url.starts_with("https://www.aljazeera.com/news/")
            || (url.contains("/news/20") || url.contains("/news/202")))
}

fn normalize_aljazeera_link(href: &str) -> Option<String> {
    if href.starts_with("https://www.aljazeera.com/") || href.starts_with("http://www.aljazeera.com/") {
        Some(href.to_string())
    } else if href.starts_with('/') {
        Some(format!("https://www.aljazeera.com{}", href))
    } else {
        None
    }
}

/// Fetch all Al Jazeera articles concurrently
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
                    debug!(%url, "Fetched Al Jazeera article");
                    Some(article)
                }
                Ok(None) => {
                    warn!(%url, "Al Jazeera fetch produced no content");
                    None
                }
                Err(e) => {
                    error!(error = %e, %url, "Al Jazeera fetch failed");
                    None
                }
            }
        })
        .collect()
        .await;

    info!(count = articles.len(), "Fetched Al Jazeera article contents");
    articles
}

/// Fetch a single Al Jazeera article
#[instrument(level = "info", skip_all, fields(%url))]
async fn fetch_article(url: &str) -> Result<Option<NewsArticle>, Box<dyn Error>> {
    // Basic sanity: only fetch aljazeera.com pages and prefer canonical article URLs
    let parsed = Url::parse(url)?;
    if parsed.domain().unwrap_or_default() != "www.aljazeera.com" {
        warn!(%url, "Skipping non-aljazeera domain");
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
    // Al Jazeera commonly: og:title or h1[aria-label="headline"] or plain h1
    let title = meta_content(&document, r#"meta[property="og:title"]"#, "content")
        .or_else(|| text_of_first(&document, r#"h1"#))
        .unwrap_or_default();

    // ----- CONTENT EXTRACTION -----
    // Modern AJ articles:
    //   - main article body paragraphs often under `div.wysiwyg` or `.article-p-wrapper`
    // Fallbacks:
    //   - article p, main p
    let candidates = [
        r#"div.wysiwyg p"#,
        r#"div.article-p-wrapper p"#,
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
    info!(bytes = len, "Parsed Al Jazeera article");

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
    // A) JSON-LD blocks (Al Jazeera uses NewsArticle schema frequently)
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

    // B) Common meta variants
    for css in &[
        r#"meta[property="article:published_time"]"#,
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

    // D) Textual fallbacks (e.g., "Published On 18 Oct 2025")
    if let Ok(sel) = Selector::parse(".gc__date__date .date-simple, [class*=\"date\"], time") {
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
        if let Some(raw) = obj.get("datePublished").and_then(|x| x.as_str()).map(|s| s.to_string()) {
            return Some((raw.clone(), raw));
        }
    }
    None
}

/* -------------------- DEBUG (optional) -------------------- */

fn dump_section_debug(section: &str, document: &Html, html: &str, final_url: &str) {
    let any_a = Selector::parse("a[href]").unwrap();
    let card_link = Selector::parse(r#"a.u-clickable-card__link.article-card__link[href]"#).unwrap();

    eprintln!("\n--- DEBUG: No URLs for section {section} ---");
    eprintln!("Fetched URL (after redirects): {final_url}");
    eprintln!("HTML length: {}", html.len());

    let count_card = document.select(&card_link).count();
    eprintln!(r#"Found a.u-clickable-card__link.article-card__link[href] count = {}"#, count_card);

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