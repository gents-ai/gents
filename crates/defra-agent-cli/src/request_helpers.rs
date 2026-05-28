use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_agent::graphql::escape_graphql_string;
use defra_agent_protocol::transcript::present_persisted_message;
use serde_json::Value;

use crate::{optional_f64_field, optional_i64_field, post_graphql, require_non_empty};

#[derive(Debug, Clone)]
pub(crate) struct SubmittedRequest {
    pub(crate) request_id: String,
    pub(crate) session_id: String,
    pub(crate) agent_did: String,
    pub(crate) behavior_id: Option<String>,
    pub(crate) temperature: Option<f64>,
    pub(crate) top_p: Option<f64>,
    pub(crate) top_k: Option<i64>,
    pub(crate) max_tokens: Option<i64>,
    pub(crate) metadata: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RequestSubmitOptions {
    pub(crate) temperature: Option<f64>,
    pub(crate) top_p: Option<f64>,
    pub(crate) top_k: Option<i64>,
    pub(crate) max_tokens: Option<i64>,
    pub(crate) metadata: Option<String>,
    pub(crate) valid_until: Option<DateTime<Utc>>,
    pub(crate) retry_parent_request: Option<String>,
    pub(crate) retry_root_request: Option<String>,
}

pub(crate) fn response_query(request_id: &str) -> String {
    format!(
        r#"{{
            AgentResponse(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                order: {{ created_at: DESC }},
                limit: 1
            ) {{
                request_id
                behavior_id
                session_id
                status
                content
                reasoning
                error_message
                token_count
                progress_seq
                materialized_message_sequence
                materialized_at
                completed_at
                interrupted_at
            }}
        }}"#,
        request_id = escape_graphql_string(request_id),
    )
}

pub(crate) fn materialized_message_query(session_id: &str, sequence: i64) -> String {
    format!(
        r#"{{
            AgentMessage(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    sequence: {{ _eq: {sequence} }}
                }},
                limit: 1
            ) {{
                role
                content
                sequence
            }}
        }}"#,
        session_id = escape_graphql_string(session_id),
    )
}

/// Query that yields the subset of `AgentRequest` fields the waiter uses to
/// decide when a request has reached a terminal lifecycle state (even if no
/// `AgentResponse` row ever materializes).
pub(crate) fn request_terminal_query(request_id: &str) -> String {
    format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                order: {{ created_at: DESC }},
                limit: 1
            ) {{
                request_id
                lifecycle_state
                failure_reason
                interrupt_requested_at
                valid_until
            }}
        }}"#,
        request_id = escape_graphql_string(request_id),
    )
}

/// Returns `true` when a request's lifecycle_state is one of the terminal
/// states that the waiter should stop polling on. `interrupted` counts even
/// though the response row may stay `status="streaming"` with partial content
/// and a stamped `interrupted_at`.
pub(crate) fn is_terminal_lifecycle_state(state: &str) -> bool {
    matches!(
        state,
        "completed" | "failed" | "superseded" | "dead" | "interrupted"
    )
}

fn response_field_is_blank(response: &Value, field: &str) -> bool {
    response
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
}

fn response_materialized_sequence(response: &Value) -> Option<i64> {
    response
        .get("materialized_message_sequence")
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        })
}

pub(crate) async fn hydrate_materialized_response_content(
    graphql: &str,
    response: &mut Value,
) -> Result<bool> {
    let content_blank = response_field_is_blank(response, "content");
    let reasoning_blank = response_field_is_blank(response, "reasoning");
    if !content_blank && !reasoning_blank {
        return Ok(true);
    }

    let Some(sequence) = response_materialized_sequence(response) else {
        return Ok(!content_blank || !reasoning_blank);
    };
    let Some(session_id) = response.get("session_id").and_then(Value::as_str) else {
        return Ok(!content_blank || !reasoning_blank);
    };

    let query = materialized_message_query(session_id, sequence);
    let message_response = post_graphql(graphql, &query).await?;
    let Some(message) = message_response
        .pointer("/data/AgentMessage")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
    else {
        return Ok(false);
    };
    let Some(role) = message.get("role").and_then(Value::as_str) else {
        return Ok(false);
    };
    let Some(content) = message.get("content").and_then(Value::as_str) else {
        return Ok(false);
    };

    let presentation = present_persisted_message(role, content);
    let Some(object) = response.as_object_mut() else {
        return Ok(false);
    };

    if content_blank && !presentation.body_markdown.trim().is_empty() {
        object.insert(
            "content".to_string(),
            Value::String(presentation.body_markdown),
        );
    }
    if reasoning_blank {
        if let Some(reasoning) = presentation
            .reasoning_markdown
            .filter(|value| !value.trim().is_empty())
        {
            object.insert("reasoning".to_string(), Value::String(reasoning));
        }
    }

    // A terminal response can legitimately materialize to no visible text,
    // for example after a final assistant message that only closes a tool
    // loop. The waiter only needs to know that the referenced message row
    // exists; visible content is optional.
    Ok(true)
}

pub(crate) async fn create_agent_request(
    graphql: &str,
    agent_did: &str,
    content: &str,
    session_id: Option<&str>,
    behavior_id: Option<&str>,
    options: RequestSubmitOptions,
) -> Result<SubmittedRequest> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = session_id
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let created_at = chrono::Utc::now().to_rfc3339();
    let behavior_field = behavior_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            format!(
                r#"
                behavior_id: "{}","#,
                escape_graphql_string(value)
            )
        })
        .unwrap_or_default();
    let valid_until_literal = options.valid_until.map(|at| {
        format!(
            r#"valid_until: "{}""#,
            escape_graphql_string(&at.to_rfc3339())
        )
    });
    let request_override_fields = vec![
        optional_f64_field("temperature", options.temperature),
        optional_f64_field("top_p", options.top_p),
        optional_i64_field("top_k", options.top_k),
        optional_i64_field("max_tokens", options.max_tokens),
        options
            .metadata
            .as_ref()
            .map(|metadata| format!(r#"metadata: "{}""#, escape_graphql_string(metadata))),
        valid_until_literal,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                ");
    let request_override_fields = if request_override_fields.is_empty() {
        String::new()
    } else {
        format!("{request_override_fields},\n                ")
    };
    let retry_parent_value = options.retry_parent_request.as_deref().unwrap_or_default();
    let retry_root_value = options
        .retry_root_request
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            if retry_parent_value.is_empty() {
                request_id.clone()
            } else {
                retry_parent_value.to_string()
            }
        });
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{agent_did}",
                {behavior_field}
                session_id: "{session_id}",
                retry_parent_request: "{retry_parent}",
                retry_root_request: "{retry_root}",
                superseded_by_request: "",
                content: "{content}",
                {request_override_fields}status: "pending",
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "interactive",
                failure_reason: "",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: 3
            }}) {{ _docID }}
        }}"#,
        request_id = escape_graphql_string(&request_id),
        agent_did = escape_graphql_string(agent_did),
        behavior_field = behavior_field,
        session_id = escape_graphql_string(&session_id),
        retry_parent = escape_graphql_string(retry_parent_value),
        retry_root = escape_graphql_string(&retry_root_value),
        content = escape_graphql_string(content),
        request_override_fields = request_override_fields,
    );
    post_graphql(graphql, &mutation).await?;

    Ok(SubmittedRequest {
        request_id,
        session_id,
        agent_did: agent_did.to_string(),
        behavior_id: behavior_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        temperature: options.temperature,
        top_p: options.top_p,
        top_k: options.top_k,
        max_tokens: options.max_tokens,
        metadata: options.metadata,
    })
}

/// Poll both `AgentRequest.lifecycle_state` and the latest `AgentResponse`
/// row until either:
///   - the request reaches a terminal lifecycle state (`completed`, `failed`,
///     `superseded`, `dead`, `interrupted`), or
///   - the response reaches a historical terminal status (`complete`,
///     `error`).
///
/// Returning is intentionally lenient on partial data:
///   - `interrupted` requests stamp `interrupted_at` before terminalizing the
///     response as `error`, so callers can observe a durable interrupt marker
///     even if the request lifecycle reaches `interrupted` first.
///   - historical/background writers have used both `complete` and
///     `completed`; both spellings are treated as terminal success.
///   - `dead`/`Stale` requests (TTL'd before ever claiming) may have no
///     `AgentResponse` row at all; in that case we synthesize one and rely on
///     the top-level `request` field for the terminal info.
///
/// The returned JSON is backward-compatible with the old response-only shape:
/// all previous `AgentResponse` fields remain at the top level, and a new
/// `request` field carries the lifecycle view for callers that want it. For
/// completed live-tail responses, `content`/`reasoning` are hydrated from the
/// materialized `AgentMessage` in the returned JSON only; the persisted
/// `AgentResponse` row remains the live-tail surface.
pub(crate) async fn wait_for_terminal_response(
    graphql: &str,
    request_id: &str,
    timeout_secs: u64,
    poll_secs: u64,
) -> Result<serde_json::Value> {
    let idle_timeout = Duration::from_secs(timeout_secs);
    let mut last_progress_at = tokio::time::Instant::now();
    let mut last_progress_signature: Option<String> = None;

    loop {
        // Fetch request + response in sequence (cheap on the embedded node;
        // also keeps the "no GraphQL batching" contract simple for the
        // HTTP path).
        let request_row = {
            let query = request_terminal_query(request_id);
            let response = post_graphql(graphql, &query).await?;
            response
                .pointer("/data/AgentRequest")
                .and_then(|v| v.as_array())
                .and_then(|rows| rows.first())
                .cloned()
        };
        let response_row = {
            let query = response_query(request_id);
            let response = post_graphql(graphql, &query).await?;
            response
                .pointer("/data/AgentResponse")
                .and_then(|v| v.as_array())
                .and_then(|rows| rows.first())
                .cloned()
        };

        // Track "something observable changed" for the idle-timeout budget.
        // Combine both rows so mutations on just the request (e.g. interrupt
        // latch, transition to dead) count as progress.
        let signature = serde_json::to_string(&serde_json::json!({
            "request": request_row,
            "response": response_row,
        }))
        .context("serializing AgentRequest + AgentResponse progress rows for timeout tracking")?;
        if last_progress_signature.as_deref() != Some(signature.as_str()) {
            last_progress_signature = Some(signature);
            last_progress_at = tokio::time::Instant::now();
        }

        let lifecycle_state = request_row
            .as_ref()
            .and_then(|row| row.get("lifecycle_state"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let response_status = response_row
            .as_ref()
            .and_then(|row| row.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let should_wait_for_materialized_content =
            matches!(response_status, "complete" | "completed")
                && response_row.as_ref().is_some_and(|row| {
                    response_field_is_blank(row, "content")
                        && response_materialized_sequence(row).is_some()
                });

        let terminal_by_request = is_terminal_lifecycle_state(lifecycle_state);
        let terminal_by_response = matches!(response_status, "complete" | "completed" | "error");
        if terminal_by_request || terminal_by_response {
            // Build the return value: prefer the real response row when present
            // (interrupted / streaming-with-partial-content / complete / error).
            // If no row ever materialized (dead/Stale pre-claim), synthesize a
            // minimal one so older consumers that read top-level fields don't
            // explode.
            let mut envelope = response_row.unwrap_or_else(|| {
                serde_json::json!({
                    "request_id": request_id,
                    "status": null,
                    "content": null,
                })
            });
            let hydrated = hydrate_materialized_response_content(graphql, &mut envelope).await?;
            if should_wait_for_materialized_content && !hydrated {
                if last_progress_at.elapsed() >= idle_timeout {
                    anyhow::bail!(
                        "timed out waiting for materialized AgentMessage {request_id} after {timeout_secs}s of inactivity\n{}",
                        request_diagnostic_hint(request_id)
                    );
                }
                tokio::time::sleep(Duration::from_secs(poll_secs)).await;
                continue;
            }
            if let Some(object) = envelope.as_object_mut() {
                object.insert(
                    "request".to_string(),
                    request_row.unwrap_or(serde_json::Value::Null),
                );
            }
            return Ok(envelope);
        }

        if last_progress_at.elapsed() >= idle_timeout {
            anyhow::bail!(
                "timed out waiting for AgentResponse {request_id} after {timeout_secs}s of inactivity\n{}",
                request_diagnostic_hint(request_id)
            );
        }

        tokio::time::sleep(Duration::from_secs(poll_secs)).await;
    }
}

pub(crate) fn request_diagnostic_hint(request_id: &str) -> String {
    format!(
        "Next:\n  1. Run `defra-agent show request {request_id}`\n  2. Run `defra-agent show response {request_id}`\n  3. Inspect the runtime with `defra-agent status`"
    )
}

pub(crate) fn resolve_request_id(positional: Option<&str>, flag: Option<&str>) -> Result<String> {
    let positional = positional.map(str::trim).filter(|value| !value.is_empty());
    let flag = flag.map(str::trim).filter(|value| !value.is_empty());
    match (positional, flag) {
        (Some(positional), Some(flag)) if positional != flag => {
            anyhow::bail!(
                "conflicting request ids provided: positional={} and --request-id={}\nNext:\n  1. Pass the request id once: `defra-agent show response REQUEST_ID`\n  2. Or use `--request-id REQUEST_ID`, but not both",
                positional,
                flag
            );
        }
        (Some(request_id), _) | (_, Some(request_id)) => Ok(request_id.to_string()),
        (None, None) => anyhow::bail!(
            "missing request id\nNext:\n  1. Pass it positionally: `defra-agent show response REQUEST_ID`\n  2. Or use `--request-id REQUEST_ID`"
        ),
    }
}

pub(crate) fn resolve_request_content(
    content: Option<&str>,
    content_file: Option<&Path>,
) -> Result<String> {
    match (content, content_file) {
        (Some(_), Some(path)) => anyhow::bail!(
            "provide either --content or --content-file, not both ({})",
            path.display()
        ),
        (Some(content), None) => Ok(require_non_empty("content", content)?.to_string()),
        (None, Some(path)) => {
            let content = fs::read_to_string(path)
                .with_context(|| format!("reading request content from {}", path.display()))?;
            Ok(require_non_empty("content-file", &content)?.to_string())
        }
        (None, None) => {
            anyhow::bail!("request content is required; pass --content or --content-file")
        }
    }
}

pub(crate) fn write_json_output_file(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating output directory {}", parent.display()))?;
    }
    let contents =
        serde_json::to_vec_pretty(value).context("encoding JSON output for output file")?;
    fs::write(path, contents)
        .with_context(|| format!("writing JSON output file {}", path.display()))?;
    Ok(())
}

pub(crate) fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// Parse a short human duration (e.g. `30s`, `5m`, `2h`, `1d`). Bare numbers
/// are treated as seconds for convenience.
pub(crate) fn parse_duration_suffix(raw: &str) -> Result<Duration> {
    let s = raw.trim();
    if s.is_empty() {
        anyhow::bail!("duration must not be empty");
    }
    let split = s.find(|c: char| c.is_alphabetic()).unwrap_or(s.len());
    let (num_part, suffix) = s.split_at(split);
    let n: u64 = num_part
        .parse()
        .with_context(|| format!("invalid duration number in {raw}"))?;
    let secs = match suffix {
        "" | "s" => n,
        "m" => n.checked_mul(60).context("duration overflow")?,
        "h" => n.checked_mul(3600).context("duration overflow")?,
        "d" => n.checked_mul(86400).context("duration overflow")?,
        other => anyhow::bail!("unknown duration suffix {other:?} (use s, m, h, d)"),
    };
    Ok(Duration::from_secs(secs))
}

/// Resolve the `--valid-until` flag to an absolute deadline. `None` on the
/// CLI defaults to `now + 5m` — the standard TTL for interactive requests.
/// Pass `"none"` or `"0"` to explicitly disable the TTL for this submission.
pub(crate) fn parse_valid_until_flag(raw: Option<&str>) -> Result<Option<DateTime<Utc>>> {
    match raw.map(str::trim) {
        None => Ok(Some(Utc::now() + chrono::Duration::minutes(5))),
        Some("") | Some("none") | Some("0") => Ok(None),
        Some(value) => {
            let dur = parse_duration_suffix(value)?;
            let secs = i64::try_from(dur.as_secs()).context("duration too large")?;
            Ok(Some(Utc::now() + chrono::Duration::seconds(secs)))
        }
    }
}

/// Minimal projection of an AgentRequest used by `resend` to copy over
/// submission inputs. Queried via the HTTP GraphQL endpoint. Carries the
/// sampling overrides and `metadata` so resend preserves submitter intent —
/// dropping them would silently change model behavior on retry.
#[derive(Debug, Clone)]
pub(crate) struct StaleRequestView {
    pub(crate) agent_did: String,
    pub(crate) behavior_id: Option<String>,
    pub(crate) content: String,
    pub(crate) lifecycle_state: String,
    pub(crate) failure_reason: String,
    pub(crate) retry_root_request: Option<String>,
    pub(crate) temperature: Option<f64>,
    pub(crate) top_p: Option<f64>,
    pub(crate) top_k: Option<i64>,
    pub(crate) max_tokens: Option<i64>,
    pub(crate) metadata: Option<String>,
}

pub(crate) async fn fetch_request_view(
    graphql: &str,
    request_id: &str,
) -> Result<StaleRequestView> {
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                limit: 1
            ) {{
                agent_did
                behavior_id
                content
                lifecycle_state
                failure_reason
                retry_root_request
                temperature
                top_p
                top_k
                max_tokens
                metadata
            }}
        }}"#,
        request_id = escape_graphql_string(request_id),
    );
    let response = post_graphql(graphql, &query).await?;
    let row = response
        .pointer("/data/AgentRequest")
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("request {request_id} not found"))?;
    let as_string = |key: &str| {
        row.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let as_optional = |key: &str| {
        row.get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    let as_optional_f64 = |key: &str| row.get(key).and_then(|v| v.as_f64());
    let as_optional_i64 = |key: &str| row.get(key).and_then(|v| v.as_i64());
    Ok(StaleRequestView {
        agent_did: as_string("agent_did"),
        behavior_id: as_optional("behavior_id"),
        content: as_string("content"),
        lifecycle_state: as_string("lifecycle_state"),
        failure_reason: as_string("failure_reason"),
        retry_root_request: as_optional("retry_root_request"),
        temperature: as_optional_f64("temperature"),
        top_p: as_optional_f64("top_p"),
        top_k: as_optional_i64("top_k"),
        max_tokens: as_optional_i64("max_tokens"),
        metadata: as_optional("metadata"),
    })
}
