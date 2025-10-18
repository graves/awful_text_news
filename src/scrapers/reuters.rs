use crate::models::NewsArticle;
use futures::stream::{self, StreamExt};
use once_cell::sync::Lazy;
use quick_xml::events::Event;
use quick_xml::escape::unescape;
use quick_xml::Reader;
use reqwest::{redirect, Client, Url};
use scraper::{Html, Selector};
use std::error::Error;
use std::time::Duration;
use tracing::{debug, error, info, warn};

static CLIENT: Lazy<Client> = Lazy::new(|| {
    use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE, REFERER, USER_AGENT};
    let mut h = HeaderMap::new();
    h.insert(USER_AGENT, HeaderValue::from_static(
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
         (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36"
    ));
    h.insert(ACCEPT, HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"));
    h.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.9"));
    h.insert(REFERER, HeaderValue::from_static("https://news.google.com/"));

    Client::builder()
        .default_headers(h)
        .timeout(Duration::from_secs(20))
        .pool_idle_timeout(Duration::from_secs(10))
        .redirect(redirect::Policy::limited(10))
        .build()
        .expect("failed to build reqwest client")
});

const GNEWS_FEEDS: &[&str] = &[
    "https://news.google.com/rss/search?q=site%3Areuters.com%2Fworld%2F&hl=en-US&gl=US&ceid=US%3Aen",
    "https://news.google.com/rss/search?q=site%3Areuters.com%2Fsustainability%2F&hl=en-US&gl=US&ceid=US%3Aen",
    "https://news.google.com/rss/search?q=site%3Areuters.com%2Ftechnology%2F&hl=en-US&gl=US&ceid=US%3Aen",
];

/// Index Reuters article URLs via Google News only (no reuters.com section hits)
pub async fn index_articles() -> Result<Vec<String>, Box<dyn Error>> {
    let mut all = Vec::<String>::new();

    for feed in GNEWS_FEEDS {
        let links = fetch_gnews_item_links(feed).await?;
        let before = all.len();

        // Resolve Google redirects concurrently; we only read the final URL
        let resolved: Vec<String> = stream::iter(links.into_iter())
            .map(|g| async move { resolve_gnews_redirect(&g).await })
            .buffer_unordered(12)
            .filter_map(|r| async move { r.ok() })
            .collect()
            .await;

        for mut u in resolved {
            if let Some(i) = u.find(['?', '#']) { u.truncate(i); }
            if u.starts_with("https://www.reuters.com/")
                && (u.contains("/world/") || u.contains("/sustainability/") || u.contains("/technology/"))
            {
                all.push(u);
            }
        }

        let added = all.len() - before;
        info!(rss = *feed, added, "GNews-only indexing");
    }

    all.sort();
    all.dedup();
    info!(total = all.len(), "Total Reuters URLs (via GNews only)");
    if all.is_empty() {
        warn!("GNews-only path returned 0 URLs — check connectivity to news.google.com and redirect policy.");
    }
    Ok(all)
}

/* -------------------- GNEWS RSS HELPERS -------------------- */

async fn fetch_gnews_item_links(feed_url: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let xml = CLIENT.get(feed_url).send().await?.text().await?;
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut in_link = false;
    let mut links = Vec::<String>::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().as_ref().eq_ignore_ascii_case(b"link") => {
                in_link = true;
            }
            Ok(Event::End(e)) if e.name().as_ref().eq_ignore_ascii_case(b"link") => {
                in_link = false;
            }
            Ok(Event::Text(t)) if in_link => {
                let raw = std::str::from_utf8(t.as_ref())?;         // BytesText -> &str
                let s = quick_xml::escape::unescape(raw)?.into_owned();
                if s.starts_with("https://news.google.com/rss/articles/")
                    || s.starts_with("https://news.google.com/articles/") {
                    links.push(s);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                warn!(error = %e, "XML parse error; returning partial results");
                break;
            }
            _ => {}
        }
        buf.clear();
    }

    links.truncate(40);
    Ok(links)
}

async fn resolve_gnews_redirect(gnews_link: &str) -> Result<String, Box<dyn Error>> {
    // Follow redirects; we don't need to read the body
    let res = CLIENT.get(gnews_link).send().await?;
    Ok(res.url().to_string())
}

/* -------------------- FETCH CONTENT (kept for pipeline compatibility) -------------------- */

/// Fetch all Reuters articles (from already-indexed URLs)
pub async fn fetch_articles(urls: Vec<String>) -> Vec<NewsArticle> {
    let concurrency = 8usize;

    stream::iter(urls.into_iter())
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
        .await
}

async fn fetch_article(url: &str) -> Result<Option<NewsArticle>, Box<dyn Error>> {
    // sanity
    let parsed = Url::parse(url)?;
    if parsed.domain().unwrap_or_default() != "www.reuters.com" {
        warn!(%url, "Skipping non-Reuters URL");
        return Ok(None);
    }

    // Light-weight body fetch + simple extraction (no section-page scraping)
    let body = CLIENT.get(url).send().await?.text().await?;
    let document = Html::parse_document(&body);

    let candidates = [
        r#"div[data-testid="article-body"] p"#,
        r#"article p[data-testid^="paragraph-"]"#,
        r#"article p"#,
    ];

    let title = text_of_first(&document, "h1")
        .or_else(|| meta_content(&document, r#"meta[property="og:title"]"#, "content"))
        .unwrap_or_default();

    let mut content = String::new();
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
            break;
        }
    }

    if !title.is_empty() && !content.is_empty() {
        content = format!("Title: {}\n\n{}", title, content);
    }

    if content.is_empty() {
        debug!(
            preview = %body.chars().take(600).collect::<String>().replace('\n', " "),
            "No article content parsed; HTML preview"
        );
        Ok(None)
    } else {
        Ok(Some(NewsArticle {
            source: url.to_string(),
            content,
        }))
    }
}

/* -------------------- SMALL HELPERS -------------------- */

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