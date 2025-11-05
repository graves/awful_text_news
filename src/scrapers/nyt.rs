use crate::models::NewsArticle;
use futures::stream::{self, StreamExt};
use once_cell::sync::Lazy;
use reqwest::Client;
use scraper::{Html, Selector};
use serde::Deserialize;
use std::error::Error;
use std::time::Duration;
use tracing::{debug, error, info, instrument, warn};

// Global HTTP client with realistic UA + timeouts
static CLIENT: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .user_agent(concat!(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) ",
            "AppleWebKit/537.36 (KHTML, like Gecko) ",
            "Chrome/127.0.0.0 Safari/537.36"
        ))
        .timeout(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .expect("failed to build reqwest client")
});

#[derive(Debug, Deserialize)]
struct NYTimesResponse {
    results: Vec<NYTimesArticle>,
}

#[derive(Debug, Deserialize)]
struct NYTimesArticle {
    url: String,
    title: String,
}

/// Index NYT articles via their Top Stories API
#[instrument(level = "info")]
pub async fn index_articles(api_key: Option<&str>) -> Result<Vec<(String, String)>, Box<dyn Error>> {
    let api_key = match api_key {
        Some(key) => key,
        None => {
            warn!("No NYT API key provided, skipping NYT articles");
            return Ok(Vec::new());
        }
    };
    
    let api_url = format!(
        "https://api.nytimes.com/svc/topstories/v2/home.json?api-key={}",
        api_key
    );

    info!("Fetching NYT top stories from API");
    
    let response = CLIENT.get(&api_url).send().await?;
    
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await?;
        error!(status = %status, body = %body, "NYT API request failed");
        return Err(format!("NYT API returned status {}: {}", status, body).into());
    }

    let nyt_response: NYTimesResponse = response.json().await?;
    
    // Take first 30 URLs and titles
    let articles: Vec<(String, String)> = nyt_response
        .results
        .into_iter()
        .take(30)
        .map(|article| (article.url, article.title))
        .collect();

    info!(
        count = articles.len(),
        source = "NYT Top Stories API",
        "Indexed NYT article URLs and titles"
    );
    debug!(articles = ?articles, "NYT URLs and titles");

    Ok(articles)
}

/// Fetch all NYT articles concurrently through removepaywalls.com
#[instrument(level = "info", skip_all)]
pub async fn fetch_articles(articles: Vec<(String, String)>) -> Vec<NewsArticle> {
    let concurrency = 4usize; // Lower concurrency to be respectful to removepaywalls.com

    let articles: Vec<NewsArticle> = stream::iter(articles.into_iter())
        .map(|(url, api_title)| async move {
            let res = fetch_article(&url, &api_title).await;
            (url, res)
        })
        .buffer_unordered(concurrency)
        .filter_map(|(url, res)| async move {
            match res {
                Ok(Some(article)) => {
                    debug!(%url, "Fetched NYT article");
                    Some(article)
                }
                Ok(None) => {
                    warn!(%url, "NYT fetch produced no content");
                    None
                }
                Err(e) => {
                    error!(error = %e, %url, "NYT fetch failed");
                    None
                }
            }
        })
        .collect()
        .await;

    info!(count = articles.len(), "Fetched NYT article contents");
    articles
}

/// Fetch a single NYT article through accessarticlenow.com (the iframe backend)
#[instrument(level = "info", skip_all, fields(%url))]
async fn fetch_article(url: &str, api_title: &str) -> Result<Option<NewsArticle>, Box<dyn Error>> {
    // Construct the accessarticlenow.com URL (this is what removepaywalls.com uses in its iframe)
    let proxy_url = format!("https://accessarticlenow.com/api/c/google?q={}", url);
    
    info!(%proxy_url, "Fetching through accessarticlenow.com");
    
    let body = CLIENT.get(&proxy_url).send().await?.text().await?;
    let document = Html::parse_document(&body);

    // Extract title
    let title_selector = Selector::parse(r#"h1[data-testid="headline"]"#)
        .or_else(|_| Selector::parse("h1.css-88wicj"))
        .or_else(|_| Selector::parse("h1"))
        .unwrap();
    
    let scraped_title = document
        .select(&title_selector)
        .next()
        .map(|el| el.text().collect::<String>().trim().to_string())
        .unwrap_or_default();

    // Use API title as fallback if scraped title is empty or blank
    let title = if scraped_title.is_empty() || scraped_title.trim().is_empty() {
        debug!(api_title = %api_title, "Using API title as fallback (scraped title was blank)");
        api_title.to_string()
    } else {
        scraped_title
    };

    debug!(title = %title, "Final title");

    // Extract published date
    let time_selector = Selector::parse(r#"time[datetime]"#).unwrap();
    let published_date = if let Some(el) = document.select(&time_selector).next() {
        if let Some(datetime) = el.value().attr("datetime") {
            datetime.to_string()
        } else {
            // Fallback: try to get the text content of the time element
            el.text()
                .collect::<String>()
                .trim()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        }
    } else {
        "Date not found".to_string()
    };

    debug!(published_date = %published_date, "Extracted published date");

    // Add title and date at the top
    let mut content = String::new();
    content.push_str(&format!("# {}\n\n", title));
    content.push_str(&format!("Published: {}\n\n", published_date));

    // Try multiple strategies to extract article body
    let mut paragraphs_found = 0;
    
    // Strategy 1: Look for section[name="articleBody"]
    if let Ok(selector) = Selector::parse(r#"section[name="articleBody"]"#) {
        if let Some(article_section) = document.select(&selector).next() {
            debug!("Found section[name='articleBody']");
            if let Ok(p_selector) = Selector::parse("p") {
                for paragraph in article_section.select(&p_selector) {
                    let text = paragraph
                        .text()
                        .collect::<String>()
                        .trim()
                        .to_string();
                    
                    if !text.is_empty() && text.len() > 10 {
                        content.push_str(&text);
                        content.push_str("\n\n");
                        paragraphs_found += 1;
                    }
                }
            }
        }
    }
    
    // Strategy 2: If no paragraphs found, try .StoryBodyCompanionColumn
    if paragraphs_found == 0 {
        if let Ok(selector) = Selector::parse(".StoryBodyCompanionColumn") {
            debug!("Trying .StoryBodyCompanionColumn");
            if let Ok(p_selector) = Selector::parse("p") {
                for container in document.select(&selector) {
                    for paragraph in container.select(&p_selector) {
                        let text = paragraph
                            .text()
                            .collect::<String>()
                            .trim()
                            .to_string();
                        
                        if !text.is_empty() && text.len() > 10 {
                            content.push_str(&text);
                            content.push_str("\n\n");
                            paragraphs_found += 1;
                        }
                    }
                }
            }
        }
    }
    
    // Strategy 3: If still no paragraphs, try any p tag with css-ac37hb class
    if paragraphs_found == 0 {
        if let Ok(selector) = Selector::parse("p.css-ac37hb, p.evys1bk0") {
            debug!("Trying p.css-ac37hb");
            for paragraph in document.select(&selector) {
                let text = paragraph
                    .text()
                    .collect::<String>()
                    .trim()
                    .to_string();
                
                if !text.is_empty() && text.len() > 10 {
                    content.push_str(&text);
                    content.push_str("\n\n");
                    paragraphs_found += 1;
                }
            }
        }
    }
    
    // Strategy 4: Last resort - try all <p> tags in the document
    if paragraphs_found == 0 {
        if let Ok(selector) = Selector::parse("p") {
            debug!("Trying all p tags");
            for paragraph in document.select(&selector) {
                let text = paragraph
                    .text()
                    .collect::<String>()
                    .trim()
                    .to_string();
                
                // More strict filtering for all p tags to avoid navigation/footer text
                if !text.is_empty() && text.len() > 50 {
                    content.push_str(&text);
                    content.push_str("\n\n");
                    paragraphs_found += 1;
                }
            }
        }
    }
    
    debug!(paragraphs_found, "Extracted paragraphs");

    let len = content.len();
    info!(bytes = len, "Parsed NYT article");

    if len > 200 {
        // Ensure we have substantial content
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
