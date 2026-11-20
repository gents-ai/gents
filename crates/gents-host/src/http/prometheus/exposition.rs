use chrono::{DateTime, Utc};

pub(super) fn push_metric_prelude(lines: &mut Vec<String>, name: &str, help: &str) {
    push_metric_prelude_with_type(lines, name, help, "gauge");
}

pub(super) fn push_metric_prelude_with_type(
    lines: &mut Vec<String>,
    name: &str,
    help: &str,
    kind: &str,
) {
    lines.push(format!("# HELP {name} {help}"));
    lines.push(format!("# TYPE {name} {kind}"));
}

pub(super) fn push_metric_sample(
    lines: &mut Vec<String>,
    name: &str,
    labels: &[(&str, String)],
    value: impl std::fmt::Display,
) {
    lines.push(format!("{name}{} {value}", format_metric_labels(labels),));
}

pub(super) fn format_metric_labels(labels: &[(&str, String)]) -> String {
    if labels.is_empty() {
        return String::new();
    }
    let rendered = labels
        .iter()
        .map(|(key, value)| format!(r#"{key}="{}""#, escape_prometheus_label(value)))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{rendered}}}")
}

fn escape_prometheus_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

pub(super) fn rfc3339_timestamp_seconds(value: &str) -> Option<i64> {
    rfc3339_timestamp(value).map(|timestamp| timestamp.timestamp())
}

pub(super) fn rfc3339_timestamp(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}
