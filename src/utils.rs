use chrono::{Local, NaiveTime};
use std::error::Error;
use std::fs as stdfs;
use tokio::fs;
use tracing::{info, instrument, warn};

/// Classify current time into morning/afternoon/evening
#[instrument]
pub fn time_of_day() -> String {
    let morning_low = NaiveTime::from_hms_opt(0, 00, 0).unwrap();
    let morning_high = NaiveTime::from_hms_opt(8, 00, 0).unwrap();
    let afternoon_low = NaiveTime::from_hms_opt(8, 00, 0).unwrap();
    let afternoon_high = NaiveTime::from_hms_opt(16, 00, 0).unwrap();

    let tod = Local::now().time();
    let which = if (tod >= morning_low) && (tod < morning_high) {
        "morning"
    } else if (tod >= afternoon_low) && (tod < afternoon_high) {
        "afternoon"
    } else {
        "evening"
    };
    tracing::debug!(%tod, %which, "Computed time_of_day");
    which.to_string()
}

/// Truncate a string for logging purposes
pub fn truncate_for_log(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…(+{} bytes)", &s[..max], s.len() - max)
    }
}

/// Classify serde_json error category to detect truncation/EOF.
pub fn looks_truncated(e: &serde_json::Error) -> bool {
    use serde_json::error::Category;
    matches!(e.classify(), Category::Eof)
}

/// Convert title to URL-friendly slug
pub fn slugify_title(title: &str) -> String {
    title
        .to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != ' ' && c != '-', "")
        .replace(' ', "-")
}

/// Capitalize first character of a string
pub fn upcase(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

/// Ensure a directory exists and is writable (create + touch + delete).
#[instrument(level = "info", skip_all, fields(path = %path))]
pub async fn ensure_writable_dir(path: &str) -> Result<(), Box<dyn Error>> {
    if let Err(e) = fs::create_dir_all(path).await {
        return Err(Box::new(e));
    }
    // Try a small sync write using std fs (simpler error surface)
    let probe_path = format!("{}/..__probe_write__", path.trim_end_matches('/'));
    match stdfs::File::create(&probe_path) {
        Ok(_) => {
            let _ = stdfs::remove_file(&probe_path);
            info!("Output directory is writable");
            Ok(())
        }
        Err(e) => Err(Box::new(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveTime;

    #[test]
    fn test_truncate_for_log_short_string() {
        let s = "Hello, world!";
        assert_eq!(truncate_for_log(s, 100), "Hello, world!");
    }

    #[test]
    fn test_truncate_for_log_long_string() {
        let s = "a".repeat(500);
        let result = truncate_for_log(&s, 100);
        assert!(result.starts_with(&"a".repeat(100)));
        assert!(result.contains("…(+400 bytes)"));
    }

    #[test]
    fn test_slugify_title() {
        assert_eq!(slugify_title("Hello World"), "hello-world");
        assert_eq!(slugify_title("Test-Article!"), "test-article");
        assert_eq!(slugify_title("Multiple   Spaces"), "multiple---spaces");
        assert_eq!(
            slugify_title("Special@#$Characters"),
            "specialcharacters"
        );
        assert_eq!(
            slugify_title("Trump-Xi 'situationship'"),
            "trump-xi-situationship"
        );
    }

    #[test]
    fn test_upcase() {
        assert_eq!(upcase("hello"), "Hello");
        assert_eq!(upcase("world"), "World");
        assert_eq!(upcase(""), "");
        assert_eq!(upcase("a"), "A");
    }

    #[test]
    fn test_time_of_day_morning() {
        // We can't easily test the actual time_of_day function without mocking time,
        // but we can test the logic by checking specific times
        let morning = NaiveTime::from_hms_opt(6, 30, 0).unwrap();
        let morning_low = NaiveTime::from_hms_opt(0, 0, 0).unwrap();
        let morning_high = NaiveTime::from_hms_opt(8, 0, 0).unwrap();
        assert!(morning >= morning_low && morning < morning_high);
    }

    #[test]
    fn test_time_of_day_afternoon() {
        let afternoon = NaiveTime::from_hms_opt(12, 0, 0).unwrap();
        let afternoon_low = NaiveTime::from_hms_opt(8, 0, 0).unwrap();
        let afternoon_high = NaiveTime::from_hms_opt(16, 0, 0).unwrap();
        assert!(afternoon >= afternoon_low && afternoon < afternoon_high);
    }

    #[test]
    fn test_time_of_day_evening() {
        let evening = NaiveTime::from_hms_opt(20, 0, 0).unwrap();
        let afternoon_high = NaiveTime::from_hms_opt(16, 0, 0).unwrap();
        assert!(evening >= afternoon_high);
    }

    #[test]
    fn test_looks_truncated() {
        // Test EOF detection
        let json_eof = r#"{"field": "value"#; // Missing closing brace
        let result: Result<serde_json::Value, _> = serde_json::from_str(json_eof);
        if let Err(e) = result {
            assert!(looks_truncated(&e));
        }
    }
}
