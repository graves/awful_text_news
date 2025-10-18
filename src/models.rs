use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct NewsArticle {
    pub source: String,
    pub content: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FrontPage {
    pub local_date: String,
    pub time_of_day: String,
    pub local_time: String,
    pub articles: Vec<AwfulNewsArticle>,
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize, Serialize)]
pub struct AwfulNewsArticle {
    pub source: Option<String>,
    pub dateOfPublication: String,
    pub timeOfPublication: String,
    pub title: String,
    pub category: String,
    pub summaryOfNewsArticle: String,
    pub keyTakeAways: Vec<String>,
    pub namedEntities: Vec<NamedEntity>,
    pub importantDates: Vec<ImportantDate>,
    pub importantTimeframes: Vec<ImportantTimeframe>,
    pub tags: Vec<String>,
    pub content: Option<String>,
}

impl AwfulNewsArticle {
    /// Extract the domain name (before .com/.org/etc) from the source URL
    /// For example: "https://lite.cnn.com/article" -> "cnn"
    pub fn source_tag(&self) -> Option<String> {
        self.source.as_ref().and_then(|url| {
            // Parse the URL and extract the host
            if let Ok(parsed) = url::Url::parse(url) {
                if let Some(host) = parsed.host_str() {
                    // Split by dots and get the domain before the TLD
                    let parts: Vec<&str> = host.split('.').collect();
                    // Handle cases like "lite.cnn.com" -> "cnn" or "cnn.com" -> "cnn"
                    if parts.len() >= 2 {
                        // Get the second-to-last part (domain before TLD)
                        return Some(parts[parts.len() - 2].to_string());
                    }
                }
            }
            None
        })
    }
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize, Serialize)]
pub struct NamedEntity {
    pub name: String,
    pub whatIsThisEntity: String,
    pub whyIsThisEntityRelevantToTheArticle: String,
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize, Serialize)]
pub struct ImportantDate {
    pub dateMentionedInArticle: String,
    pub descriptionOfWhyDateIsRelevant: String,
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize, Serialize)]
pub struct ImportantTimeframe {
    pub approximateTimeFrameStart: String,
    pub approximateTimeFrameEnd: String,
    pub descriptionOfWhyTimeFrameIsRelevant: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_news_article_creation() {
        let article = NewsArticle {
            source: "https://example.com".to_string(),
            content: "Test content".to_string(),
        };
        assert_eq!(article.source, "https://example.com");
        assert_eq!(article.content, "Test content");
    }

    #[test]
    fn test_frontpage_serialization() {
        let frontpage = FrontPage {
            local_date: "2025-05-06".to_string(),
            time_of_day: "evening".to_string(),
            local_time: "20:30:00".to_string(),
            articles: vec![],
        };

        let json = serde_json::to_string(&frontpage).unwrap();
        assert!(json.contains("2025-05-06"));
        assert!(json.contains("evening"));
    }

    #[test]
    fn test_frontpage_deserialization() {
        let json = r#"{
            "local_date": "2025-05-06",
            "time_of_day": "morning",
            "local_time": "08:00:00",
            "articles": []
        }"#;

        let frontpage: FrontPage = serde_json::from_str(json).unwrap();
        assert_eq!(frontpage.local_date, "2025-05-06");
        assert_eq!(frontpage.time_of_day, "morning");
        assert_eq!(frontpage.articles.len(), 0);
    }

    #[test]
    fn test_awful_news_article_with_entities() {
        let article = AwfulNewsArticle {
            source: Some("https://example.com".to_string()),
            dateOfPublication: "2025-05-06".to_string(),
            timeOfPublication: "14:30:00".to_string(),
            title: "Test Article".to_string(),
            category: "Politics & Governance".to_string(),
            summaryOfNewsArticle: "Summary here".to_string(),
            keyTakeAways: vec!["Key point 1".to_string()],
            namedEntities: vec![NamedEntity {
                name: "Entity Name".to_string(),
                whatIsThisEntity: "Description".to_string(),
                whyIsThisEntityRelevantToTheArticle: "Relevance".to_string(),
            }],
            importantDates: vec![],
            importantTimeframes: vec![],
            tags: vec!["politics".to_string(), "news".to_string()],
            content: Some("Full content".to_string()),
        };

        assert_eq!(article.title, "Test Article");
        assert_eq!(article.category, "Politics & Governance");
        assert_eq!(article.tags.len(), 2);
        assert_eq!(article.namedEntities.len(), 1);
        assert_eq!(article.namedEntities[0].name, "Entity Name");
    }

    #[test]
    fn test_named_entity_serialization() {
        let entity = NamedEntity {
            name: "John Doe".to_string(),
            whatIsThisEntity: "A person".to_string(),
            whyIsThisEntityRelevantToTheArticle: "Main subject".to_string(),
        };

        let json = serde_json::to_string(&entity).unwrap();
        let deserialized: NamedEntity = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "John Doe");
    }

    #[test]
    fn test_important_date_structure() {
        let date = ImportantDate {
            dateMentionedInArticle: "2025-12-25".to_string(),
            descriptionOfWhyDateIsRelevant: "Christmas Day".to_string(),
        };

        assert_eq!(date.dateMentionedInArticle, "2025-12-25");
    }

    #[test]
    fn test_important_timeframe_structure() {
        let timeframe = ImportantTimeframe {
            approximateTimeFrameStart: "2025-01-01".to_string(),
            approximateTimeFrameEnd: "2025-12-31".to_string(),
            descriptionOfWhyTimeFrameIsRelevant: "Full year 2025".to_string(),
        };

        assert_eq!(timeframe.approximateTimeFrameStart, "2025-01-01");
        assert_eq!(timeframe.approximateTimeFrameEnd, "2025-12-31");
    }

    #[test]
    fn test_source_tag_cnn() {
        let article = AwfulNewsArticle {
            source: Some("https://lite.cnn.com/2025/05/06/article".to_string()),
            dateOfPublication: "2025-05-06".to_string(),
            timeOfPublication: "14:30:00".to_string(),
            title: "Test".to_string(),
            category: "Politics & Governance".to_string(),
            summaryOfNewsArticle: "Summary".to_string(),
            keyTakeAways: vec![],
            namedEntities: vec![],
            importantDates: vec![],
            importantTimeframes: vec![],
            tags: vec![],
            content: None,
        };

        assert_eq!(article.source_tag(), Some("cnn".to_string()));
    }

    #[test]
    fn test_source_tag_npr() {
        let article = AwfulNewsArticle {
            source: Some("https://text.npr.org/article".to_string()),
            dateOfPublication: "2025-05-06".to_string(),
            timeOfPublication: "14:30:00".to_string(),
            title: "Test".to_string(),
            category: "Politics & Governance".to_string(),
            summaryOfNewsArticle: "Summary".to_string(),
            keyTakeAways: vec![],
            namedEntities: vec![],
            importantDates: vec![],
            importantTimeframes: vec![],
            tags: vec![],
            content: None,
        };

        assert_eq!(article.source_tag(), Some("npr".to_string()));
    }

    #[test]
    fn test_source_tag_no_source() {
        let article = AwfulNewsArticle {
            source: None,
            dateOfPublication: "2025-05-06".to_string(),
            timeOfPublication: "14:30:00".to_string(),
            title: "Test".to_string(),
            category: "Politics & Governance".to_string(),
            summaryOfNewsArticle: "Summary".to_string(),
            keyTakeAways: vec![],
            namedEntities: vec![],
            importantDates: vec![],
            importantTimeframes: vec![],
            tags: vec![],
            content: None,
        };

        assert_eq!(article.source_tag(), None);
    }

    #[test]
    fn test_source_tag_simple_domain() {
        let article = AwfulNewsArticle {
            source: Some("https://example.com/article".to_string()),
            dateOfPublication: "2025-05-06".to_string(),
            timeOfPublication: "14:30:00".to_string(),
            title: "Test".to_string(),
            category: "Politics & Governance".to_string(),
            summaryOfNewsArticle: "Summary".to_string(),
            keyTakeAways: vec![],
            namedEntities: vec![],
            importantDates: vec![],
            importantTimeframes: vec![],
            tags: vec![],
            content: None,
        };

        assert_eq!(article.source_tag(), Some("example".to_string()));
    }
}
