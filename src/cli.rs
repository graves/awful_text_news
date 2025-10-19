use clap::Parser;

/// Main program to scrape and analyze news articles
/// from CNN and NPR, outputting JSON/API files and markdown reports.
#[derive(Parser, Debug)]
#[command(author, version, about)]
pub struct Cli {
    /// Output directory for the JSON API file
    #[arg(short, long)]
    pub json_output_dir: String,

    /// Output directory for the Markdown file
    #[arg(short, long)]
    pub markdown_output_dir: String,

    /// Optional path to config.yaml file
    #[arg(short, long)]
    pub config: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_parsing() {
        let cli = Cli::parse_from(&[
            "awful_text_news",
            "--json-output-dir",
            "./json",
            "--markdown-output-dir",
            "./markdown",
        ]);

        assert_eq!(cli.json_output_dir, "./json");
        assert_eq!(cli.markdown_output_dir, "./markdown");
    }

    #[test]
    fn test_cli_short_flags() {
        let cli = Cli::parse_from(&[
            "awful_text_news",
            "-j",
            "/tmp/json",
            "-m",
            "/tmp/markdown",
        ]);

        assert_eq!(cli.json_output_dir, "/tmp/json");
        assert_eq!(cli.markdown_output_dir, "/tmp/markdown");
    }
}
