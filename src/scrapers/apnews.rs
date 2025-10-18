use crate::models::NewsArticle;
use futures::stream::{self, StreamExt};
use reqwest::get;
use scraper::{Html, Selector};
use std::error::Error;
use tracing::{debug, error, info, instrument, warn};

/// Index AP News articles via Google search (last 24 hours)
#[instrument(level = "info")]
pub async fn index_articles() -> Result<Vec<String>, Box<dyn Error>> {
    let google_search_url = "https://www.google.com/search?q=site:apnews.com+inurl:article&hl=en&tbs=qdr:d&num=20";
    
    let html = get(google_search_url).await?.text().await?;
    let document = Html::parse_document(&html);
    
    // Google search results are in <a> tags with specific structure
    // We'll look for links that go to apnews.com/article/
    let link_selector = Selector::parse("a").unwrap();
    
    let mut article_urls = Vec::new();
    for element in document.select(&link_selector) {
        if let Some(href) = element.value().attr("href") {
            // Google wraps URLs in /url?q= format, or they might be direct links
            if href.contains("apnews.com/article/") {
                // Extract the actual URL from Google's wrapper
                let url = if href.starts_with("/url?q=") {
                    // Extract URL between q= and the next &
                    href.strip_prefix("/url?q=")
                        .and_then(|s| s.split('&').next())
                        .map(|s| urlencoding::decode(s).unwrap_or_default().to_string())
                } else if href.starts_with("https://apnews.com/article/") {
                    Some(href.to_string())
                } else {
                    None
                };
                
                if let Some(url) = url {
                    if !article_urls.contains(&url) {
                        article_urls.push(url);
                    }
                }
            }
        }
        
        if article_urls.len() >= 20 {
            break;
        }
    }
    
    info!(
        count = article_urls.len(),
        source = "Google search for apnews.com",
        "Indexed AP News article URLs"
    );
    debug!(urls = ?article_urls, "AP News URLs");
    
    Ok(article_urls)
}

/// Fetch all AP News articles concurrently
#[instrument(level = "info", skip_all)]
pub async fn fetch_articles(urls: Vec<String>) -> Vec<NewsArticle> {
    let articles: Vec<NewsArticle> = stream::iter(urls.clone())
        .then(|url: String| async move {
            match fetch_article(&url).await {
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
        .filter(|opt| std::future::ready(opt.is_some()))
        .map(|opt| opt.unwrap())
        .collect()
        .await;
    
    info!(count = articles.len(), "Fetched AP News article contents");
    articles
}

/// Fetch a single AP News article
#[instrument(level = "info", skip_all, fields(%url))]
async fn fetch_article(url: &str) -> Result<Option<NewsArticle>, Box<dyn Error>> {
    let body = get(url).await?.text().await?;
    let document = Html::parse_document(&body);
    
    let mut content = String::new();
    
    // Extract article content from RichTextStoryBody
    let article_selector = Selector::parse(".RichTextStoryBody, .RichTextBody")?;
    
    for element in document.select(&article_selector) {
        let text = element.text().collect::<Vec<_>>().join(" ");
        content.push_str(&text);
        content.push_str("\n");
    }
    
    // Extract publication date
    let date_selector = Selector::parse(".Page-dateModified")?;
    if let Some(date_element) = document.select(&date_selector).next() {
        let date_text = date_element.text().collect::<Vec<_>>().join(" ");
        content.insert_str(0, &format!("Published: {}\n\n", date_text.trim()));
    }
    
    let len = content.len();
    info!(bytes = len, "Parsed AP News article");
    
    Ok(Some(NewsArticle {
        source: url.to_string(),
        content,
    }))
}
