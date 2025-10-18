use crate::models::FrontPage;
use chrono::{Duration, Local, NaiveTime};
use std::error::Error;
use tokio::fs;
use tracing::{error, info, instrument};

/// Write FrontPage to JSON file with date-based directory structure
#[instrument(level = "info", skip_all, fields(json_output_dir = %json_output_dir))]
pub async fn write_frontpage(
    front_page: &FrontPage,
    json_output_dir: &str,
) -> Result<(), Box<dyn Error>> {
    let json = serde_json::to_string(front_page)?;

    let midnight = NaiveTime::from_hms_opt(23, 59, 59).unwrap();
    let now = Local::now().time();
    let yesterday = Local::now().date_naive() - Duration::days(1);

    let full_json_dir = if front_page.time_of_day == "evening" && (now >= midnight) {
        format!("{}/{}", json_output_dir, yesterday.to_string())
    } else {
        format!("{}/{}", json_output_dir, front_page.local_date)
    };

    info!(%full_json_dir, "Ensuring JSON directory exists");
    if let Err(e) = fs::create_dir_all(&full_json_dir).await {
        error!(%full_json_dir, error = %e, "Failed to create JSON dir");
        return Err(e.into());
    }

    let output_json_filename = if front_page.time_of_day == "evening" && (now >= midnight) {
        format!("{}/{}.json", full_json_dir, yesterday.to_string())
    } else {
        format!("{}/{}.json", full_json_dir, front_page.time_of_day)
    };

    info!(path = %output_json_filename, "Writing JSON");
    fs::write(&output_json_filename, json).await?;
    info!(path = %output_json_filename, "Wrote JSON API file");

    Ok(())
}
