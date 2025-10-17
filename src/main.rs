use awful_aj::api::ask;
use awful_aj::config_dir;
use awful_aj::template;
use awful_aj::{config, config::AwfulJadeConfig, template::ChatTemplate};
use chrono::{Duration, Local, NaiveTime};
use clap::Parser;
use futures::stream::{self, StreamExt};
use itertools::Itertools;
use reqwest::get;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::fmt::Write;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::time::Instant;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{debug, error, info, instrument, warn};
use tracing_subscriber::{EnvFilter, fmt as tfmt};
use url::Url;

// Std FS for quarantine writes
use std::fs as stdfs;
use std::io::Write as IoWrite;

// ===== CLI & Data types =====

/// Main program to scrape and analyze news articles
/// from CNN and NPR, outputting JSON/API files and markdown reports.
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    /// Output directory for the JSON API file
    #[arg(short, long)]
    json_output_dir: String,

    /// Output directory for the Markdown file
    #[arg(short, long)]
    markdown_output_dir: String,
}

#[derive(Debug)]
struct NewsArticle {
    source: String,
    content: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FrontPage {
    local_date: String,
    time_of_day: String,
    local_time: String,
    articles: Vec<AwfulNewsArticle>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AwfulNewsArticle {
    pub source: Option<String>,
    pub dateOfPublication: String,
    pub timeOfPublication: String,
    pub title: String,
    pub summaryOfNewsArticle: String,
    pub keyTakeAways: Vec<String>,
    pub namedEntities: Vec<NamedEntity>,
    pub importantDates: Vec<ImportantDate>,
    pub importantTimeframes: Vec<ImportantTimeframe>,
    pub content: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct NamedEntity {
    pub name: String,
    pub whatIsThisEntity: String,
    pub whyIsThisEntityRelevantToTheArticle: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ImportantDate {
    pub dateMentionedInArticle: String,
    pub descriptionOfWhyDateIsRelevant: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ImportantTimeframe {
    pub approximateTimeFrameStart: String,
    pub approximateTimeFrameEnd: String,
    pub descriptionOfWhyTimeFrameIsRelevant: String,
}

// ===== Main =====

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
        // Fail fast: we won't be able to write incremental JSON snapshots anyway.
        return Err(e);
    }

    // Base URLs
    let cnn_page_url = "https://lite.cnn.com";
    let cnn_base_url = Url::parse(cnn_page_url).expect("Invalid base URL");
    let npr_page_url = "https://text.npr.org";
    let npr_base_url = Url::parse(npr_page_url).expect("Invalid base URL");

    // ---- Index CNN ----
    let cnn_html = get(cnn_page_url).await?.text().await?;
    let cnn_document = Html::parse_document(&cnn_html);
    let cnn_story_selector = Selector::parse(".card--lite a[href]").unwrap();
    let mut cnn_article_urls = Vec::new();
    for element in cnn_document.select(&cnn_story_selector) {
        if let Some(href) = element.value().attr("href") {
            if let Ok(resolved) = cnn_base_url.join(href) {
                cnn_article_urls.push(resolved.to_string());
            }
        }
    }
    info!(
        count = cnn_article_urls.len(),
        source = cnn_page_url,
        "Indexed CNN article URLs"
    );
    debug!(urls = ?cnn_article_urls, "CNN URLs");

    // ---- Index NPR ----
    let npr_html = get(npr_page_url).await?.text().await?;
    let npr_document = Html::parse_document(&npr_html);
    let npr_story_selector = Selector::parse(".topic-title").unwrap();
    let mut npr_article_urls = Vec::new();
    for element in npr_document.select(&npr_story_selector) {
        if let Some(href) = element.value().attr("href") {
            if let Ok(resolved) = npr_base_url.join(href) {
                npr_article_urls.push(resolved.to_string());
            }
        }
    }
    info!(
        count = npr_article_urls.len(),
        source = npr_page_url,
        "Indexed NPR article URLs"
    );
    debug!(urls = ?npr_article_urls, "NPR URLs");

    // ---- Fetch CNN articles ----
    let cnn_articles: Vec<NewsArticle> = stream::iter(cnn_article_urls.clone())
        .then(|url: String| async move {
            match fetch_cnn_article(&url).await {
                Ok(Some(article)) => {
                    debug!(%url, "Fetched CNN article");
                    Some(article)
                }
                Ok(None) => {
                    warn!(%url, "CNN fetch produced no content");
                    None
                }
                Err(e) => {
                    error!(error = %e, %url, "CNN fetch failed");
                    None
                }
            }
        })
        .filter(|opt| std::future::ready(opt.is_some()))
        .map(|opt| opt.unwrap())
        .collect()
        .await;
    info!(count = cnn_articles.len(), "Fetched CNN article contents");

    // ---- Fetch NPR articles ----
    let npr_articles: Vec<NewsArticle> = stream::iter(npr_article_urls.clone())
        .then(|url: String| async move {
            match fetch_npr_article(&url).await {
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
    info!(count = npr_articles.len(), "Fetched NPR article contents");

    // ---- Combine ----
    let articles = vec![cnn_articles, npr_articles]
        .into_iter()
        .flatten()
        .collect::<Vec<NewsArticle>>();
    info!(count = articles.len(), "Total articles to analyze");

    // ---- Load template & config ----
    let template = template::load_template("news_parser").await?;
    info!("Loaded template: news_parser");
    let conf_file = config_dir()?.join("config.yaml");
    let config_path = conf_file.to_str().expect("Not a valid config filename");
    let config = config::load_config(config_path).unwrap();
    info!(config_path, "Loaded configuration");

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

    // ---- Analyze each article ----
    let mut processed_count = 0usize;
    for (i, article) in articles.iter().enumerate() {
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

                        front_page.articles.push(awful_news_article);
                        info!(
                            index = i,
                            total_articles = front_page.articles.len(),
                            "Added analyzed article to FrontPage"
                        );

                        // Persist incremental JSON
                        let json = match serde_json::to_string(&front_page) {
                            Ok(j) => j,
                            Err(e) => {
                                error!(error = %e, index = i, "Failed to serialize FrontPage after article");
                                processed_count += 1;
                                info!(
                                    processed = processed_count,
                                    total = articles.len(),
                                    "Progress"
                                );
                                continue;
                            }
                        };

                        let midnight = NaiveTime::from_hms_opt(23, 59, 59).unwrap();
                        let now = Local::now().time();
                        let yesterday = now - Duration::days(1);

                        let _api_file_dir =
                            if front_page.time_of_day == "evening" && (now >= midnight) {
                                yesterday.to_string()
                            } else {
                                front_page.local_date.clone()
                            };
                        // Note: we used to create a naked dir here; it wasn't used for writing.
                        // We keep only the real output dir below.

                        let full_json_dir =
                            if front_page.time_of_day == "evening" && (now >= midnight) {
                                format!("{}/{}", args.json_output_dir, yesterday.to_string())
                            } else {
                                format!("{}/{}", args.json_output_dir, front_page.local_date)
                            };

                        info!(%full_json_dir, "Ensuring JSON directory exists");
                        if let Err(e) = fs::create_dir_all(&full_json_dir).await {
                            error!(%full_json_dir, error = %e, "Failed to create JSON dir");
                        }

                        let output_json_filename =
                            if front_page.time_of_day == "evening" && (now >= midnight) {
                                format!("{}/{}.json", full_json_dir, yesterday.to_string())
                            } else {
                                format!("{}/{}.json", full_json_dir, front_page.time_of_day)
                            };

                        info!(path = %output_json_filename, "Writing JSON");
                        if let Err(e) = fs::write(&output_json_filename, json).await {
                            error!(path = %output_json_filename, error = %e, "Failed writing JSON file");
                        } else if processed_count == 0 {
                            info!(path = %output_json_filename, "Wrote initial JSON API file");
                        }
                    }
                    Err(e) => {
                        warn!(
                            index = i,
                            error = %e,
                            response_preview = %truncate_for_log(&response_json, 300),
                            "Model returned non-conforming JSON; skipping article"
                        );
                    }
                }
            }
            Err(e) => {
                error!(index = i, source = %article.source, error = %e, "API call failed; skipping article");
            }
        }

        processed_count += 1;
        info!(
            processed = processed_count,
            total = articles.len(),
            "Progress"
        );
    }

    // ---- Markdown output ----
    let midnight = NaiveTime::from_hms_opt(23, 59, 59).unwrap();
    let now = Local::now().time();
    let yesterday = now - Duration::days(1);

    let markdown = front_page_to_markdown(&front_page);
    let output_markdown_filename = if front_page.time_of_day == "evening" && (now >= midnight) {
        format!(
            "{}/{}_{}.md",
            args.markdown_output_dir,
            front_page.local_date,
            yesterday.to_string()
        )
    } else {
        format!(
            "{}/{}_{}.md",
            args.markdown_output_dir, front_page.local_date, front_page.time_of_day
        )
    };

    info!(path = %output_markdown_filename, "Writing Markdown");
    if let Err(e) = fs::write(&output_markdown_filename, markdown).await {
        error!(path = %output_markdown_filename, error = %e, "Failed writing Markdown");
    } else {
        info!(path = %output_markdown_filename, "Wrote FrontPage Markdown");
    }

    // ---- Index updates ----
    if let Err(e) = update_date_toc_file(
        &args.markdown_output_dir,
        &front_page,
        &format!("{}_{}.md", front_page.local_date, front_page.time_of_day),
    )
    .await
    {
        error!(error = %e, "Failed to update date TOC file");
    }

    if let Err(e) = update_summary_md(
        &args.markdown_output_dir,
        &front_page,
        &format!("{}_{}.md", front_page.local_date, front_page.time_of_day),
    )
    .await
    {
        error!(error = %e, "Failed to update SUMMARY.md");
    }

    if let Err(e) = update_daily_news_index(
        &args.markdown_output_dir,
        &front_page,
        &format!("{}_{}.md", front_page.local_date, front_page.time_of_day),
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

// ===== Helpers =====

fn truncate_for_log(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…(+{} bytes)", &s[..max], s.len() - max)
    }
}

/// Save raw API response to ./_debug/responses with a stable-ish fingerprint.
/// Always best-effort (errors ignored), but we log path + size.
fn log_and_quarantine(index: usize, body: &str) {
    if let Err(e) = stdfs::create_dir_all("./_debug/responses") {
        warn!(error = %e, "Failed to create quarantine dir");
        return;
    }
    let mut hasher = DefaultHasher::new();
    body.hash(&mut hasher);
    let fp = hasher.finish();
    let path = format!("./_debug/responses/resp_{index}_{fp:016x}.json");
    match stdfs::File::create(&path) {
        Ok(mut f) => {
            if let Err(e) = f.write_all(body.as_bytes()) {
                warn!(path = %path, error = %e, "Failed to write quarantined response");
            } else {
                info!(index, path = %path, bytes = body.len(), "Quarantined raw API response");
            }
        }
        Err(e) => warn!(path = %path, error = %e, "Failed to create quarantined response file"),
    }
}

/// Classify serde_json error category to detect truncation/EOF.
fn looks_truncated(e: &serde_json::Error) -> bool {
    use serde_json::error::Category;
    matches!(e.classify(), Category::Eof)
}

#[instrument]
pub fn time_of_day() -> String {
    let morning_low = NaiveTime::from_hms_opt(0, 00, 0).unwrap();
    let morning_high = NaiveTime::from_hms_opt(8, 00, 0).unwrap();
    let afternoon_low = NaiveTime::from_hms_opt(8, 00, 0).unwrap();
    let afternoon_high = NaiveTime::from_hms_opt(16, 00, 0).unwrap();
    let _evening_low = NaiveTime::from_hms_opt(16, 00, 0).unwrap();
    let _evening_high = NaiveTime::from_hms_opt(0, 00, 0).unwrap();

    let tod = Local::now().time();
    let which = if (tod >= morning_low) && (tod < morning_high) {
        "morning"
    } else if (tod >= afternoon_low) && (tod < afternoon_high) {
        "afternoon"
    } else {
        "evening"
    };
    debug!(%tod, %which, "Computed time_of_day");
    which.to_string()
}

#[instrument(level = "info", skip_all, fields(%url))]
async fn fetch_cnn_article(url: &str) -> Result<Option<NewsArticle>, Box<dyn Error>> {
    let body = get(url).await?.text().await?;
    let document = Html::parse_document(&body);
    let mut content = String::new();
    let headline_selector = Selector::parse(".headline--lite")?;
    let article_selector = Selector::parse(".article--lite")?;

    for element in document
        .select(&headline_selector)
        .chain(document.select(&article_selector))
    {
        let text = element.text().collect::<Vec<_>>().join(" ");
        content.push_str(&text);
        content.push_str("\n");
    }

    let len = content.len();
    info!(bytes = len, "Parsed CNN article");
    Ok(Some(NewsArticle {
        source: url.to_string(),
        content,
    }))
}

#[instrument(level = "info", skip_all, fields(%url))]
async fn fetch_npr_article(url: &str) -> Result<Option<NewsArticle>, Box<dyn Error>> {
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

#[instrument(level = "debug", skip_all)]
pub fn front_page_to_markdown(front_page: &FrontPage) -> String {
    let mut md = String::new();

    writeln!(md, "# Awful Times\n").unwrap();
    writeln!(md, "#### Edition published at {}\n", front_page.local_time).unwrap();

    for article in &front_page.articles {
        writeln!(md, "## {}\n", article.title).unwrap();
        if let Some(source) = &article.source {
            writeln!(md, "- [source]({})", source).unwrap();
        }
        writeln!(
            md,
            "- _Published: {} {}_\n",
            article.dateOfPublication, article.timeOfPublication
        )
        .unwrap();
        writeln!(md, "### Summary\n").unwrap();
        writeln!(md, "{}\n", article.summaryOfNewsArticle.trim()).unwrap();

        if !article.keyTakeAways.is_empty() {
            writeln!(md, "### Key Takeaways").unwrap();
            for takeaway in &article.keyTakeAways {
                writeln!(md, "  - {}", takeaway).unwrap();
            }
            writeln!(md).unwrap();
        }

        if !article.namedEntities.is_empty() {
            writeln!(md, "### Named Entities").unwrap();
            for entity in &article.namedEntities {
                writeln!(md, "- **{}**", entity.name).unwrap();
                writeln!(md, "    - {}", entity.whatIsThisEntity).unwrap();
                writeln!(md, "    - {}", entity.whyIsThisEntityRelevantToTheArticle).unwrap();
            }
            writeln!(md).unwrap();
        }

        if !article.importantDates.is_empty() {
            writeln!(md, "### Important Dates").unwrap();
            for date in &article.importantDates {
                writeln!(md, "  - **{}**", date.dateMentionedInArticle).unwrap();
                writeln!(md, "    - {}", date.descriptionOfWhyDateIsRelevant).unwrap();
            }
            writeln!(md).unwrap();
        }

        if !article.importantTimeframes.is_empty() {
            writeln!(md, "### Important Timeframes").unwrap();
            for timeframe in &article.importantTimeframes {
                writeln!(
                    md,
                    "  - **From _{}_ to _{}_**",
                    timeframe.approximateTimeFrameStart, timeframe.approximateTimeFrameEnd
                )
                .unwrap();
                writeln!(
                    md,
                    "    - {}",
                    timeframe.descriptionOfWhyTimeFrameIsRelevant
                )
                .unwrap();
            }
            writeln!(md).unwrap();
        }

        writeln!(md, "---\n").unwrap();
    }

    debug!(chars = md.len(), "Rendered Markdown length");
    md
}

fn slugify_title(title: &str) -> String {
    title
        .to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != ' ', "")
        .replace(' ', "-")
}

#[instrument(level = "info", skip_all, fields(%markdown_output_dir, date = %front_page.local_date, file = %markdown_filename))]
async fn update_date_toc_file(
    markdown_output_dir: &str,
    front_page: &FrontPage,
    markdown_filename: &str,
) -> Result<(), Box<dyn Error>> {
    let toc_path = format!("{}/{}.md", markdown_output_dir, front_page.local_date);
    let mut toc_md = String::new();

    if !Path::new(&toc_path).exists() {
        writeln!(
            toc_md,
            "# Editions published on {}\n",
            front_page.local_date
        )
        .unwrap();
    }

    writeln!(
        toc_md,
        "- [{}](./{})",
        upcase(&front_page.time_of_day),
        markdown_filename
    )
    .unwrap();

    for article in &front_page.articles {
        let slug = slugify_title(&article.title);
        writeln!(
            toc_md,
            "\t- [{}]({}#{})",
            article.title, markdown_filename, slug
        )
        .unwrap();
    }

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&toc_path)
        .await?;
    file.write_all(toc_md.as_bytes()).await?;
    info!(path = %toc_path, "Updated TOC file");
    Ok(())
}

fn upcase(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

#[instrument(level = "info", skip_all, fields(%markdown_output_dir, date = %front_page.local_date, file = %markdown_filename))]
async fn update_summary_md(
    markdown_output_dir: &str,
    front_page: &FrontPage,
    markdown_filename: &str,
) -> Result<(), Box<dyn Error>> {
    let summary_path = format!("{}/SUMMARY.md", markdown_output_dir);
    let mut summary = String::new();

    if Path::new(&summary_path).exists() {
        summary = fs::read_to_string(&summary_path).await?;
    } else {
        summary.push_str("# Summary\n\n[Home](./home.md)\n- [PGP](./pgp.md)\n- [Contact](./contact.md)\n- [Daily News](./daily_news.md)\n");
    }

    let date_heading = format!(
        "    - [{}](./{}.md)",
        front_page.local_date, front_page.local_date
    );
    let edition_heading = format!(
        "        - [{}](./{})",
        upcase(&front_page.time_of_day),
        markdown_filename
    );

    let mut lines: Vec<String> = summary.lines().map(|l| l.to_string()).collect();

    let mut inserted = false;
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == date_heading.trim() {
            let mut j = i + 1;
            let mut found_edition = false;
            while j < lines.len() && lines[j].starts_with("        - ") {
                if lines[j].trim() == edition_heading.trim() {
                    found_edition = true;
                    break;
                }
                j += 1;
            }
            if !found_edition {
                lines.insert(j, edition_heading.clone());
            }
            inserted = true;
            break;
        }
        i += 1;
    }

    if !inserted {
        if let Some(pos) = lines.iter().position(|l| l.contains("- [Daily News]")) {
            let insert_at = pos + 1;
            lines.insert(insert_at, date_heading.clone());
            lines.insert(insert_at + 1, edition_heading.clone());
        }
    }

    fs::write(&summary_path, lines.join("\n")).await?;
    info!(path = %summary_path, "Updated SUMMARY.md");
    Ok(())
}

#[instrument(level = "info", skip_all, fields(%markdown_output_dir, date = %front_page.local_date, file = %markdown_filename))]
async fn update_daily_news_index(
    markdown_output_dir: &str,
    front_page: &FrontPage,
    markdown_filename: &str,
) -> Result<(), Box<dyn Error>> {
    let index_path = format!("{}/daily_news.md", markdown_output_dir);
    let mut content = String::new();

    if Path::new(&index_path).exists() {
        content = fs::read_to_string(&index_path).await?;
    } else {
        content.push_str("# Awful News Index\n\n");
    }

    let date_heading = format!(
        "- [**{}**](./{}.md)",
        front_page.local_date, front_page.local_date
    );
    let edition_entry = format!(
        "    - [{}](./{})",
        upcase(&front_page.time_of_day),
        markdown_filename
    );

    let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    let mut inserted = false;
    let mut i = 0;

    while i < lines.len() {
        if lines[i].trim() == date_heading.trim() {
            let mut j = i + 1;
            let mut found_edition = false;
            while j < lines.len() && lines[j].starts_with("    - ") {
                if lines[j].trim() == edition_entry.trim() {
                    found_edition = true;
                    break;
                }
                j += 1;
            }
            if !found_edition {
                lines.insert(j, edition_entry.clone());
            }
            inserted = true;
            break;
        }
        i += 1;
    }

    if !inserted {
        if let Some(pos) = lines
            .iter()
            .position(|l| l.starts_with("# Awful News Index"))
        {
            let insert_at = pos + 1;
            lines.insert(insert_at, "".to_string());
            lines.insert(insert_at + 1, date_heading.clone());
            lines.insert(insert_at + 2, edition_entry.clone());
        } else {
            lines.push(date_heading.clone());
            lines.push(edition_entry.clone());
        }
    }

    fs::write(&index_path, lines.join("\n")).await?;
    info!(path = %index_path, "Updated daily_news.md index");
    Ok(())
}

// ===== Retry & API call wrappers with tracing and timing =====

use rand::{Rng, thread_rng};
use std::fmt;
use std::time::Duration as StdDuration;
use tokio::time::sleep;

pub trait AskAsync {
    type Response;
    async fn ask(&self, text: &str) -> Result<Self::Response, Box<dyn Error>>;
}

pub struct RetryAsk<T> {
    inner: T,
    max_retries: usize,
    base_delay: StdDuration,
    max_delay: StdDuration,
}

impl<T> RetryAsk<T>
where
    T: AskAsync,
{
    pub fn new(inner: T, max_retries: usize, base_delay: StdDuration) -> Self {
        Self {
            inner,
            max_retries,
            base_delay,
            max_delay: StdDuration::from_secs(30),
        }
    }
}

impl<T> fmt::Debug for RetryAsk<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetryAsk")
            .field("max_retries", &self.max_retries)
            .field("base_delay", &self.base_delay)
            .field("max_delay", &self.max_delay)
            .finish()
    }
}

impl<T> AskAsync for RetryAsk<T>
where
    T: AskAsync + fmt::Debug,
{
    type Response = T::Response;

    #[instrument(level = "info", skip_all)]
    async fn ask(&self, text: &str) -> Result<Self::Response, Box<dyn Error>> {
        let total_t0 = Instant::now();
        let mut attempt = 0usize;

        loop {
            let attempt_t0 = Instant::now();
            match self.inner.ask(text).await {
                Ok(resp) => {
                    let attempt_dt = attempt_t0.elapsed();
                    let total_dt = total_t0.elapsed();
                    info!(
                        attempt,
                        elapsed_ms_attempt = attempt_dt.as_millis() as u128,
                        elapsed_ms_total = total_dt.as_millis() as u128,
                        "ask() attempt succeeded"
                    );
                    return Ok(resp);
                }
                Err(e) => {
                    attempt += 1;
                    let attempt_dt = attempt_t0.elapsed();
                    let total_dt = total_t0.elapsed();

                    if attempt > self.max_retries {
                        error!(
                            attempt,
                            max = self.max_retries,
                            elapsed_ms_attempt = attempt_dt.as_millis() as u128,
                            elapsed_ms_total = total_dt.as_millis() as u128,
                            error = %e,
                            "ask() exhausted retries"
                        );
                        return Err(e);
                    }

                    // backoff calc
                    let mut delay = self.base_delay.saturating_mul(1 << (attempt - 1));
                    if delay > self.max_delay {
                        delay = self.max_delay;
                    }
                    let jitter_ms: u64 = thread_rng().gen_range(0..=250);
                    let delay = delay + StdDuration::from_millis(jitter_ms);

                    warn!(
                        attempt,
                        max = self.max_retries,
                        elapsed_ms_attempt = attempt_dt.as_millis() as u128,
                        elapsed_ms_total = total_dt.as_millis() as u128,
                        ?delay,
                        error = %e,
                        "ask() attempt failed; backing off"
                    );
                    sleep(delay).await;
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct AskFnWrapper<'a> {
    pub config: &'a AwfulJadeConfig,
    pub template: &'a ChatTemplate,
}

impl<'a> AskAsync for AskFnWrapper<'a> {
    type Response = String;

    #[instrument(level = "info", skip_all)]
    async fn ask(&self, text: &str) -> Result<Self::Response, Box<dyn Error>> {
        let t0 = Instant::now();
        let res = ask(self.config, text.to_string(), self.template, None, None).await;
        let dt = t0.elapsed();

        match &res {
            Ok(_) => info!(elapsed_ms = dt.as_millis() as u128, "API call succeeded"),
            Err(e) => warn!(elapsed_ms = dt.as_millis() as u128, error = %e, "API call failed"),
        }
        res
    }
}

#[instrument(level = "info", skip_all)]
pub async fn ask_with_backoff(
    config: &AwfulJadeConfig,
    article: &String,
    template: &ChatTemplate,
) -> Result<String, Box<dyn Error>> {
    let t0 = Instant::now();
    let client = AskFnWrapper { config, template };
    let api = RetryAsk::new(client, 5, StdDuration::from_secs(1));
    let res = api.ask(article).await;
    let dt = t0.elapsed();

    match &res {
        Ok(_) => info!(
            elapsed_ms_total = dt.as_millis() as u128,
            "ask_with_backoff succeeded"
        ),
        Err(e) => {
            error!(elapsed_ms_total = dt.as_millis() as u128, error = %e, "ask_with_backoff failed")
        }
    }
    res
}

// ===== IO guards =====

/// Ensure a directory exists and is writable (create + touch + delete).
#[instrument(level = "info", skip_all, fields(path = %path))]
async fn ensure_writable_dir(path: &str) -> Result<(), Box<dyn Error>> {
    if let Err(e) = fs::create_dir_all(path).await {
        return Err(Box::new(e));
    }
    // Try a small sync write using std fs (simpler error surface)
    let probe_path = format!("{}/.__probe_write__", path.trim_end_matches('/'));
    match stdfs::File::create(&probe_path) {
        Ok(_) => {
            let _ = stdfs::remove_file(&probe_path);
            info!("Output directory is writable");
            Ok(())
        }
        Err(e) => Err(Box::new(e)),
    }
}
