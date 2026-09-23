use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::time::Duration;

use anyhow::{Context, Result};
use gents::graphql::escape_graphql_string;
use gents_protocol::client_protocol::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde_json::Value;

use crate::{
    observe_canonical_request_output, post_graphql, request_diagnostic_hint,
    request_output_envelope, RequestOutputEnvelope,
};

use super::SubmittedRequest;

#[derive(Debug, Clone)]
pub(super) struct ToolCallProgress {
    pub(super) tool_call_doc_id: String,
    pub(super) tool_call_key: String,
    pub(super) tool_name: String,
    pub(super) status: String,
    pub(super) arguments: String,
    pub(super) result: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ChatProgressMarker {
    request_lifecycle_state: Option<RequestLifecycleState>,
    request_failure_reason: Option<String>,
    request_lease_expires_at: Option<String>,
    request_interrupt_requested_at: Option<String>,
    request_valid_until: Option<String>,
    canonical_output: Option<ChatOutputProgress>,
    tools: Vec<ChatToolProgressMarker>,
}

/// Streaming-relevant canonical output observation. Only presentation-carrying
/// variants enter change detection and the reasoning indicator; retained
/// partials are historical diagnostics and never enter either.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ChatOutputProgress {
    kind: &'static str,
    body_markdown: String,
    reasoning_markdown: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ChatToolProgressMarker {
    tool_call_key: Option<String>,
    status: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
}

pub(super) fn chat_progress_query(request: &SubmittedRequest) -> String {
    let request_doc_id = &request.request_doc_id;
    let scope = gents::session::session_scope_filter(
        &request.agent_did,
        &request.session_id,
        request.requester_did.as_deref(),
    );
    format!(
        r#"{{
            AgentRequest(
                filter: {{ {scope}, _docID: {{ _eq: "{request_doc_id}" }} }},
                order: {{ created_at: DESC }},
                limit: 2
            ) {{
                _docID agent_did requester_did session_id
                request_id
                lifecycle_state
                failure_reason
                execution_generation
                execution_lease_secs
                execution_lease_expires_at
                terminal_output
                terminalized_at
                interrupt_requested_at
                valid_until
            }}
            AgentToolCall(
                filter: {{
                    {scope},
                    request_doc_id: {{ _eq: "{request_doc_id}" }}
                }},
                order: {{ started_at: ASC }}
            ) {{
                _docID
                tool_call_key
                tool_name
                status
                started_at
                completed_at
            }}
        }}"#,
        request_doc_id = escape_graphql_string(request_doc_id),
    )
}

pub(super) async fn load_existing_tool_call_keys(
    graphql: &str,
    session_id: &str,
) -> Result<std::collections::BTreeMap<String, String>> {
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }}
            ) {{
                tool_call_key
                status
            }}
        }}"#,
        session_id = escape_graphql_string(session_id),
    );
    let response = post_graphql(graphql, &query).await?;
    let rows = response
        .pointer("/data/AgentToolCall")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    Ok(rows
        .into_iter()
        .filter_map(|row| {
            Some((
                row.get("tool_call_key")?.as_str()?.to_string(),
                row.get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ))
        })
        .collect())
}

pub(super) async fn stream_turn_progress(
    graphql: &str,
    submitted: &SubmittedRequest,
    mut known_tool_calls: std::collections::BTreeMap<String, String>,
    timeout_secs: u64,
    poll_secs: u64,
) -> Result<RequestOutputEnvelope> {
    let idle_timeout = Duration::from_secs(timeout_secs);
    let mut last_progress_at = tokio::time::Instant::now();
    let mut latest_progress_marker: Option<ChatProgressMarker> = None;
    let mut thinking_printed = false;
    let mut tool_output_fingerprints = std::collections::BTreeMap::new();

    loop {
        let query = chat_progress_query(submitted);
        let response = post_graphql(graphql, &query).await?;
        let rows = response
            .pointer("/data/AgentRequest")
            .and_then(Value::as_array)
            .context("chat request query omitted rows")?;
        anyhow::ensure!(
            rows.len() == 1,
            "committed chat physical request missing or ambiguous"
        );
        let request_row = rows.first().cloned();
        let request = request_row
            .as_ref()
            .map(|row| {
                serde_json::from_value::<AgentRequestRow>(row.clone())
                    .context("decoding chat progress AgentRequest row")
            })
            .transpose()?;

        if let Some(request) = request.as_ref() {
            anyhow::ensure!(
                request.agent_did.as_deref() == Some(submitted.agent_did.as_str())
                    && request.requester_did == submitted.requester_did
                    && request.session_id.as_deref() == Some(submitted.session_id.as_str()),
                "chat request scope differs from committed receipt"
            );
        }

        let mut observed_output = match request.as_ref() {
            Some(request) => Some(observe_canonical_request_output(graphql, request).await?),
            None => None,
        };

        let tool_rows = response
            .pointer("/data/AgentToolCall")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut terminal_tool_dependency_pending = false;
        for mut tool in tool_rows.iter().filter_map(decode_tool_call_progress) {
            let terminal = matches!(tool.status.as_str(), "completed" | "error");
            let presentation = crate::load_canonical_tool_presentation(
                graphql,
                &tool.tool_call_doc_id,
                submitted,
                terminal,
            )
            .await?;
            tool.arguments = presentation.arguments;
            if terminal && presentation.result.is_none() {
                terminal_tool_dependency_pending = true;
                tool.result = presentation.live_output;
                let output_fingerprint = tool.result.as_deref().map(text_fingerprint);
                let previous_fingerprint = tool_output_fingerprints
                    .get(&tool.tool_call_key)
                    .copied()
                    .flatten();
                if previous_fingerprint != output_fingerprint {
                    tool_output_fingerprints.insert(tool.tool_call_key.clone(), output_fingerprint);
                    last_progress_at = tokio::time::Instant::now();
                    println!("{}", format_live_tool_progress_line(&tool));
                    io::stdout().flush()?;
                }
                continue;
            }
            tool.result = presentation.result.or(presentation.live_output);
            let output_fingerprint = tool.result.as_deref().map(text_fingerprint);
            let previous_status = known_tool_calls.get(&tool.tool_call_key).cloned();
            let previous_fingerprint = tool_output_fingerprints
                .get(&tool.tool_call_key)
                .copied()
                .flatten();
            if previous_status.as_deref() == Some(tool.status.as_str())
                && previous_fingerprint == output_fingerprint
            {
                continue;
            }
            tool_output_fingerprints.insert(tool.tool_call_key.clone(), output_fingerprint);
            known_tool_calls.insert(tool.tool_call_key.clone(), tool.status.clone());
            last_progress_at = tokio::time::Instant::now();
            if previous_status.is_none() && matches!(tool.status.as_str(), "completed" | "error") {
                println!(
                    "[tool] {} {}",
                    tool.tool_name,
                    format_tool_args_preview(&tool.arguments)
                );
            }
            println!("{}", format_tool_progress_line(&tool));
            io::stdout().flush()?;
        }

        if terminal_tool_dependency_pending {
            if last_progress_at.elapsed() >= idle_timeout {
                anyhow::bail!(
                    "timed out waiting for canonical tool output for request {} after {}s of inactivity\n{}",
                    submitted.request_id,
                    timeout_secs,
                    request_diagnostic_hint(&submitted.request_id)
                );
            }
            tokio::time::sleep(Duration::from_secs(poll_secs)).await;
            continue;
        }

        let lifecycle_state = request.as_ref().and_then(|row| row.lifecycle_state);
        let failure_reason = request
            .as_ref()
            .and_then(|row| row.failure_reason.as_deref())
            .unwrap_or("");
        let marker = chat_progress_marker(request.as_ref(), observed_output.as_ref(), &tool_rows);
        if latest_progress_marker.as_ref() != Some(&marker) {
            latest_progress_marker = Some(marker);
            last_progress_at = tokio::time::Instant::now();
        }
        if let Some(output) = observed_output.as_ref() {
            if !thinking_printed && should_print_thinking(lifecycle_state, output) {
                println!("[thinking]");
                io::stdout().flush()?;
                thinking_printed = true;
            }
        }

        let terminal_by_request = lifecycle_state.is_some_and(RequestLifecycleState::is_terminal);
        if terminal_by_request {
            let request = request
                .as_ref()
                .context("terminal chat request lost its row before exact selection")?;
            let output = observed_output
                .take()
                .context("terminal chat request output observation is missing")?;
            match output {
                gents::session::CanonicalRequestOutput::Loading => {
                    if last_progress_at.elapsed() >= idle_timeout {
                        anyhow::bail!(
                            "timed out waiting for materialized AgentMessage {} after {}s of inactivity\n{}",
                            submitted.request_id,
                            timeout_secs,
                            request_diagnostic_hint(&submitted.request_id)
                        );
                    }
                    tokio::time::sleep(Duration::from_secs(poll_secs)).await;
                    continue;
                }
                gents::session::CanonicalRequestOutput::Denied => {
                    anyhow::bail!("canonical terminal output for request {} is denied", submitted.request_id)
                }
                gents::session::CanonicalRequestOutput::Conflicted => anyhow::bail!(
                    "canonical terminal output for request {} is conflicted",
                    submitted.request_id
                ),
                gents::session::CanonicalRequestOutput::Invalid => {
                    anyhow::bail!(
                        "canonical terminal output for request {} is invalid",
                        submitted.request_id
                    )
                }
                gents::session::CanonicalRequestOutput::Retracted => anyhow::bail!(
                    "canonical terminal output for request {} is retracted",
                    submitted.request_id
                ),
                gents::session::CanonicalRequestOutput::RetainedPartial(_) => anyhow::bail!(
                    "canonical terminal output for request {} resolved only to retained partial diagnostics",
                    submitted.request_id
                ),
                gents::session::CanonicalRequestOutput::TerminalNoMessage
                | gents::session::CanonicalRequestOutput::TerminalMessage { .. } => {
                    let envelope = request_output_envelope(request, output)?;
                    let presentation = match &envelope.output {
                        crate::CliOutputObservation::TerminalMessage { presentation, .. } => {
                            Some(presentation)
                        }
                        crate::CliOutputObservation::TerminalNoMessage => None,
                        _ => unreachable!("exact terminal selection produced another variant"),
                    };
                    if let Some(presentation) = presentation {
                        if !presentation.body_markdown.trim().is_empty() {
                            println!("{}", presentation.body_markdown);
                            io::stdout().flush()?;
                        }
                    }

                    let error_message = failure_reason.trim();
                    if !error_message.is_empty() {
                        println!("[agent error] {error_message}");
                        println!("[inspect] gents response show {}", submitted.request_id);
                        io::stdout().flush()?;
                    } else if matches!(
                        envelope.request.lifecycle_state,
                        RequestLifecycleState::Failed | RequestLifecycleState::Dead
                    ) {
                        println!("[inspect] gents response show {}", submitted.request_id);
                        io::stdout().flush()?;
                    }

                    return Ok(envelope);
                }
                gents::session::CanonicalRequestOutput::Absent
                | gents::session::CanonicalRequestOutput::Live(_)
                | gents::session::CanonicalRequestOutput::Settling(_)
                | gents::session::CanonicalRequestOutput::Published { .. } => {
                    // Lifecycle and canonical publication replicate separately.
                    // Published is useful preview state, but only the request's
                    // exact terminal selection may settle this turn.
                    if last_progress_at.elapsed() >= idle_timeout {
                        anyhow::bail!(
                            "timed out waiting for terminal canonical selection for request {} after {}s of inactivity\n{}",
                            submitted.request_id,
                            timeout_secs,
                            request_diagnostic_hint(&submitted.request_id)
                        );
                    }
                    tokio::time::sleep(Duration::from_secs(poll_secs)).await;
                    continue;
                }
            }
        }

        if last_progress_at.elapsed() >= idle_timeout {
            anyhow::bail!(
                "timed out waiting for canonical output for request {} after {}s of inactivity\n{}",
                submitted.request_id,
                timeout_secs,
                request_diagnostic_hint(&submitted.request_id)
            );
        }

        tokio::time::sleep(Duration::from_secs(poll_secs)).await;
    }
}

fn chat_output_progress(
    output: &gents::session::CanonicalRequestOutput,
) -> Option<ChatOutputProgress> {
    let (kind, presentation) = match output {
        gents::session::CanonicalRequestOutput::Live(value) => ("live", value),
        gents::session::CanonicalRequestOutput::Settling(value) => ("settling", value),
        gents::session::CanonicalRequestOutput::Published { presentation, .. } => {
            ("published", presentation)
        }
        // Retained partials are historical diagnostics, never streaming answer
        // progress; Loading/absence carry no new bytes either.
        _ => return None,
    };
    Some(ChatOutputProgress {
        kind,
        body_markdown: presentation.body_markdown.clone(),
        reasoning_markdown: presentation.reasoning_markdown.clone(),
    })
}

fn chat_progress_marker(
    request_row: Option<&AgentRequestRow>,
    output: Option<&gents::session::CanonicalRequestOutput>,
    tool_rows: &[Value],
) -> ChatProgressMarker {
    ChatProgressMarker {
        request_lifecycle_state: request_row.and_then(|row| row.lifecycle_state),
        request_failure_reason: request_row.and_then(|row| row.failure_reason.clone()),
        request_lease_expires_at: request_row
            .and_then(|row| row.execution_lease_expires_at.clone()),
        request_interrupt_requested_at: request_row
            .and_then(|row| row.interrupt_requested_at.clone()),
        request_valid_until: request_row.and_then(|row| row.valid_until.clone()),
        canonical_output: output.and_then(chat_output_progress),
        tools: tool_rows.iter().map(chat_tool_progress_marker).collect(),
    }
}

fn chat_tool_progress_marker(row: &Value) -> ChatToolProgressMarker {
    ChatToolProgressMarker {
        tool_call_key: scalar_marker(Some(row), "tool_call_key"),
        status: scalar_marker(Some(row), "status"),
        started_at: scalar_marker(Some(row), "started_at"),
        completed_at: scalar_marker(Some(row), "completed_at"),
    }
}

/// Reasoning has only begun once the request is actively processing and the
/// canonical observation carries reasoning presentation without published body
/// bytes yet.
fn should_print_thinking(
    lifecycle_state: Option<RequestLifecycleState>,
    output: &gents::session::CanonicalRequestOutput,
) -> bool {
    let (body, reasoning) = match chat_output_progress(output) {
        Some(progress) => (progress.body_markdown, progress.reasoning_markdown),
        None => return false,
    };
    lifecycle_state == Some(RequestLifecycleState::Processing)
        && body.trim().is_empty()
        && reasoning
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
}

fn scalar_marker(row: Option<&Value>, field: &str) -> Option<String> {
    let value = row?.get(field)?;
    if value.is_null() {
        return None;
    }
    value
        .as_str()
        .map(ToOwned::to_owned)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .or_else(|| value.as_bool().map(|value| value.to_string()))
}

pub(super) fn decode_tool_call_progress(row: &Value) -> Option<ToolCallProgress> {
    Some(ToolCallProgress {
        tool_call_doc_id: row.get("_docID")?.as_str()?.to_string(),
        tool_call_key: row.get("tool_call_key")?.as_str()?.to_string(),
        tool_name: row.get("tool_name")?.as_str()?.to_string(),
        status: row.get("status")?.as_str()?.to_string(),
        arguments: String::new(),
        result: None,
    })
}

pub(super) fn format_tool_progress_line(tool: &ToolCallProgress) -> String {
    match tool.status.as_str() {
        "completed" => match tool.result.as_deref().and_then(preview_compact_text) {
            Some(result) => format!(
                "[tool done] {} {} => {}",
                tool.tool_name,
                format_tool_args_preview(&tool.arguments),
                result
            ),
            None => format!(
                "[tool done] {} {}",
                tool.tool_name,
                format_tool_args_preview(&tool.arguments)
            ),
        },
        "error" => format!(
            "[tool error] {} {} => {}",
            tool.tool_name,
            format_tool_args_preview(&tool.arguments),
            tool.result
                .as_deref()
                .and_then(preview_compact_text)
                .unwrap_or_else(|| "-".to_string())
        ),
        _ => match tool.result.as_deref().and_then(preview_compact_text) {
            Some(output) => format!(
                "[tool] {} {} => {}",
                tool.tool_name,
                format_tool_args_preview(&tool.arguments),
                output
            ),
            None => format!(
                "[tool] {} {}",
                tool.tool_name,
                format_tool_args_preview(&tool.arguments)
            ),
        },
    }
}

fn format_live_tool_progress_line(tool: &ToolCallProgress) -> String {
    match tool.result.as_deref().and_then(preview_compact_text) {
        Some(output) => format!(
            "[tool] {} {} => {}",
            tool.tool_name,
            format_tool_args_preview(&tool.arguments),
            output
        ),
        None => format!(
            "[tool] {} {}",
            tool.tool_name,
            format_tool_args_preview(&tool.arguments)
        ),
    }
}

pub(super) fn format_tool_args_preview(value: &str) -> String {
    preview_compact_text(value)
        .map(|preview| format!("({preview})"))
        .unwrap_or_default()
}

pub(super) fn preview_compact_text(value: &str) -> Option<String> {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = compact.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(if trimmed.chars().count() > 120 {
        format!("{}...", trimmed.chars().take(120).collect::<String>())
    } else {
        trimmed.to_string()
    })
}

fn text_fingerprint(value: &str) -> (usize, u64) {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    (value.len(), hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_progress_query_omits_retired_tool_payload_fields() {
        let submitted = SubmittedRequest {
            request_id: "request-one".to_string(),
            session_id: "session-one".to_string(),
            agent_did: "did:key:agent".to_string(),
            behavior_id: None,
            request_doc_id: "doc-one".to_string(),
            requester_did: Some("did:key:requester".to_string()),
            input: None,
            created_at: None,
        };

        let query = chat_progress_query(&submitted);

        // Retired AgentToolCall storage fields must not be queried; the SDL
        // no longer defines them (contracts/canonical-output.md deletions).
        assert!(!query.contains(" args"));
        assert!(!query.contains(" result"));
        // Surviving tool lifecycle fields and canonical request fields remain.
        assert!(query.contains("tool_call_key"));
        assert!(query.contains("_docID"));
        assert!(query.contains("tool_name"));
        assert!(query.contains("status"));
        assert!(query.contains("started_at"));
        assert!(query.contains("completed_at"));
        assert!(query.contains("terminalized_at"));
        assert!(query.contains("terminal_output"));
    }

    #[test]
    fn canonical_tool_presentation_retains_compact_arguments_and_result() {
        let tool = ToolCallProgress {
            tool_call_doc_id: "physical-tool".into(),
            tool_call_key: "tool-key".into(),
            tool_name: "bash".into(),
            status: "completed".into(),
            arguments: "{\n  \"command\": \"printf hi\"\n}".into(),
            result: Some("hi\nthere".into()),
        };
        assert_eq!(
            format_tool_progress_line(&tool),
            "[tool done] bash ({ \"command\": \"printf hi\" }) => hi there"
        );
    }

    #[test]
    fn compact_preview_is_unicode_safe_and_bounded() {
        let preview = preview_compact_text(&"考".repeat(121)).expect("nonempty preview");
        assert_eq!(preview.chars().count(), 123);
        assert!(preview.ends_with("..."));
    }

    #[test]
    fn thinking_indicator_follows_active_lifecycle_and_projected_reasoning() {
        let reasoning_only =
            gents::session::CanonicalRequestOutput::Live(gents::session::CanonicalPresentation {
                body_markdown: String::new(),
                reasoning_markdown: Some("pondering the request".to_string()),
                selected_source: None,
            });
        assert!(should_print_thinking(
            Some(RequestLifecycleState::Processing),
            &reasoning_only
        ));

        // Inference has not begun during claimed work; no reasoning yet.
        assert!(!should_print_thinking(
            Some(RequestLifecycleState::Claimed),
            &reasoning_only
        ));

        let with_content =
            gents::session::CanonicalRequestOutput::Live(gents::session::CanonicalPresentation {
                body_markdown: "answer body".to_string(),
                reasoning_markdown: Some("pondering the request".to_string()),
                selected_source: None,
            });
        assert!(!should_print_thinking(
            Some(RequestLifecycleState::Processing),
            &with_content
        ));

        let no_reasoning =
            gents::session::CanonicalRequestOutput::Live(gents::session::CanonicalPresentation {
                body_markdown: String::new(),
                reasoning_markdown: None,
                selected_source: None,
            });
        assert!(!should_print_thinking(
            Some(RequestLifecycleState::Processing),
            &no_reasoning
        ));
        // Loading/absence carry no presentation bytes at all.
        assert!(!should_print_thinking(
            Some(RequestLifecycleState::Processing),
            &gents::session::CanonicalRequestOutput::Loading
        ));
        assert!(!should_print_thinking(None, &reasoning_only));
    }

    #[test]
    fn progress_marker_ignores_retained_partial_diagnostics() {
        let absent = chat_progress_marker(
            None,
            Some(&gents::session::CanonicalRequestOutput::Absent),
            &[],
        );
        let retained = chat_progress_marker(
            None,
            Some(&gents::session::CanonicalRequestOutput::RetainedPartial(
                Vec::new(),
            )),
            &[],
        );

        // Retained partials are historical diagnostics, not streaming answer
        // progress, so they must not register new output bytes.
        assert_eq!(absent.canonical_output, retained.canonical_output);
        assert_eq!(retained.canonical_output, None);
    }
}
