use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use defra_agent_protocol::row::ScheduledTaskRow;

pub fn normalize_optional_owned(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed.to_string())
}

pub fn normalize_required<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    let trimmed = value.trim();
    (!trimmed.is_empty())
        .then_some(trimmed)
        .with_context(|| format!("{field} must not be empty"))
}

pub fn parse_optional_i64(field: &str, value: &str) -> Result<Option<i64>> {
    match normalize_optional_owned(value) {
        Some(value) => value
            .parse::<i64>()
            .map(Some)
            .map_err(|error| anyhow!("{field} must be an integer: {error}")),
        None => Ok(None),
    }
}

pub fn parse_required_positive_i64(field: &str, value: &str) -> Result<i64> {
    let value = normalize_required(field, value)?;
    let parsed = value
        .parse::<i64>()
        .map_err(|error| anyhow!("{field} must be an integer: {error}"))?;
    if parsed <= 0 {
        return Err(anyhow!("{field} must be greater than zero"));
    }
    Ok(parsed)
}

pub fn parse_optional_f64(field: &str, value: &str) -> Result<Option<f64>> {
    match normalize_optional_owned(value) {
        Some(value) => value
            .parse::<f64>()
            .map(Some)
            .map_err(|error| anyhow!("{field} must be a number: {error}")),
        None => Ok(None),
    }
}

pub fn parse_optional_rfc3339(field: &str, value: &str) -> Result<Option<String>> {
    match normalize_optional_owned(value) {
        Some(value) => DateTime::parse_from_rfc3339(&value)
            .map(|parsed| Some(parsed.with_timezone(&Utc).to_rfc3339()))
            .map_err(|error| anyhow!("{field} must be RFC3339: {error}")),
        None => Ok(None),
    }
}

pub fn split_csv(value: &str) -> Vec<String> {
    value
        .split([',', '\n'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

pub fn bool_word(value: Option<bool>) -> &'static str {
    if value == Some(true) {
        "on"
    } else {
        "off"
    }
}

pub fn scheduled_task_is_due(row: &ScheduledTaskRow) -> bool {
    if row.enabled == Some(false) {
        return false;
    }

    match row.next_run_at.as_deref().and_then(parse_task_timestamp) {
        Some(next_run_at) => Utc::now() >= next_run_at,
        None => true,
    }
}

pub fn scheduled_task_next_run_label(row: &ScheduledTaskRow) -> String {
    match row.next_run_at.as_deref().and_then(parse_task_timestamp) {
        Some(next_run_at) if Utc::now() >= next_run_at => "now".to_string(),
        Some(next_run_at) => next_run_at.format("%Y-%m-%d %H:%MZ").to_string(),
        None => "now".to_string(),
    }
}

pub fn summarize_request_content(content: &str, fallback_id: &str) -> String {
    let normalized = truncate_line(content, 72);
    if normalized.is_empty() {
        abbreviate_identifier(fallback_id)
    } else {
        normalized
    }
}

pub fn truncate_line(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        normalized
    } else {
        let mut truncated = normalized.chars().take(max_chars).collect::<String>();
        truncated.push_str("...");
        truncated
    }
}

pub fn abbreviate_identifier(value: &str) -> String {
    if value.len() <= 10 {
        value.to_string()
    } else {
        format!("{}..{}", &value[..6], &value[value.len() - 2..])
    }
}

pub fn compact_timestamp(value: &str) -> String {
    parse_task_timestamp(value)
        .map(|timestamp| timestamp.format("%Y-%m-%d %H:%MZ").to_string())
        .unwrap_or_else(|| "time unknown".to_string())
}

fn parse_task_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_csv_trims_commas_and_lines() {
        assert_eq!(
            split_csv("alpha, beta\n gamma "),
            vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]
        );
    }

    #[test]
    fn parse_optional_numbers_accepts_empty() {
        assert_eq!(parse_optional_i64("x", "").unwrap(), None);
        assert_eq!(parse_optional_f64("x", "").unwrap(), None);
    }

    #[test]
    fn parse_optional_rfc3339_accepts_empty_and_normalizes() {
        assert_eq!(parse_optional_rfc3339("x", "").unwrap(), None);
        assert_eq!(
            parse_optional_rfc3339("x", "2026-04-14T01:02:03+00:00").unwrap(),
            Some("2026-04-14T01:02:03+00:00".to_string())
        );
    }

    #[test]
    fn scheduled_task_due_defaults_true_without_next_run() {
        let row = ScheduledTaskRow {
            task_id: "task-1".to_string(),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            name: Some("Task".to_string()),
            prompt: Some("Prompt".to_string()),
            interval_secs: Some(60),
            enabled: Some(true),
            next_run_at: None,
            last_run_at: None,
            last_status: None,
            last_error: None,
            run_count: Some(0),
            created_at: None,
            updated_at: None,
        };

        assert!(scheduled_task_is_due(&row));
        assert_eq!(scheduled_task_next_run_label(&row), "now");
    }
}
