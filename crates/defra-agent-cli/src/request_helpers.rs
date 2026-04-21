use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_agent::graphql::escape_graphql_string;
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
                completed_at
            }}
        }}"#,
        request_id = escape_graphql_string(request_id),
    )
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
    let valid_until_literal = options
        .valid_until
        .map(|at| format!(r#"valid_until: "{}""#, escape_graphql_string(&at.to_rfc3339())));
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
    let retry_parent_value = options
        .retry_parent_request
        .as_deref()
        .unwrap_or_default();
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
        let query = response_query(request_id);
        let response = post_graphql(graphql, &query).await?;
        let rows = response
            .pointer("/data/AgentResponse")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        if let Some(row) = rows.first() {
            let signature = serde_json::to_string(row)
                .context("serializing AgentResponse progress row for timeout tracking")?;
            if last_progress_signature.as_deref() != Some(signature.as_str()) {
                last_progress_signature = Some(signature);
                last_progress_at = tokio::time::Instant::now();
            }

            let status = row
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if matches!(status, "complete" | "error") {
                return Ok(row.clone());
            }
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
fn parse_duration_suffix(raw: &str) -> Result<Duration> {
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
pub(crate) fn parse_valid_until_flag(
    raw: Option<&str>,
) -> Result<Option<DateTime<Utc>>> {
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
/// submission inputs. Queried via the HTTP GraphQL endpoint.
#[derive(Debug, Clone)]
pub(crate) struct StaleRequestView {
    pub(crate) session_id: String,
    pub(crate) agent_did: String,
    pub(crate) behavior_id: Option<String>,
    pub(crate) content: String,
    pub(crate) lifecycle_state: String,
    pub(crate) failure_reason: String,
    pub(crate) retry_root_request: Option<String>,
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
                session_id
                agent_did
                behavior_id
                content
                lifecycle_state
                failure_reason
                retry_root_request
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
    Ok(StaleRequestView {
        session_id: as_string("session_id"),
        agent_did: as_string("agent_did"),
        behavior_id: as_optional("behavior_id"),
        content: as_string("content"),
        lifecycle_state: as_string("lifecycle_state"),
        failure_reason: as_string("failure_reason"),
        retry_root_request: as_optional("retry_root_request"),
    })
}
