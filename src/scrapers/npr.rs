use crate::models::NewsArticle;
use futures::stream::{self, StreamExt};
use reqwest::get;
use scraper::{Html, Selector};
use std::error::Error;
use tracing::{debug, error, info, instrument, warn};
use url::Url;

/// Index NPR Text homepage to extract article URLs
#[instrument(level = "info")]
pub async fn index_articles() -> Result<Vec<String>, Box<dyn Error>> {
    let npr_page_url = "https://text.npr.org";
    let npr_base_url = Url::parse(npr_page_url)?;

    let html = get(npr_page_url).await?.text().await?;
    let document = Html::parse_document(&html);
    let story_selector = Selector::parse(".topic-title").unwrap();
    
    let mut article_urls = Vec::new();
    for element in document.select(&story_selector) {
        if let Some(href) = element.value().attr("href") {
            if let Ok(resolved) = npr_base_url.join(href) {
                article_urls.push(resolved.to_string());
            }
        }
    }
    
    info!(
        count = article_urls.len(),
        source = npr_page_url,
        "Indexed NPR article URLs"
    );
    debug!(urls = ?article_urls, "NPR URLs");
    
    Ok(article_urls)
}

/// Fetch all NPR articles concurrently
#[instrument(level = "info", skip_all)]
pub async fn fetch_articles(urls: Vec<String>) -> Vec<NewsArticle> {
    let articles: Vec<NewsArticle> = stream::iter(urls.clone())
        .then(|url: String| async move {
            match fetch_article(&url).await {
                Ok(Some(article)) => {
                    debug!(%url, "Fetched NPR article");
                    Some(article)
                }
                Ok(None) => {
                    warn!(%url, "NPR fetch produced no content");
                    None
                }
                Err(e) => {
                    error!(error = %e, %url, "NPR fetch failed");
                    None
                }
            }
        })
        .filter(|opt| std::future::ready(opt.is_some()))
        .map(|opt| opt.unwrap())
        .collect()
        .await;
    
    info!(count = articles.len(), "Fetched NPR article contents");
    articles
}

/// Fetch a single NPR article
#[instrument(level = "info", skip_all, fields(%url))]
async fn fetch_article(url: &str) -> Result<Option<NewsArticle>, Box<dyn Error>> {
    let body = get(url).await?.text().await?;
    let document = Html::parse_document(&body);

    let mut content = String::new();
    let headline_selector = Selector::parse(".story-head")?;
    let article_selector = Selector::parse(".paragraphs-container")?;

    for element in document
        .select(&headline_selector)
        .chain(document.select(&article_selector))
    {
        let text = element.text().collect::<Vec<_>>().join(" ");
        content.push_str(&text);
        content.push_str("\n");
    }

    let len = content.len();
    info!(bytes = len, "Parsed NPR article");
    Ok(Some(NewsArticle {
        source: url.to_string(),
        content,
    }))
}
