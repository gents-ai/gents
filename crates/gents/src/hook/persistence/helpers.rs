use super::*;
use anyhow::Context;

#[derive(Debug, thiserror::Error)]
#[error("background tool budget exceeded ({0} admitted)")]
pub(crate) struct BackgroundCapacityExceeded(pub(crate) usize);

#[derive(Debug, Deserialize)]
struct BackgroundedRow {
    lifecycle_state: String,
    spawned_by_tool_call_doc_id: Option<String>,
}

/// A pending spawned child is a durable admission reservation created in the
/// same transaction as its slot check; pending provider calls are not admitted.
fn decode_live_backgrounded_rows(value: serde_json::Value) -> anyhow::Result<usize> {
    let rows: Vec<BackgroundedRow> =
        serde_json::from_value(value).context("decode background row count")?;
    Ok(rows
        .into_iter()
        .filter(|row| row.lifecycle_state == "running" || row.spawned_by_tool_call_doc_id.is_some())
        .count())
}

/// The caller request's backgrounded tool rows that have not ended, read
/// through `reader` so an adoption counts them inside its transaction.
pub(crate) async fn count_live_backgrounded_rows_in<R: crate::config_client::ConfigRead>(
    reader: &R,
    request_doc_id: &str,
) -> anyhow::Result<usize> {
    let escaped_request_doc_id = crate::graphql::escape_graphql_string(request_doc_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    request_doc_id: {{ _eq: "{escaped_request_doc_id}" }},
                    await_mode: {{ _eq: "background" }},
                    lifecycle_state: {{ _in: ["running", "pending"] }}
                }}
            ) {{
                lifecycle_state spawned_by_tool_call_doc_id
            }}
        }}"#
    );
    let response = reader.execute_read(&query).await.map_err(|error| {
        error.context(format!(
            "query live backgrounded tool count for request document {request_doc_id} failed"
        ))
    })?;
    let value = response
        .pointer("/data/AgentToolCall")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("background row count omitted AgentToolCall rows"))?;
    decode_live_backgrounded_rows(value)
}

/// The live count a background admission checks against the ceiling.
/// DefraDB transactions do not conflict on predicate reads, so the admission
/// also bumps the request's `background_admission_sequence`: concurrent
/// admissions to one request then conflict at commit and their idempotent
/// transaction replays this count.
pub(crate) async fn reserve_background_slot_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    request_doc_id: &str,
) -> anyhow::Result<usize> {
    let live = count_live_backgrounded_rows_in(txn, request_doc_id).await?;
    let escaped = crate::graphql::escape_graphql_string(request_doc_id);
    let request = txn
        .execute(&format!(
            r#"{{ AgentRequest(docID: "{escaped}") {{ background_admission_sequence }} }}"#
        ))
        .await?;
    let rows = request
        .pointer("/data/AgentRequest")
        .and_then(serde_json::Value::as_array)
        .context("background admission request lookup omitted rows")?;
    anyhow::ensure!(
        rows.len() == 1,
        "background admission request {request_doc_id} is missing"
    );
    let next = rows[0]["background_admission_sequence"]
        .as_u64()
        .unwrap_or(0)
        + 1;
    txn.execute(&format!(
        r#"mutation {{ update_AgentRequest(docID: "{escaped}", input: {{ background_admission_sequence: {next} }}) {{ _docID }} }}"#
    ))
    .await?;
    Ok(live)
}

#[cfg(test)]
mod background_count_tests {
    use super::decode_live_backgrounded_rows;
    use serde_json::json;

    #[test]
    fn pending_spawn_reservations_count_but_pending_accepted_calls_do_not() {
        let rows = json!([
            {"lifecycle_state":"running","spawned_by_tool_call_doc_id":null},
            {"lifecycle_state":"pending","spawned_by_tool_call_doc_id":"parent"},
            {"lifecycle_state":"pending","spawned_by_tool_call_doc_id":null}
        ]);
        assert_eq!(decode_live_backgrounded_rows(rows).unwrap(), 2);
    }
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
    // The omitted bytes remain durably stored and retrievable. Only the
    // process-control caller sets "await_mode"; in that wait/read-process
    // context the exact next call is read_process keyed by the canonical
    // tool_call_id. Without that context or a non-empty string identity, keep
    // the generic omission wording rather than inventing a retrieval path or
    // inferring an identity.
    let canonical_tool_call_id = envelope
        .get("tool_call_id")
        .and_then(|value| value.as_str())
        .filter(|id| !id.is_empty());
    let stub = if envelope.get("await_mode").and_then(|value| value.as_str()) == Some("background")
    {
        match canonical_tool_call_id {
            Some(tool_call_id) => format!(
                "[{} bytes omitted from envelope; retrieve the full output with read_process using tool_call_id \"{}\"]",
                result.len(),
                tool_call_id
            ),
            None => format!(
                "[{} bytes omitted from envelope; the full output is retained]",
                result.len()
            ),
        }
    } else {
        format!(
            "[{} bytes omitted from envelope; the full output is retained]",
            result.len()
        )
    };
    envelope[result_key] = serde_json::Value::String(stub);
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

pub(super) fn background_budget_exceeded_payload(
    tool_name: &str,
    current_backgrounded: usize,
) -> String {
    json_string(json!({
        "ok": false,
        "failure_class": "service_unavailable",
        "code": "background_tool_budget_exceeded",
        "path": "/",
        "message": format!(
            "parent request has reached the concurrent backgrounded tool ceiling ({MAX_BACKGROUNDED_TOOLS_PER_PARENT}); wait for a completion notification, then retry"
        ),
        "retryable": true,
        "service_id": "process",
        "tool_name": tool_name,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_results_lead_with_the_answer_and_keep_nulls() {
        let value = json!({"ok":true,"status":"idle","result":null,"session_id":"session"});
        let rendered = json_string(value.clone());
        assert!(rendered.starts_with("{\n  \"result\": null,"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&rendered).unwrap(),
            value
        );
    }

    #[test]
    fn successful_process_stop_leads_with_the_observed_outcome() {
        let value = json!({"ok":true,"status":"cancelled","process":"stopped","error":null,"tool_call_id":"call"});
        let rendered = json_string(value.clone());
        assert!(
            rendered.starts_with("{\n  \"status\": \"cancelled\",\n  \"process\": \"stopped\",")
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&rendered).unwrap(),
            value
        );
    }

    #[test]
    fn json_envelope_bounds_oversized_result_and_stays_valid_json() {
        let limits = crate::truncation::TruncationLimits::default();
        let big = "x".repeat(2 * limits.max_bytes);
        let rendered = json_envelope_with_bounded_result(
            serde_json::json!({"ok": true, "status": "completed", "result": null}),
            "result",
            &big,
            "wait_process",
            &limits,
        );
        assert!(
            rendered.len() <= limits.max_bytes,
            "envelope must fit the outer bound: {} > {}",
            rendered.len(),
            limits.max_bytes
        );
        assert!(rendered.starts_with("{\n  \"result\":"));
        let parsed: serde_json::Value =
            serde_json::from_str(&rendered).expect("envelope must remain valid JSON");
        let result = parsed["result"].as_str().expect("result string");
        assert!(!result.is_empty(), "bounded result must be non-empty");
        assert!(result.len() < big.len(), "result must be bounded");
    }

    #[test]
    fn json_envelope_falls_back_to_stub_when_escaping_inflates_past_limits() {
        let limits = crate::truncation::TruncationLimits::default();
        let pathological = vec!["\u{1}".repeat(10); 1500].join("\n");
        let rendered = json_envelope_with_bounded_result(
            serde_json::json!({"ok": true, "result": null}),
            "result",
            &pathological,
            "wait_process",
            &limits,
        );
        assert!(rendered.len() <= limits.max_bytes);
        let parsed: serde_json::Value =
            serde_json::from_str(&rendered).expect("stub envelope must be valid JSON");
        assert!(
            parsed["result"]
                .as_str()
                .is_some_and(|s| s.contains("omitted")),
            "expected the stub fallback, got {parsed}"
        );
    }

    #[test]
    fn json_envelope_stub_names_read_process_only_in_process_context() {
        let limits = crate::truncation::TruncationLimits::default();
        // Control characters escape to 6 wire bytes each, so the raw payload
        // stays under the inner truncation bound while the rendered envelope
        // exceeds the outer bound: the real omission fallback runs.
        let oversized = "\u{1}".repeat(10_000);
        let expected = |hint: &str| {
            format!(
                "[{} bytes omitted from envelope; {}]",
                oversized.len(),
                hint
            )
        };
        let with_context = json_envelope_with_bounded_result(
            serde_json::json!({
                "ok": true, "status": "completed", "result": null,
                "tool_call_id": "tool-row-2737", "await_mode": "background"
            }),
            "result",
            &oversized,
            "wait_process",
            &limits,
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&with_context).expect("envelope must remain valid JSON");
        assert_eq!(
            parsed["result"].as_str().expect("result string"),
            expected(
                "retrieve the full output with read_process using tool_call_id \"tool-row-2737\""
            )
        );
        let without_context = json_envelope_with_bounded_result(
            serde_json::json!({
                "ok": true, "status": "completed", "result": null,
                "tool_call_id": "tool-row-2737"
            }),
            "result",
            &oversized,
            "wait_process",
            &limits,
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&without_context).expect("envelope must remain valid JSON");
        assert_eq!(
            parsed["result"].as_str().expect("result string"),
            expected("the full output is retained")
        );
    }

    #[test]
    fn prepared_presentation_handles_normalized_crlf_head_and_tail() {
        let limits = crate::truncation::TruncationLimits {
            max_lines: 2,
            max_bytes: 80,
        };
        let raw = "one\r\ntwo\r\nthree\r\nfour";
        for tool in [
            "wait_process",
            "bash",
            "agent_session_list",
            "agent_session_read",
        ] {
            let (bounded, presentation) = bounded_tool_result_with_presentation(tool, raw, &limits);
            let presentation = presentation.expect("CRLF input should truncate");
            assert_eq!(
                crate::tool_call_lifecycle::delivery::render_presentation(raw, &presentation)
                    .unwrap(),
                bounded
            );
        }
    }

    #[test]
    fn prepared_presentation_keeps_the_oversized_line_as_an_exact_range() {
        use gents_protocol::output::{PayloadPresentation, PresentationPart};

        let limits = crate::truncation::TruncationLimits::default();
        let payload = format!("{{\"items\":[{}]}}", vec!["\"v\""; 20_000].join(","));
        for (tool, raw) in [
            ("wait_process", format!("Results:\n{payload}")),
            ("bash", format!("{payload}\nexit status 1")),
        ] {
            let (bounded, presentation) =
                bounded_tool_result_with_presentation(tool, &raw, &limits);
            let Some(PayloadPresentation::Composed { parts }) = presentation else {
                panic!("{tool}: oversized output must be truncated");
            };
            let range = parts
                .iter()
                .find_map(|part| match part {
                    PresentationPart::OutputRange {
                        start_byte,
                        end_byte,
                    } => Some(end_byte - start_byte),
                    PresentationPart::Literal { .. } => None,
                })
                .unwrap_or_else(|| panic!("{tool}: selection must reference the source"));
            assert_eq!(range, limits.max_bytes as u64, "{tool}");
            assert_eq!(
                crate::tool_call_lifecycle::delivery::render_presentation(
                    &raw,
                    &PayloadPresentation::Composed { parts }
                )
                .unwrap(),
                bounded,
                "{tool}"
            );
        }
    }

    #[test]
    fn prepared_presentation_handles_empty_selected_output() {
        let limits = crate::truncation::TruncationLimits {
            max_lines: 0,
            max_bytes: 0,
        };
        let raw = "one\ntwo";
        let (bounded, presentation) =
            bounded_tool_result_with_presentation("wait_process", raw, &limits);
        let presentation = presentation.expect("zero budget should truncate");
        assert_eq!(
            crate::tool_call_lifecycle::delivery::render_presentation(raw, &presentation).unwrap(),
            bounded
        );
    }
}
