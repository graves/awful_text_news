use crate::models::FrontPage;
use std::fmt::Write;
use tracing::{debug, instrument};

/// Convert a FrontPage to Markdown format
#[instrument(level = "debug", skip_all)]
pub fn front_page_to_markdown(front_page: &FrontPage) -> String {
    let mut md = String::new();

    writeln!(md, "# Awful Times\n").unwrap();
    writeln!(md, "#### Edition published at {}\n", front_page.local_time).unwrap();

    // Group articles by category
    use std::collections::BTreeMap;
    let mut articles_by_category: BTreeMap<String, Vec<&crate::models::AwfulNewsArticle>> = BTreeMap::new();
    
    for article in &front_page.articles {
        articles_by_category
            .entry(article.category.clone())
            .or_insert_with(Vec::new)
            .push(article);
    }

    // Process each category in alphabetical order
    for (category, articles) in articles_by_category {
        writeln!(md, "# {}\n", category).unwrap();

        for article in articles {
            // Title with source tag
            if let Some(tag) = article.source_tag() {
                writeln!(
                    md,
                    "## {} - <small>`{}`</small>\n",
                    article.title, tag
                )
                .unwrap();
            } else {
                writeln!(md, "## {}\n", article.title).unwrap();
            }

            // Source link
            if let Some(source) = &article.source {
                writeln!(md, "- [source]({})", source).unwrap();
            }

            // Publication date/time
            writeln!(
                md,
                "- _Published: {} {}_",
                article.dateOfPublication, article.timeOfPublication
            )
            .unwrap();

            // Category
            writeln!(md, "- **{}**", article.category).unwrap();

            // Tags
            if !article.tags.is_empty() {
                let tags_str = article.tags.join(", ");
                writeln!(md, "- <small>tags: `{}`</small>\n", tags_str).unwrap();
            } else {
                writeln!(md).unwrap();
            }

            // Summary
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
    }

    debug!(chars = md.len(), "Rendered Markdown length");
    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AwfulNewsArticle;

    #[test]
    fn test_empty_frontpage_markdown() {
        let frontpage = FrontPage {
            local_date: "2025-05-06".to_string(),
            time_of_day: "evening".to_string(),
            local_time: "20:30:00".to_string(),
            articles: vec![],
        };

        let md = front_page_to_markdown(&frontpage);
        assert!(md.contains("# Awful Times"));
        assert!(md.contains("20:30:00"));
    }

    #[test]
    fn test_frontpage_with_article() {
        let article = AwfulNewsArticle {
            source: Some("https://example.com/article".to_string()),
            dateOfPublication: "2025-05-06".to_string(),
            timeOfPublication: "14:30:00".to_string(),
            title: "Test Article".to_string(),
            category: "Science & Technology".to_string(),
            summaryOfNewsArticle: "Test summary.".to_string(),
            keyTakeAways: vec!["Point 1".to_string()],
            namedEntities: vec![],
            importantDates: vec![],
            importantTimeframes: vec![],
            tags: vec!["tech".to_string(), "science".to_string()],
            content: None,
        };

        let frontpage = FrontPage {
            local_date: "2025-05-06".to_string(),
            time_of_day: "morning".to_string(),
            local_time: "08:00:00".to_string(),
            articles: vec![article],
        };

        let md = front_page_to_markdown(&frontpage);
        assert!(md.contains("## Test Article - <small>`example`</small>"));
        assert!(md.contains("`example`"));  // source tag
        assert!(md.contains("**Science & Technology**"));  // category
        assert!(md.contains("tags: `tech, science`"));  // tags
        assert!(md.contains("Test summary"));
        assert!(md.contains("Point 1"));
    }
}
