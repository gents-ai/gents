use super::*;

#[derive(Debug, Deserialize)]
struct BackgroundedRow {
    lifecycle_state: Option<String>,
}

pub(super) async fn count_live_backgrounded_rows(
    node: &defra_node::EmbeddedNode,
    request_id: &str,
) -> anyhow::Result<usize> {
    let escaped_request_id = crate::graphql::escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    request_id: {{ _eq: "{escaped_request_id}" }},
                    await_mode: {{ _eq: "background" }}
                }}
            ) {{
                lifecycle_state
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query live backgrounded tool count for request {request_id} failed: {:?}",
            response.errors
        );
    }
    let rows: Vec<BackgroundedRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    Ok(rows
        .into_iter()
        .filter(|row| {
            !matches!(
                row.lifecycle_state.as_deref(),
                Some("completed" | "failed" | "timedOut" | "cancelled")
            )
        })
        .count())
}

pub(super) fn truncation_mode_for(tool_name: &str) -> TruncationMode {
    crate::truncation::tool_result_truncation_mode(tool_name)
}

pub(super) fn bounded_tool_result_for_model(
    tool_name: &str,
    raw_result: &str,
    limits: &crate::truncation::TruncationLimits,
) -> String {
    let (bounded, trigger, truncated) =
        truncate_text(raw_result, truncation_mode_for(tool_name), limits);
    if truncated {
        tracing::warn!(
            tool_name = %tool_name,
            truncated_by = ?trigger,
            original_bytes = raw_result.len(),
            max_lines = limits.max_lines,
            max_bytes = limits.max_bytes,
            "bounded hook-managed tool result before returning it to rig"
        );
    }
    bounded
}

pub(super) fn bounded_tool_result_with_presentation(
    tool_name: &str,
    raw_result: &str,
    limits: &crate::truncation::TruncationLimits,
) -> (String, Option<gents_protocol::output::PayloadPresentation>) {
    use gents_protocol::output::{PayloadPresentation, PresentationPart};

    if let Some(text) =
        gents_loop::loop_stream::replayed_tool_result_text(tool_name, raw_result, limits)
    {
        return (
            text.clone(),
            Some(PayloadPresentation::Composed {
                parts: vec![PresentationPart::Literal { text }],
            }),
        );
    }
    let mode = truncation_mode_for(tool_name);
    let truncated = crate::truncation::truncate(raw_result, mode, limits);
    if !truncated.truncated {
        return (truncated.text, None);
    }
    if truncated.returned_bytes == 0 {
        return (
            truncated.text.clone(),
            Some(PayloadPresentation::Composed {
                parts: vec![PresentationPart::Literal {
                    text: truncated.text,
                }],
            }),
        );
    }
    let selected_bytes = truncated.returned_bytes as u64;
    let (literal, range) = match mode {
        TruncationMode::Head => {
            let selected = &truncated.text[..truncated.returned_bytes];
            if !raw_result.starts_with(selected) {
                return (
                    truncated.text.clone(),
                    Some(PayloadPresentation::Composed {
                        parts: vec![PresentationPart::Literal {
                            text: truncated.text,
                        }],
                    }),
                );
            }
            (
                truncated.text[truncated.returned_bytes..].to_string(),
                PresentationPart::OutputRange {
                    start_byte: 0,
                    end_byte: selected_bytes,
                },
            )
        }
        TruncationMode::Tail => {
            let selected = &truncated.text[truncated.text.len() - truncated.returned_bytes..];
            let Some(start) = raw_result.rfind(selected) else {
                return (
                    truncated.text.clone(),
                    Some(PayloadPresentation::Composed {
                        parts: vec![PresentationPart::Literal {
                            text: truncated.text,
                        }],
                    }),
                );
            };
            (
                truncated.text[..truncated.text.len() - truncated.returned_bytes].to_string(),
                PresentationPart::OutputRange {
                    start_byte: start as u64,
                    end_byte: (start + selected.len()) as u64,
                },
            )
        }
    };
    let parts = match mode {
        TruncationMode::Head => vec![range, PresentationPart::Literal { text: literal }],
        TruncationMode::Tail => vec![PresentationPart::Literal { text: literal }, range],
    };
    (
        truncated.text,
        Some(PayloadPresentation::Composed { parts }),
    )
}

pub(super) fn model_observation_for_tool_result(tool_name: &str, raw_result: &str) -> String {
    if !is_read_file_tool(tool_name) {
        return raw_result.to_string();
    }

    project_read_file_observation(raw_result).unwrap_or_else(|| raw_result.to_string())
}

fn is_read_file_tool(tool_name: &str) -> bool {
    matches!(tool_name, "read_file" | "read" | "cat")
}

fn project_read_file_observation(raw_result: &str) -> Option<String> {
    project_read_file_json_observation(raw_result)
        .or_else(|| project_read_file_compact_observation(raw_result))
}

fn project_read_file_compact_observation(raw_result: &str) -> Option<String> {
    let (first_line, body) = raw_result.split_once('\n')?;
    let metadata = first_line.strip_prefix("gents_fs: ")?;
    let metadata: serde_json::Value = serde_json::from_str(metadata).ok()?;
    if metadata.get("tool").and_then(|value| value.as_str()) != Some("read_file") {
        return None;
    }
    let content = body.strip_prefix("content:\n").unwrap_or(body);
    Some(render_read_file_observation(&metadata, content))
}

fn project_read_file_json_observation(raw_result: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(raw_result.trim()).ok()?;
    let content = value.get("content").and_then(|value| value.as_str())?;
    if !looks_like_read_file_wrapper(&value) {
        return None;
    }
    Some(render_read_file_observation(&value, content))
}

fn looks_like_read_file_wrapper(value: &serde_json::Value) -> bool {
    if value.get("tool").and_then(|value| value.as_str()) == Some("read_file") {
        return true;
    }

    value.get("ok").is_some()
        && value.get("status").is_some()
        && value.get("path").is_some()
        && value.get("start_line").is_some()
        && value.get("end_line").is_some()
}

fn render_read_file_observation(metadata: &serde_json::Value, content: &str) -> String {
    let path = metadata
        .get("path")
        .and_then(|value| value.as_str())
        .filter(|path| !path.trim().is_empty())
        .unwrap_or("file");
    let truncated = metadata
        .get("truncated")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);

    let mut provenance = format!("Read {path}");
    if let (Some(start), Some(end)) = (
        json_usize(metadata, "start_line"),
        json_usize(metadata, "end_line"),
    ) {
        provenance.push_str(&format!(" (lines {start}-{end}"));
        if let Some(total) = json_usize(metadata, "total_count") {
            provenance.push_str(&format!(" of {total}"));
        }
        if truncated {
            provenance.push_str(", truncated");
        }
        provenance.push(')');
    } else if truncated {
        provenance.push_str(" (truncated)");
    }
    provenance.push(':');

    let content = content.trim_end();
    if content.is_empty() {
        provenance
    } else {
        format!("{provenance}\n{content}")
    }
}

fn json_usize(value: &serde_json::Value, key: &str) -> Option<usize> {
    value
        .get(key)
        .and_then(|value| value.as_u64())
        .and_then(|value| usize::try_from(value).ok())
}

pub(super) fn json_string(value: serde_json::Value) -> String {
    crate::tool_output::render(
        &value,
        &[
            "output",
            "result",
            "processes",
            "entries",
            "sessions",
            "agents",
            "message",
            "status",
            "process",
            "error",
            "next_call",
            "next_offset",
            "ok",
        ],
    )
    .expect("JSON values serialize")
}

/// Build a model-facing JSON envelope embedding a possibly oversized result
/// inside the hook's truncation limits. The embedded copy is bound first at
/// half the byte budget (headroom for JSON escaping), and if escaping inflation
/// still overflows the limits, fall back to a stub — the envelope must survive
/// the outer bound intact as valid JSON; the full output stays on the durable
/// AgentToolCall row.
pub(super) fn json_envelope_with_bounded_result(
    mut envelope: serde_json::Value,
    result_key: &str,
    result: &str,
    tool_name: &str,
    limits: &crate::truncation::TruncationLimits,
) -> String {
    let inner_limits = crate::truncation::TruncationLimits {
        max_lines: limits.max_lines,
        max_bytes: limits.max_bytes / 2,
    };
    let (bounded, _, _) = crate::truncation::truncate_text(
        result,
        crate::truncation::tool_result_truncation_mode(tool_name),
        &inner_limits,
    );
    envelope[result_key] = serde_json::Value::String(bounded);
    let rendered = json_string(envelope.clone());
    if rendered.len() <= limits.max_bytes {
        return rendered;
    }
    envelope[result_key] = serde_json::Value::String(format!(
        "[{} bytes omitted from envelope; full output stored on the tool-call row]",
        result.len()
    ));
    json_string(envelope)
}

pub(super) fn invalid_tool_arguments_payload(
    tool_name: &str,
    path: &str,
    message: impl Into<String>,
) -> String {
    json_string(json!({
        "ok": false,
        "failure_class": "invalid_tool_arguments",
        "path": path,
        "message": message.into(),
        "retryable": false,
        "service_id": "session",
        "tool_name": tool_name
    }))
}

pub(super) fn background_invalid_tool_arguments_payload(
    tool_name: &str,
    path: &str,
    message: impl Into<String>,
) -> String {
    json_string(json!({
        "ok": false,
        "failure_class": "argument_invalid",
        "path": path,
        "message": message.into(),
        "retryable": false,
        "service_id": "process",
        "tool_name": tool_name
    }))
}

pub(super) fn background_tool_not_allowed_payload(
    tool_name: &str,
    path: &str,
    requested: &str,
    message: impl Into<String>,
    allowed_targets: Vec<String>,
) -> String {
    json_string(json!({
        "ok": false,
        "failure_class": "tool_not_allowed",
        "path": path,
        "message": message.into(),
        "retryable": false,
        "service_id": "process",
        "tool_name": tool_name,
        "requested_tool_name": requested,
        "allowed_backgroundable_tool_names": allowed_targets
    }))
}

pub(super) fn background_budget_exceeded_payload(current_backgrounded: usize) -> String {
    json_string(json!({
        "ok": false,
        "failure_class": "argument_invalid",
        "code": "background_tool_budget_exceeded",
        "path": "/",
        "message": format!(
            "parent request has reached the concurrent backgrounded tool ceiling ({MAX_BACKGROUNDED_TOOLS_PER_PARENT})"
        ),
        "retryable": false,
        "service_id": "process",
        "tool_name": SPAWN_PROCESS_TOOL_NAME,
        "current_backgrounded": current_backgrounded,
        "max_backgrounded": MAX_BACKGROUNDED_TOOLS_PER_PARENT
    }))
}

pub(super) fn tool_not_allowed_payload(
    tool_name: &str,
    path: &str,
    requested: &str,
    message: impl Into<String>,
    allowed_targets: Vec<String>,
) -> String {
    json_string(json!({
        "ok": false,
        "failure_class": "tool_not_allowed",
        "path": path,
        "message": message.into(),
        "retryable": false,
        "service_id": "session",
        "tool_name": tool_name,
        "requested_tool_name": requested,
        "allowed_agents": allowed_targets
    }))
}

pub(super) fn service_unavailable_payload(
    tool_name: &str,
    path: &str,
    message: impl Into<String>,
    retryable: bool,
) -> String {
    json_string(json!({
        "ok": false,
        "failure_class": "service_unavailable",
        "path": path,
        "message": message.into(),
        "retryable": retryable,
        "service_id": "session",
        "tool_name": tool_name
    }))
}

impl DefraSessionHook {
    pub(super) async fn ensure_assistant_turn_sequence(
        &self,
    ) -> anyhow::Result<(String, String, chrono::DateTime<chrono::Utc>, u32)> {
        let mut state = self.state.lock().await;
        let session_id = state
            .session_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session hook missing session id"))?;
        let request_id = state
            .current_request_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("tool call is missing its active request id"))?;
        if state.current_request_doc_id.is_none() {
            anyhow::bail!("tool call is missing its active request document id");
        }
        let deadline_at = state
            .request_deadline_at
            .ok_or_else(|| anyhow::anyhow!("tool call is missing its request deadline"))?;

        let sequence = state.begin_or_continue_assistant_turn();

        Ok((session_id, request_id, deadline_at, sequence))
    }
}
