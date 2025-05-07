use awful_aj::api::ask;
use awful_aj::config;
use awful_aj::config_dir;
use awful_aj::template;
use chrono::Duration;
use chrono::Local;
use chrono::NaiveTime;
use futures::stream::{self, StreamExt};
use reqwest::get;
use scraper::{Html, Selector};
use serde::Deserialize;
use serde::Serialize;
use std::error::Error;
use std::fmt::Write;
use tokio::fs;
use url::Url;
use clap::Parser;
use std::path::Path;
use tokio::io::AsyncWriteExt;


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

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let start_time = std::time::Instant::now();

    let args = Cli::parse();

    let cnn_page_url = "https://lite.cnn.com";
    let cnn_base_url = Url::parse(cnn_page_url).expect("Invalid base URL");

    let npr_page_url = "https://text.npr.org";
    let npr_base_url = Url::parse(npr_page_url).expect("Invalid base URL");

    // Fetch CNN Lite front page
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

    // Fetch NPR Lite front page
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

    let articles = vec![cnn_articles, npr_articles]
        .into_iter()
        .flatten()
        .collect::<Vec<NewsArticle>>();

    let template = template::load_template("news_parser").await?;
    let conf_file = config_dir()?.join("config.yaml");
    let config =
        config::load_config(conf_file.to_str().expect("Not a valid config filename")).unwrap();

    let local_date = Local::now().date_naive().to_string();
    let local_time = Local::now().time().to_string();
    let mut front_page = FrontPage {
        time_of_day: time_of_day(),
        local_time: local_time,
        local_date: local_date,
        articles: Vec::new(),
    };

    // Output
    let mut processed_count = 0;
    for article in &articles {
        let response_json = ask(&config, article.content.clone(), &template, None, None).await?;
        let awful_news_article: Result<AwfulNewsArticle, serde_json::Error> =
            serde_json::from_str(&response_json);

        if awful_news_article.is_ok() {
            let mut awful_news_article = awful_news_article.unwrap();
            awful_news_article.source = Some(article.source.clone());
            awful_news_article.content = Some(article.content.clone());
            front_page.articles.push(awful_news_article);

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

            fs::create_dir_all(&full_json_dir).await?;

            let output_json_filename = if front_page.time_of_day == "evening" && (today >= midnight) {
                format!("{}/{}.json", full_json_dir, yesterday.to_string())
            } else {
                format!("{}/{}.json", full_json_dir, front_page.time_of_day)
            };

            fs::write(&output_json_filename, json).await?;
            if processed_count == 0 {
                println!("Wrote JSON API file to {}", output_json_filename);
            }
        }

        processed_count += 1;
        println!("Processed {}/{} articles", processed_count, articles.len());
    }

    let midnight = NaiveTime::from_hms_opt(23, 59, 59).unwrap();
    let today = Local::now().time();
    let yesterday = today - Duration::days(1);

    let markdown = front_page_to_markdown(&front_page);

    let output_markdown_filename = if front_page.time_of_day == "evening" && (today >= midnight) {
        format!("{}/{}_{}.md", args.markdown_output_dir, front_page.local_date, yesterday.to_string())
    } else {
        format!("{}/{}_{}.md", args.markdown_output_dir, front_page.local_date, front_page.time_of_day)
    };
    
    fs::write(&output_markdown_filename, markdown).await?;

    println!("Wrote FrontPage to {}", output_markdown_filename);

    let _res = update_date_toc_file(&args.markdown_output_dir, &front_page, &output_markdown_filename).await?;

    let elapsed = start_time.elapsed();
    println!(
        "Execution time: {:.2?} ({}.{:03} seconds)",
        elapsed,
        elapsed.as_secs(),
        elapsed.subsec_millis()
    );

    Ok(())
}

pub fn time_of_day() -> String {
    let morning_low = NaiveTime::from_hms_opt(4, 00, 0).unwrap();
    let morning_high = NaiveTime::from_hms_opt(12, 00, 0).unwrap();

    let afternoon_low = NaiveTime::from_hms_opt(12, 00, 0).unwrap();
    let afternoon_high = NaiveTime::from_hms_opt(8, 00, 0).unwrap();

    let _evening_low = NaiveTime::from_hms_opt(8, 00, 0).unwrap();
    let _evening_high = NaiveTime::from_hms_opt(4, 00, 0).unwrap();

    let time_of_day = Local::now().time();

    if (time_of_day >= morning_low) && (time_of_day < morning_high) {
        "morning".to_string()
    } else if (time_of_day >= afternoon_low) && (time_of_day < afternoon_high) {
        "afternoon".to_string()
    } else {
        "evening".to_string()
    }
}

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

async fn fetch_npr_article(url: &str) -> Result<Option<NewsArticle>, Box<dyn Error>> {
    // Attempt to fetch and parse the page
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

    Ok(Some(NewsArticle {
        source: url.to_string(),
        content,
    }))
}

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

    md
}

/// Sanitize titles into Markdown-compatible fragment identifiers
fn slugify_title(title: &str) -> String {
    title
        .to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != ' ', "")
        .replace(' ', "-")
}

/// Append or create a TOC markdown file for the day's editions
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
        front_page.time_of_day,
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

    println!("Updated TOC file at {}", toc_path);
    Ok(())
}