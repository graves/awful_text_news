use crate::models::FrontPage;
use crate::utils::{slugify_title, upcase};
use std::error::Error;
use std::fmt::Write;
use std::path::Path;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{info, instrument};

/// Update the date-specific table of contents file
#[instrument(level = "info", skip_all, fields(%markdown_output_dir, date = %front_page.local_date, file = %markdown_filename))]
pub async fn update_date_toc_file(
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

    // Group articles by category
    use std::collections::BTreeMap;
    let mut articles_by_category: BTreeMap<String, Vec<&crate::models::AwfulNewsArticle>> = BTreeMap::new();
    
    for article in &front_page.articles {
        articles_by_category
            .entry(article.category.clone())
            .or_insert_with(Vec::new)
            .push(article);
    }

    // Write articles organized by category (alphabetically)
    for (category, articles) in articles_by_category {
        let category_slug = slugify_title(&category);
        writeln!(toc_md, "\t- [**{}**]({}#{})", category, markdown_filename, category_slug).unwrap();
        
        for article in articles {
            let slug = slugify_title(&article.title);
            let source_tag = article.source_tag()
                .map(|tag| format!(" <small>`{}`</small>", tag))
                .unwrap_or_default();
            
            writeln!(
                toc_md,
                "\t\t- {} - [{}]({}#{})",
                source_tag, article.title, markdown_filename, slug
            )
            .unwrap();
        }
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

/// Update the SUMMARY.md file for mdBook navigation
#[instrument(level = "info", skip_all, fields(%markdown_output_dir, date = %front_page.local_date, file = %markdown_filename))]
pub async fn update_summary_md(
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

/// Update the daily_news.md index file
#[instrument(level = "info", skip_all, fields(%markdown_output_dir, date = %front_page.local_date, file = %markdown_filename))]
pub async fn update_daily_news_index(
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
