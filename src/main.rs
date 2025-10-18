use awful_aj::{config, config_dir, template};
use chrono::Local;
use clap::Parser;
use itertools::Itertools;
use std::error::Error;
use tracing::{debug, error, info, instrument, warn};
use tracing_subscriber::{fmt as tfmt, EnvFilter};

mod api;
mod cli;
mod models;
mod outputs;
mod scrapers;
mod utils;

use api::ask_with_backoff;
use cli::Cli;
use models::{AwfulNewsArticle, FrontPage, ImportantDate, ImportantTimeframe, NamedEntity};
use outputs::{indexes, json, markdown};
use utils::{ensure_writable_dir, log_and_quarantine, looks_truncated, time_of_day, truncate_for_log};

#[tokio::main]
#[instrument]
async fn main() -> Result<(), Box<dyn Error>> {
    // --- Tracing init ---
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tfmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .with_timer(tracing_subscriber::fmt::time::UtcTime::rfc_3339())
        .init();

    let start_time = std::time::Instant::now();
    info!("news_update starting up");

    // Parse CLI
    let args = Cli::parse();
    debug!(?args.json_output_dir, ?args.markdown_output_dir, "Parsed CLI arguments");

    // Early check: ensure JSON output dir is writable
    if let Err(e) = ensure_writable_dir(&args.json_output_dir).await {
        error!(
            path = %args.json_output_dir,
            error = %e,
            "JSON output directory is not writable (fix perms or choose a different path)"
        );
        return Err(e);
    }

    // ---- Index and fetch articles ----
    let cnn_urls = scrapers::cnn::index_articles().await?;
    let npr_urls = scrapers::npr::index_articles().await?;

    let cnn_articles = scrapers::cnn::fetch_articles(cnn_urls).await;
    let npr_articles = scrapers::npr::fetch_articles(npr_urls).await;

    let articles = vec![cnn_articles, npr_articles]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    info!(count = articles.len(), "Total articles to analyze");

    // ---- Load template & config ----
    let template = template::load_template("news_parser").await?;
    info!("Loaded template: news_parser");
    let conf_file = config_dir()?.join("config.yaml");
    let config_path = conf_file.to_str().expect("Not a valid config filename");
    let config = config::load_config(config_path).unwrap();
    info!(config_path, "Loaded configuration");
    
    // Wrap config and template in Arc for sharing across parallel tasks
    use std::sync::Arc;
    let config = Arc::new(config);
    let template = Arc::new(template);

    // ---- Build front page ----
    let local_date = Local::now().date_naive().to_string();
    let local_time = Local::now().time().to_string();
    let mut front_page = FrontPage {
        time_of_day: time_of_day(),
        local_time,
        local_date,
        articles: Vec::new(),
    };
    info!(time_of_day = %front_page.time_of_day, local_date = %front_page.local_date, local_time = %front_page.local_time, "FrontPage initialized");

    // ---- Analyze articles in parallel (8 at a time) ----
    use futures::stream::{self, StreamExt};
    const PARALLEL_BATCH_SIZE: usize = 8;
    
    let total_articles = articles.len();
    info!(parallel_batch_size = PARALLEL_BATCH_SIZE, "Starting parallel article processing");
    
    // Process articles concurrently
    let results: Vec<Option<AwfulNewsArticle>> = stream::iter(articles.iter().enumerate())
        .map(|(i, article)| {
            let config = Arc::clone(&config);
            let template = Arc::clone(&template);
            async move {
                debug!(index = i, source = %article.source, "Analyzing article");

                // First ask
                match ask_with_backoff(&config, &article.content, &template).await {
                    Ok(response_json) => {
                        // Quarantine + meta
                        log_and_quarantine(i, &response_json);

                        // Try parse
                        let mut parsed = serde_json::from_str::<AwfulNewsArticle>(&response_json);

                        // If the parse failed due to EOF (truncation), re-ask ONCE
                        if let Err(ref e) = parsed {
                            if looks_truncated(e) {
                                warn!(index = i, error = %e, "EOF while parsing; re-asking once");
                                match ask_with_backoff(&config, &article.content, &template).await {
                                    Ok(r2) => {
                                        log_and_quarantine(i, &r2);
                                        parsed = serde_json::from_str::<AwfulNewsArticle>(&r2);
                                    }
                                    Err(e2) => {
                                        warn!(index = i, error = %e2, "Re-ask failed; will skip article");
                                    }
                                }
                            }
                        }

                        match parsed {
                            Ok(mut awful_news_article) => {
                                awful_news_article.source = Some(article.source.clone());
                                awful_news_article.content = Some(article.content.clone());

                                // dedupe
                                awful_news_article.namedEntities = awful_news_article
                                    .namedEntities
                                    .into_iter()
                                    .unique_by(|e| e.name.clone())
                                    .collect::<Vec<NamedEntity>>();
                                awful_news_article.importantDates = awful_news_article
                                    .importantDates
                                    .into_iter()
                                    .unique_by(|e| e.descriptionOfWhyDateIsRelevant.clone())
                                    .collect::<Vec<ImportantDate>>();
                                awful_news_article.importantTimeframes = awful_news_article
                                    .importantTimeframes
                                    .into_iter()
                                    .unique_by(|e| e.descriptionOfWhyTimeFrameIsRelevant.clone())
                                    .collect::<Vec<ImportantTimeframe>>();
                                awful_news_article.keyTakeAways = awful_news_article
                                    .keyTakeAways
                                    .into_iter()
                                    .unique()
                                    .collect::<Vec<String>>();

                                info!(index = i, "Successfully processed article");
                                Some(awful_news_article)
                            }
                            Err(e) => {
                                warn!(
                                    index = i,
                                    error = %e,
                                    response_preview = %truncate_for_log(&response_json, 300),
                                    "Model returned non-conforming JSON; skipping article"
                                );
                                None
                            }
                        }
                    }
                    Err(e) => {
                        error!(index = i, source = %article.source, error = %e, "API call failed; skipping article");
                        None
                    }
                }
            }
        })
        .buffer_unordered(PARALLEL_BATCH_SIZE)
        .collect()
        .await;

    // Add successful results to front_page
    for result in results.into_iter().flatten() {
        front_page.articles.push(result);
    }
    
    info!(
        total = total_articles,
        successful = front_page.articles.len(),
        failed = total_articles - front_page.articles.len(),
        "Completed parallel article processing"
    );

    // Write final JSON after all articles processed
    if let Err(e) = json::write_frontpage(&front_page, &args.json_output_dir).await {
        error!(error = %e, "Failed to write final JSON");
    }

    // ---- Markdown output ----
    let md = markdown::front_page_to_markdown(&front_page);
    let output_markdown_filename = format!(
        "{}/{}_{}.md",
        args.markdown_output_dir, front_page.local_date, front_page.time_of_day
    );

    info!(path = %output_markdown_filename, "Writing Markdown");
    if let Err(e) = tokio::fs::write(&output_markdown_filename, md).await {
        error!(path = %output_markdown_filename, error = %e, "Failed writing Markdown");
    } else {
        info!(path = %output_markdown_filename, "Wrote FrontPage Markdown");
    }

    // ---- Index updates ----
    let markdown_filename = format!("{}_{}.md", front_page.local_date, front_page.time_of_day);
    
    if let Err(e) = indexes::update_date_toc_file(
        &args.markdown_output_dir,
        &front_page,
        &markdown_filename,
    )
    .await
    {
        error!(error = %e, "Failed to update date TOC file");
    }

    if let Err(e) = indexes::update_summary_md(
        &args.markdown_output_dir,
        &front_page,
        &markdown_filename,
    )
    .await
    {
        error!(error = %e, "Failed to update SUMMARY.md");
    }

    if let Err(e) = indexes::update_daily_news_index(
        &args.markdown_output_dir,
        &front_page,
        &markdown_filename,
    )
    .await
    {
        error!(error = %e, "Failed to update daily_news.md index");
    }

    let elapsed = start_time.elapsed();
    info!(
        ?elapsed,
        secs = elapsed.as_secs(),
        millis = elapsed.subsec_millis(),
        "Execution complete"
    );
    Ok(())
}
