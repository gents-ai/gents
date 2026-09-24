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

/// Build the identical rendered notification while retaining unchanged result
/// bytes as references to the canonical tool-output stream. Only wrappers,
/// collapsed whitespace, XML entities and the truncation marker are literals.
pub(super) fn tool_completion_presentation(
    tool_call_id: &str,
    tool_name: &str,
    status: &str,
    result: &str,
    reason: Option<&str>,
) -> (String, Vec<gents_protocol::output::PresentationPart>) {
    use gents_protocol::output::PresentationPart;

    let prefix = format!(
        "<tool-completion tool_call_id=\"{}\" tool_name=\"{}\" status=\"{}\">\n  <result>",
        xml_escape_attr(tool_call_id),
        xml_escape_attr(tool_name),
        xml_escape_attr(status),
    );
    let reason_element = reason
        .map(|reason| format!("\n  <reason>{}</reason>", xml_escape_text(reason)))
        .unwrap_or_default();
    let suffix = format!("</result>{reason_element}\n</tool-completion>");
    let mut parts = vec![PresentationPart::Literal {
        text: prefix.clone(),
    }];
    let mut rendered_result = String::new();
    let mut normalized_bytes = 0usize;
    let mut saw_text = false;
    let mut pending_space = false;
    let mut truncated = false;
    let mut range_start = None::<usize>;
    let mut range_end = 0usize;
    let flush_range = |parts: &mut Vec<PresentationPart>, start: &mut Option<usize>, end: usize| {
        if let Some(start) = start.take() {
            parts.push(PresentationPart::OutputRange {
                start_byte: start as u64,
                end_byte: end as u64,
            });
        }
    };
    for (index, ch) in result.char_indices() {
        if ch.is_whitespace() {
            pending_space |= saw_text;
            continue;
        }
        let width = ch.len_utf8();
        let separator = usize::from(pending_space && saw_text);
        if normalized_bytes + separator + width > 4000 {
            truncated = true;
            break;
        }
        if separator == 1 {
            flush_range(&mut parts, &mut range_start, range_end);
            rendered_result.push(' ');
            parts.push(PresentationPart::Literal { text: " ".into() });
            normalized_bytes += 1;
        }
        pending_space = false;
        saw_text = true;
        normalized_bytes += width;
        match ch {
            '&' | '<' | '>' | '"' | '\'' => {
                flush_range(&mut parts, &mut range_start, range_end);
                let escaped = xml_escape_text(&ch.to_string());
                rendered_result.push_str(&escaped);
                parts.push(PresentationPart::Literal { text: escaped });
            }
            _ => {
                rendered_result.push(ch);
                if range_start.is_none() {
                    range_start = Some(index);
                }
                range_end = index + width;
            }
        }
    }
    flush_range(&mut parts, &mut range_start, range_end);
    if truncated {
        rendered_result.push_str("...");
        parts.push(PresentationPart::Literal { text: "...".into() });
    }
    parts.push(PresentationPart::Literal {
        text: suffix.clone(),
    });
    (format!("{prefix}{rendered_result}{suffix}"), parts)
}

pub(super) fn subagent_notification_presentation(
    edge: &ChildEdge,
    status: &str,
    summary: &str,
    source: &str,
) -> (String, Vec<gents_protocol::output::PresentationPart>) {
    use gents_protocol::output::PresentationPart;
    let rendered = render_notification(edge, status, summary);
    let prefix = format!(
        r#"<subagent-notification child_request_id="{}" child_session_id="{}" behavior_id="{}" parent_tool_call_id="{}" status="{}">
<summary>"#,
        xml_escape_attr(&edge.child_request_id),
        xml_escape_attr(&edge.child_session_id),
        xml_escape_attr(&edge.behavior_id),
        xml_escape_attr(&edge.parent_tool_call_id),
        xml_escape_attr(status),
    );
    let suffix = "</summary>\n</subagent-notification>".to_string();
    let mut parts = vec![PresentationPart::Literal { text: prefix }];
    if compact_summary(source) == summary {
        let (_, mut source_parts) = tool_completion_presentation("", "", "", source, None);
        source_parts.remove(0);
        source_parts.pop();
        parts.extend(source_parts);
    } else {
        // Failure summaries may be synthesized by the verified child-terminal
        // owner rather than copied from tool output. They remain small wrapper
        // metadata while the header still proves the exact bridge source.
        parts.push(PresentationPart::Literal {
            text: xml_escape_text(summary),
        });
    }
    parts.push(PresentationPart::Literal { text: suffix });
    (rendered, parts)
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

#[cfg(test)]
mod canonical_presentation_tests {
    use super::*;

    #[test]
    fn composed_tool_completion_matches_existing_renderer() {
        let long = "é".repeat(2_100);
        for result in [
            "plain output",
            "  spaced\n\toutput  ",
            "<&> \"quoted\" 'single' ✓",
            &long,
        ] {
            let (rendered, _) = tool_completion_presentation(
                "call<&>",
                "bash",
                "failed",
                result,
                Some("reason<&>"),
            );
            assert_eq!(
                rendered,
                render_tool_completion("call<&>", "bash", "failed", result, Some("reason<&>"))
            );
        }
    }

    #[test]
    fn composed_subagent_notification_matches_existing_renderer() {
        let edge = ChildEdge {
            parent_request_id: "parent".into(),
            parent_request_doc_id: "parent-doc".into(),
            parent_agent_did: "did:test:parent".into(),
            parent_requester_did: None,
            parent_tool_call_id: "call<&>".into(),
            parent_tool_call_doc_id: "tool-doc".into(),
            parent_session_id: "parent-session".into(),
            child_request_id: "child<&>".into(),
            child_request_doc_id: "child-doc".into(),
            child_session_id: "session".into(),
            child_agent_did: "did:test:child".into(),
            child_requester_did: None,
            behavior_id: "behavior".into(),
            await_mode: crate::tool_call_lifecycle::AwaitMode::Background,
            lifecycle_state: "completed".into(),
        };
        let source = "  child\nresult <&> ✓ ";
        let summary = compact_summary(source);
        let (rendered, _) =
            subagent_notification_presentation(&edge, "completed", &summary, source);
        assert_eq!(rendered, render_notification(&edge, "completed", &summary));
    }
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

#[cfg(test)]
pub(super) fn first_row<T>(data: Option<&serde_json::Value>, collection: &str) -> Option<T>
where
    T: for<'de> Deserialize<'de>,
{
    data.and_then(|data| data.get(collection))
        .and_then(|value| serde_json::from_value::<Vec<T>>(value.clone()).ok())
        .and_then(|mut rows| rows.pop())
}
