use awful_aj::api::ask;
use awful_aj::config_dir;
use awful_aj::template;
use awful_aj::{config, config::AwfulJadeConfig, template::ChatTemplate};
use chrono::Duration;
use chrono::Local;
use chrono::NaiveTime;
use clap::Parser;
use futures::stream::{self, StreamExt};
use itertools::Itertools;
use reqwest::get;
use scraper::{Html, Selector};
use serde::Deserialize;
use serde::Serialize;
use std::error::Error;
use std::fmt::Write;
use std::path::Path;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use url::Url;

/// Main program to scrape and analyze news articles
/// from CNN and NPR, outputting JSON/API files and markdown reports.
/// Uses SQLite-style documentation for clarity.
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

/// Struct representing a news article with its content
#[derive(Debug)]
struct NewsArticle {
    source: String,
    content: String,
}

/// Struct for the front page of news articles
#[derive(Debug, Deserialize, Serialize)]
pub struct FrontPage {
    local_date: String,
    time_of_day: String,
    local_time: String,
    articles: Vec<AwfulNewsArticle>,
}

/// Struct for each news article with metadata and analysis
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

/// Struct for named entities in articles
#[derive(Debug, Deserialize, Serialize)]
pub struct NamedEntity {
    pub name: String,
    pub whatIsThisEntity: String,
    pub whyIsThisEntityRelevantToTheArticle: String,
}

/// Struct for important dates in articles
#[derive(Debug, Deserialize, Serialize)]
pub struct ImportantDate {
    pub dateMentionedInArticle: String,
    pub descriptionOfWhyDateIsRelevant: String,
}

/// Struct for important timeframes in articles
#[derive(Debug, Deserialize, Serialize)]
pub struct ImportantTimeframe {
    pub approximateTimeFrameStart: String,
    pub approximateTimeFrameEnd: String,
    pub descriptionOfWhyTimeFrameIsRelevant: String,
}

/// Main program to scrape and analyze news articles
/// from CNN and NPR, outputting JSON/API files and markdown reports.
///
/// # Arguments
/// * `json_output_dir` - Directory to save JSON API files (required)
/// * `markdown_output_dir` - Directory to save markdown reports (required)
///
/// # Returns
/// * `Result<(), Box<dyn Error>>` - Ok on success, error if any step fails
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Initialize start time for performance metrics
    let start_time = std::time::Instant::now();

    // Parse command-line arguments
    let args = Cli::parse();

    // Set base URLs for CNN and NPR
    let cnn_page_url = "https://lite.cnn.com";
    let cnn_base_url = Url::parse(cnn_page_url).expect("Invalid base URL");

    let npr_page_url = "https://text.npr.org";
    let npr_base_url = Url::parse(npr_page_url).expect("Invalid base URL");

    // Fetch and process articles from CNN
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

    println!(
        "Indexed {} article urls from {}",
        cnn_article_urls.len(),
        cnn_page_url
    );

    // Fetch and process articles from NPR
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

    println!(
        "Indexed {} article urls from {}",
        npr_article_urls.len(),
        npr_page_url
    );

    // Independently fetch each article from CNN
    let cnn_articles: Vec<NewsArticle> = stream::iter(cnn_article_urls)
        .filter_map(|url: String| async move { fetch_cnn_article(&url).await.unwrap() })
        .collect()
        .await;

    println!("Fetched {} article contents from CNN", cnn_articles.len());

    // Independently fetch each article from NPR
    let npr_articles: Vec<NewsArticle> = stream::iter(npr_article_urls)
        .filter_map(|url: String| async move { fetch_npr_article(&url).await.unwrap() })
        .collect()
        .await;

    println!("Fetched {} article contents from NPR", npr_articles.len());

    // Combine articles from both sources
    let articles = vec![cnn_articles, npr_articles]
        .into_iter()
        .flatten()
        .collect::<Vec<NewsArticle>>();

    // Load configuration and template
    let template = template::load_template("news_parser").await?;
    let conf_file = config_dir()?.join("config.yaml");
    let config =
        config::load_config(conf_file.to_str().expect("Not a valid config filename")).unwrap();

    // Create front page with current date/time
    let local_date = Local::now().date_naive().to_string();
    let local_time = Local::now().time().to_string();
    let mut front_page = FrontPage {
        time_of_day: time_of_day(),
        local_time: local_time,
        local_date: local_date,
        articles: Vec::new(),
    };

    // Process each article and generate JSON/API file
    let mut processed_count = 0;
    for article in &articles {
        // Ask the API to analyze this article
        let response_json = ask_with_backoff(&config, &article.content.clone(), &template).await?;
        let awful_news_article: Result<AwfulNewsArticle, serde_json::Error> =
            serde_json::from_str(&response_json);

        if awful_news_article.is_ok() {
            let mut awful_news_article = awful_news_article.unwrap();
            // Populate source and content
            awful_news_article.source = Some(article.source.clone());
            awful_news_article.content = Some(article.content.clone());

            // Deduplicate entities and timeframes to avoid redundancy
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

            // Add to front page
            front_page.articles.push(awful_news_article);

            // Generate JSON output based on time of day
            let json = serde_json::to_string(&front_page).unwrap();

            let midnight = NaiveTime::from_hms_opt(23, 59, 59).unwrap();
            let today = Local::now().time();
            let yesterday = today - Duration::days(1);

            let api_file_dir = if front_page.time_of_day == "evening" && (today >= midnight) {
                format!("{}", yesterday.to_string())
            } else {
                format!("{}", front_page.local_date)
            };

            fs::create_dir_all(&api_file_dir).await?;

            let full_json_dir = if front_page.time_of_day == "evening" && (today >= midnight) {
                format!("{}/{}", args.json_output_dir, yesterday.to_string())
            } else {
                format!("{}/{}", args.json_output_dir, front_page.local_date)
            };

            println!("🛠 About to create: {}", full_json_dir);

            fs::create_dir_all(&full_json_dir).await?;

            let output_json_filename = if front_page.time_of_day == "evening" && (today >= midnight)
            {
                format!("{}/{}.json", full_json_dir, yesterday.to_string())
            } else {
                format!("{}/{}.json", full_json_dir, front_page.time_of_day)
            };

            println!("📝 Writing JSON to: {}", output_json_filename);

            fs::write(&output_json_filename, json).await?;
            if processed_count == 0 {
                println!("Wrote JSON API file to {}", output_json_filename);
            }
        }

        // Print progress
        processed_count += 1;
        println!("Processed {}/{} articles", processed_count, articles.len());
    }

    let midnight = NaiveTime::from_hms_opt(23, 59, 59).unwrap();
    let today = Local::now().time();
    let yesterday = today - Duration::days(1);

    // Generate markdown output
    let markdown = front_page_to_markdown(&front_page);

    let output_markdown_filename = if front_page.time_of_day == "evening" && (today >= midnight) {
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

    fs::write(&output_markdown_filename, markdown).await?;

    println!("Wrote FrontPage to {}", output_markdown_filename);

    // Update TOC and index files
    let _res = update_date_toc_file(
        &args.markdown_output_dir,
        &front_page,
        &format!("{}_{}.md", front_page.local_date, front_page.time_of_day),
    )
    .await?;

    update_summary_md(
        &args.markdown_output_dir,
        &front_page,
        &format!("{}_{}.md", front_page.local_date, front_page.time_of_day),
    )
    .await?;

    update_daily_news_index(
        &args.markdown_output_dir,
        &front_page,
        &format!("{}_{}.md", front_page.local_date, front_page.time_of_day),
    )
    .await?;

    // Log execution time
    let elapsed = start_time.elapsed();
    println!(
        "Execution time: {:.2?} ({}.{:03} seconds)",
        elapsed,
        elapsed.as_secs(),
        elapsed.subsec_millis()
    );

    Ok(())
}

/// Determine the time of day based on current local time
///
/// # Returns
/// * `String` - "morning", "afternoon", or "evening"
pub fn time_of_day() -> String {
    let morning_low = NaiveTime::from_hms_opt(0, 00, 0).unwrap();
    let morning_high = NaiveTime::from_hms_opt(8, 00, 0).unwrap();

    let afternoon_low = NaiveTime::from_hms_opt(8, 00, 0).unwrap();
    let afternoon_high = NaiveTime::from_hms_opt(16, 00, 0).unwrap();

    let _evening_low = NaiveTime::from_hms_opt(16, 00, 0).unwrap();
    let _evening_high = NaiveTime::from_hms_opt(0, 00, 0).unwrap();

    let time_of_day = Local::now().time();

    if (time_of_day >= morning_low) && (time_of_day < morning_high) {
        "morning".to_string()
    } else if (time_of_day >= afternoon_low) && (time_of_day < afternoon_high) {
        "afternoon".to_string()
    } else {
        "evening".to_string()
    }
}

/// Fetch and parse a CNN article from the given URL
///
/// # Arguments
/// * `url` - The URL of the CNN article to fetch
///
/// # Returns
/// * `Result<Option<NewsArticle>, Box<dyn Error>>` - Ok with parsed article or None if no content
async fn fetch_cnn_article(url: &str) -> Result<Option<NewsArticle>, Box<dyn Error>> {
    // Attempt to fetch and parse the page
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

    Ok(Some(NewsArticle {
        source: url.to_string(),
        content,
    }))
}

/// Fetch and parse an NPR article from the given URL
///
/// # Arguments
/// * `url` - The URL of the NPR article to fetch
///
/// # Returns
/// * `Result<Option<NewsArticle>, Box<dyn Error>>` - Ok with parsed article or None if no content
async fn fetch_npr_article(url: &str) -> Result<Option<NewsArticle>, Box<dyn Error>> {
    // Attempt to fetch and parse the page
    let body = get(url).await?.text().await?;

    // Parse HTML document
    let document = Html::parse_document(&body);

    // Get headlines and article content
    let mut content = String::new();
    let headline_selector = Selector::parse(".story-head")?;
    let article_selector = Selector::parse(".paragraphs-container")?;

    // Extract text from elements
    for element in document
        .select(&headline_selector)
        .chain(document.select(&article_selector))
    {
        let text = element.text().collect::<Vec<_>>().join(" ");
        content.push_str(&text);
        content.push_str("\n");
    }

    Ok(Some(NewsArticle {
        source: url.to_string(),
        content,
    }))
}

/// Convert a FrontPage struct to markdown
///
/// # Arguments
/// * `front_page` - The FrontPage object containing news articles to format
///
/// # Returns
/// * `String` - Markdown-formatted content of the front page
pub fn front_page_to_markdown(front_page: &FrontPage) -> String {
    let mut md = String::new();

    // Header
    writeln!(md, "# Awful Times\n").unwrap();
    writeln!(md, "#### Edition published at {}\n", front_page.local_time).unwrap();

    // Article sections
    for article in &front_page.articles {
        writeln!(md, "## {}\n", article.title).unwrap();

        // Source link
        if let Some(source) = &article.source {
            writeln!(md, "- [source]({})", source).unwrap();
        }
        // Publication date and time
        writeln!(
            md,
            "- _Published: {} {}_\n",
            article.dateOfPublication, article.timeOfPublication
        )
        .unwrap();

        // Summary
        writeln!(md, "### Summary\n").unwrap();
        writeln!(md, "{}\n", article.summaryOfNewsArticle.trim()).unwrap();

        // Key takeaways
        if !article.keyTakeAways.is_empty() {
            writeln!(md, "### Key Takeaways").unwrap();
            for takeaway in &article.keyTakeAways {
                writeln!(md, "  - {}", takeaway).unwrap();
            }
            writeln!(md).unwrap();
        }

        // Named entities
        if !article.namedEntities.is_empty() {
            writeln!(md, "### Named Entities").unwrap();
            for entity in &article.namedEntities {
                writeln!(md, "- **{}**", entity.name).unwrap();
                writeln!(md, "    - {}", entity.whatIsThisEntity).unwrap();
                writeln!(md, "    - {}", entity.whyIsThisEntityRelevantToTheArticle).unwrap();
            }
            writeln!(md).unwrap();
        }

        // Important dates
        if !article.importantDates.is_empty() {
            writeln!(md, "### Important Dates").unwrap();
            for date in &article.importantDates {
                writeln!(md, "  - **{}**", date.dateMentionedInArticle).unwrap();
                writeln!(md, "    - {}", date.descriptionOfWhyDateIsRelevant).unwrap();
            }
            writeln!(md).unwrap();
        }

        // Important timeframes
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

        // Separator line
        writeln!(md, "---\n").unwrap();
    }

    md
}

/// Sanitize titles into Markdown-compatible fragment identifiers
///
/// # Arguments
/// * `title` - The original title string to sanitize
///
/// # Returns
/// * `String` - Lowercase, alphanumeric-only version with spaces replaced by
fn slugify_title(title: &str) -> String {
    title
        .to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != ' ', "")
        .replace(' ', "-")
}

/// Append or create a TOC markdown file for the day's editions
///
/// # Arguments
/// * `markdown_output_dir` - Directory where markdown files are stored
/// * `front_page` - The FrontPage object containing the current edition's data
/// * `markdown_filename` - Name of the markdown file to update
///
/// # Returns
/// * `Result<(), Box<dyn Error>>` - Ok on success, error if file operations fail
async fn update_date_toc_file(
    markdown_output_dir: &str,
    front_page: &FrontPage,
    markdown_filename: &str,
) -> Result<(), Box<dyn Error>> {
    let toc_path = format!("{}/{}.md", markdown_output_dir, front_page.local_date);
    let mut toc_md = String::new();

    // Create TOC file if it doesn't exist
    if !Path::new(&toc_path).exists() {
        writeln!(
            toc_md,
            "# Editions published on {}\n",
            front_page.local_date
        )
        .unwrap();
    }

    // Add current edition to TOC
    writeln!(
        toc_md,
        "- [{}](./{})",
        upcase(&front_page.time_of_day),
        markdown_filename
    )
    .unwrap();

    // Add article links
    for article in &front_page.articles {
        let slug = slugify_title(&article.title);
        writeln!(
            toc_md,
            "\t- [{}]({}#{})",
            article.title, markdown_filename, slug
        )
        .unwrap();
    }

    // Write to file
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&toc_path)
        .await?;

    file.write_all(toc_md.as_bytes()).await?;

    println!("Updated TOC file at {}", toc_path);
    Ok(())
}

/// Converts a string to its title-case version.
///
/// This function takes a slice of bytes representing text and returns
/// a new string where the first character of each word is converted
/// to uppercase, while the rest remain unchanged.
///
/// # Examples
/// ```
/// let result = upcase("hello world");
/// assert_eq!(result, "Hello World");
///
/// let result = upcase("HELLO WORLD");
/// assert_eq!(result, "Hello World");
///
/// let result = upcase("hello");
/// assert_eq!(result, "Hello");
/// ```
///
/// # Parameters
/// * `s` - A slice of bytes representing the input text.
///
/// # Returns
/// * `String` - A new string with title-cased characters.
fn upcase(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

/// Update the SUMMARY.md file with new edition entries
///
/// # Arguments
/// * `markdown_output_dir` - Directory where markdown files are stored
/// * `front_page` - The FrontPage object containing the current edition's data
/// * `markdown_filename` - Name of the markdown file to update
///
/// # Returns
/// * `Result<(), Box<dyn Error>>` - Ok on success, error if file operations fail
async fn update_summary_md(
    markdown_output_dir: &str,
    front_page: &FrontPage,
    markdown_filename: &str,
) -> Result<(), Box<dyn Error>> {
    let summary_path = format!("{}/SUMMARY.md", markdown_output_dir);
    let mut summary = String::new();

    // Read existing summary file
    if Path::new(&summary_path).exists() {
        summary = fs::read_to_string(&summary_path).await?;
    } else {
        // Create default summary if it doesn't exist
        summary.push_str("# Summary\n\n[Home](./home.md)\n- [PGP](./pgp.md)\n- [Contact](./contact.md)\n- [Daily News](./daily_news.md)\n");
    }

    // Find and insert date/edition entries
    let date_heading = format!(
        "    - [{}](./{}.md)",
        front_page.local_date, front_page.local_date
    );
    let edition_heading = format!(
        "        - [{}](./{})",
        upcase(&front_page.time_of_day),
        markdown_filename
    );

    // Prepare updated lines
    let mut lines: Vec<String> = summary.lines().map(|l| l.to_string()).collect();

    let mut inserted = false;
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == date_heading.trim() {
            // Date already exists, check if edition is listed
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
        // Insert new date section under `- [Daily News](./daily_news.md)`
        if let Some(pos) = lines.iter().position(|l| l.contains("- [Daily News]")) {
            let insert_at = pos + 1;
            lines.insert(insert_at, date_heading.clone());
            lines.insert(insert_at + 1, edition_heading.clone());
        }
    }

    // Write back
    fs::write(&summary_path, lines.join("\n")).await?;
    println!("Updated SUMMARY.md");

    Ok(())
}

/// Update the daily_news.md index file with new edition entries
///
/// # Arguments
/// * `markdown_output_dir` - Directory where markdown files are stored
/// * `front_page` - The FrontPage object containing the current edition's data
/// * `markdown_filename` - Name of the markdown file to update
///
/// # Returns
/// * `Result<(), Box<dyn Error>>` - Ok on success, error if file operations fail
async fn update_daily_news_index(
    markdown_output_dir: &str,
    front_page: &FrontPage,
    markdown_filename: &str,
) -> Result<(), Box<dyn Error>> {
    let index_path = format!("{}/daily_news.md", markdown_output_dir);
    let mut content = String::new();

    // Read existing index file
    if Path::new(&index_path).exists() {
        content = fs::read_to_string(&index_path).await?;
    } else {
        // Create default index if it doesn't exist
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
            // Date exists, check if edition exists
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
        // Insert new date and edition near the top (after header)
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
    println!("Updated daily_news.md");
    Ok(())
}

use std::fmt;
use std::time::SystemTime; // For backoff timing

// 1. Define `TryInterval` for exponential backoff
pub trait TryInterval {
    fn next(&mut self) -> Result<Duration, Box<dyn Error>>;
}

// 2. Implement exponential backoff with a base delay
struct ExponentialBackoff {
    base_delay: Duration,
}
impl TryInterval for ExponentialBackoff {
    fn next(&mut self) -> Result<Duration, Box<dyn Error>> {
        let now = SystemTime::now();
        let elapsed = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();

        // If no error, return a delay (e.g., for initial attempt)
        if elapsed.as_secs() < 1 {
            return Ok(self.base_delay);
        }

        // Otherwise, compute exponential backoff
        let delay = self.base_delay * 2;
        if delay > Duration::new(30, 0).unwrap() {
            return Err("Maximum backoff reached".into()); // Cap maximum backoff
        }

        Ok(delay)
    }
}

// 3. Define `AskAsync` trait with retry logic
pub trait AskAsync {
    type Response;
    async fn ask(&self, text: &str) -> Result<Self::Response, Box<dyn Error>>;
}

// 4. Implement retry logic for `ask`
pub struct RetryAsk<T>(T, usize, Duration);
impl<T> RetryAsk<T>
where
    T: AskAsync,
{
    pub fn new(inner: T, max_retries: usize, delay: Duration) -> Self {
        RetryAsk(inner, max_retries, delay)
    }
}

impl<T> AskAsync for RetryAsk<T>
where
    T: AskAsync + fmt::Debug,
{
    type Response = T::Response;

    async fn ask(&self, text: &str) -> Result<Self::Response, Box<dyn Error>> {
        let mut retries = 0;

        while retries < self.1 {
            match self.0.ask(text).await {
                Ok(response) => return Ok(response),
                Err(e) => {
                    eprintln!("Error: {e}");
                    if retries < self.1 - 1 {
                        let delay = if self.2.is_zero() {
                            Duration::new(1, 0).unwrap()
                        } else {
                            self.2 * (retries + 1).pow(2).try_into().unwrap()
                        };
                        std::thread::sleep(delay.to_std().unwrap());
                    } else {
                        return Err(e);
                    }
                    retries += 1;
                }
            }
        }
        unreachable!("Max retries reached")
    }
}

#[derive(Debug)]
pub struct AskFnWrapper<'a> {
    pub config: &'a AwfulJadeConfig,
    pub template: &'a ChatTemplate,
}

impl<'a> AskAsync for AskFnWrapper<'a> {
    type Response = String;

    async fn ask(&self, text: &str) -> Result<Self::Response, Box<dyn Error>> {
        // Call the function from the dependency
        ask(self.config, text.to_string(), self.template, None, None).await
    }
}

pub async fn ask_with_backoff(
    config: &AwfulJadeConfig,
    article: &String,
    template: &ChatTemplate,
) -> Result<String, Box<dyn Error>> {
    let client = AskFnWrapper { config, template };
    let api = RetryAsk::new(client, 5, Duration::new(1, 0).unwrap());
    api.ask(article).await
}
