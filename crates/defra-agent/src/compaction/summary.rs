use anyhow::{Context, Result};
use serde::Deserialize;

pub(super) fn compaction_prompt() -> &'static str {
    "Summarize the earlier conversation turns immediately before this message. \
Return JSON only with keys: summary (string), files_read (array of strings), \
files_modified (array of strings), key_decisions (array of strings), \
pending_questions (array of strings). Preserve concrete facts, file paths, \
unfinished work, and major findings. Do not invent tool results."
}

pub(super) fn parse_summary_response(raw_summary: &str) -> Result<SummaryResponse> {
    let trimmed = raw_summary.trim();
    let json = trimmed
        .strip_prefix("```json")
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim)
        .or_else(|| {
            trimmed
                .strip_prefix("```")
                .and_then(|value| value.strip_suffix("```"))
                .map(str::trim)
        })
        .unwrap_or(trimmed);

    let mut summary: SummaryResponse = serde_json::from_str(json)
        .with_context(|| format!("parsing compaction summary response: {json}"))?;
    dedupe_paths(&mut summary.files_read);
    dedupe_paths(&mut summary.files_modified);
    Ok(summary)
}

pub(super) fn format_summary(
    narrative: &str,
    files_read: &[String],
    files_modified: &[String],
    key_decisions: &[String],
    pending_questions: &[String],
) -> String {
    let mut sections = vec![narrative.trim().to_string()];

    if !files_read.is_empty() {
        sections.push(format!(
            "Files read:\n{}",
            files_read
                .iter()
                .map(|item| format!("- {}", item.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    if !files_modified.is_empty() {
        sections.push(format!(
            "Files modified:\n{}",
            files_modified
                .iter()
                .map(|item| format!("- {}", item.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    if !key_decisions.is_empty() {
        sections.push(format!(
            "Key decisions and findings:\n{}",
            key_decisions
                .iter()
                .map(|item| format!("- {}", item.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    if !pending_questions.is_empty() {
        sections.push(format!(
            "Pending questions:\n{}",
            pending_questions
                .iter()
                .map(|item| format!("- {}", item.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    sections
        .into_iter()
        .filter(|section| !section.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(super) fn dedupe_paths(paths: &mut Vec<String>) {
    paths.sort();
    paths.dedup();
}

#[derive(Debug, Deserialize)]
pub(super) struct SummaryResponse {
    pub summary: String,
    #[serde(default)]
    pub files_read: Vec<String>,
    #[serde(default)]
    pub files_modified: Vec<String>,
    #[serde(default)]
    pub key_decisions: Vec<String>,
    #[serde(default)]
    pub pending_questions: Vec<String>,
}
