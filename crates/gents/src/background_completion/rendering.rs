use super::*;

pub(super) fn render_notification(edge: &ChildEdge, status: &str, summary: &str) -> String {
    format!(
        r#"<subagent-notification child_request_id="{child_request_id}" child_session_id="{child_session_id}" behavior_id="{behavior_id}" parent_tool_call_id="{parent_tool_call_id}" status="{status}">
<summary>{summary}</summary>
</subagent-notification>"#,
        child_request_id = xml_escape_attr(&edge.child_request_id),
        child_session_id = xml_escape_attr(&edge.child_session_id),
        behavior_id = xml_escape_attr(&edge.behavior_id),
        parent_tool_call_id = xml_escape_attr(&edge.parent_tool_call_id),
        status = xml_escape_attr(status),
        summary = xml_escape_text(summary),
    )
}

pub(super) fn render_tool_completion(
    tool_call_id: &str,
    tool_name: &str,
    status: &str,
    result: &str,
    reason: Option<&str>,
) -> String {
    let reason_element = reason
        .map(|reason| format!("\n  <reason>{}</reason>", xml_escape_text(reason)))
        .unwrap_or_default();
    format!(
        r#"<tool-completion tool_call_id="{tool_call_id}" tool_name="{tool_name}" status="{status}">
  <result>{result}</result>{reason_element}
</tool-completion>"#,
        tool_call_id = xml_escape_attr(tool_call_id),
        tool_name = xml_escape_attr(tool_name),
        status = xml_escape_attr(status),
        result = xml_escape_text(&compact_summary(result)),
        reason_element = reason_element,
    )
}

pub(super) fn child_terminal_status(terminal: &ChildTerminal) -> &'static str {
    match terminal {
        ChildTerminal::Failed { .. } => "failed",
        ChildTerminal::Dead => "dead",
        ChildTerminal::Interrupted => "interrupted",
        ChildTerminal::Superseded => "superseded",
    }
}

pub(super) fn child_terminal_summary(terminal: &ChildTerminal) -> String {
    match terminal {
        ChildTerminal::Failed { reason, .. } => compact_summary(reason),
        ChildTerminal::Dead => "child request reached the dead terminal state".to_string(),
        ChildTerminal::Interrupted => "child request was interrupted".to_string(),
        ChildTerminal::Superseded => "child request was superseded".to_string(),
    }
}

pub(super) fn compact_summary(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    const LIMIT: usize = 4000;
    if normalized.len() <= LIMIT {
        return normalized;
    }

    let boundary = normalized
        .char_indices()
        .map(|(idx, _)| idx)
        .take_while(|idx| *idx <= LIMIT)
        .last()
        .unwrap_or(0);
    let mut truncated = normalized[..boundary].to_string();
    truncated.push_str("...");
    truncated
}

pub(super) fn xml_escape_attr(value: &str) -> String {
    xml_escape_text(value)
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(super) fn xml_escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub(super) fn non_empty(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

pub(super) fn first_row<T>(data: Option<&serde_json::Value>, collection: &str) -> Option<T>
where
    T: for<'de> Deserialize<'de>,
{
    data.and_then(|data| data.get(collection))
        .and_then(|value| serde_json::from_value::<Vec<T>>(value.clone()).ok())
        .and_then(|mut rows| rows.pop())
}
