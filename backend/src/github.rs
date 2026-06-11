use std::time::Duration;

use anyhow::{Context as _, bail};
use serde_json::json;

use crate::config::GithubConfig;

const GITHUB_API_BASE: &str = "https://api.github.com";
const REQUEST_TIMEOUT_SECS: u64 = 15;
const MAX_TITLE_CHARS: usize = 200;
const MAX_BODY_BYTES: usize = 16 * 1024;
const ERROR_BODY_EXCERPT_LEN: usize = 256;

#[async_trait::async_trait]
pub trait IssueReporter: Send + Sync {
    /// Files a new issue and returns its `html_url`.
    async fn create_issue(&self, title: &str, body: &str) -> anyhow::Result<String>;
}

pub struct GithubIssueReporter {
    client: reqwest::Client,
    repo: String,
    token: String,
    labels: Vec<String>,
}

impl GithubIssueReporter {
    pub fn new(cfg: &GithubConfig) -> anyhow::Result<Self> {
        cfg.validate()?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .context("building GitHub HTTP client")?;
        Ok(Self { client, repo: cfg.repo.clone(), token: cfg.token.clone(), labels: cfg.labels.clone() })
    }
}

#[async_trait::async_trait]
impl IssueReporter for GithubIssueReporter {
    async fn create_issue(&self, title: &str, body: &str) -> anyhow::Result<String> {
        let title = truncate_chars(title, MAX_TITLE_CHARS);
        let body = truncate_bytes(body, MAX_BODY_BYTES);
        let url = format!("{GITHUB_API_BASE}/repos/{}/issues", self.repo);

        let mut payload = json!({
            "title": title,
            "body": body,
        });
        if !self.labels.is_empty() {
            payload["labels"] = json!(self.labels);
        }

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", concat!("corre-gym/", env!("CARGO_PKG_VERSION")))
            .json(&payload)
            .send()
            .await
            .context("GitHub create-issue request failed")?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            let excerpt: String = body_text.chars().take(ERROR_BODY_EXCERPT_LEN).collect();
            bail!("github API {status}: {excerpt}");
        }

        let value: serde_json::Value = resp.json().await.context("parsing GitHub create-issue response")?;
        let html_url = value
            .get("html_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("github API response missing html_url"))?;
        Ok(html_url.to_string())
    }
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn truncate_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push_str("\n…[truncated]");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructor_rejects_invalid_repo() {
        let cfg = GithubConfig { repo: "not-a-repo".into(), token: "x".into(), labels: vec![] };
        assert!(GithubIssueReporter::new(&cfg).is_err());
    }

    #[test]
    fn constructor_accepts_valid_repo() {
        let cfg = GithubConfig { repo: "owner/repo".into(), token: "x".into(), labels: vec![] };
        assert!(GithubIssueReporter::new(&cfg).is_ok());
    }

    #[test]
    fn truncate_chars_caps_length_with_ellipsis() {
        let long = "a".repeat(300);
        let out = truncate_chars(&long, 50);
        assert_eq!(out.chars().count(), 50);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn truncate_bytes_appends_marker_when_oversize() {
        let huge = "x".repeat(MAX_BODY_BYTES + 100);
        let out = truncate_bytes(&huge, MAX_BODY_BYTES);
        assert!(out.contains("[truncated]"));
        assert!(out.starts_with("x"));
    }
}
