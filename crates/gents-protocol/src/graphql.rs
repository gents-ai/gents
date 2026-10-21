use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client_protocol::{
    project_attempt, AttemptView, ClientHeadProjection, ClientTurnState, RequestSnapshot,
};
use crate::output::TranscriptMessage;
use crate::row::{AgentRequestRow, AgentToolCallRow};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphqlSubmittedRequest {
    pub request_id: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphqlTurnState {
    pub request: Option<AgentRequestRow>,
}

impl GraphqlTurnState {
    pub fn projected_head(&self) -> Option<ClientHeadProjection> {
        self.attempt_view().as_ref().map(project_attempt)
    }

    pub fn derived_turn_state(&self) -> Option<ClientTurnState> {
        self.projected_head().map(|head| head.turn_state)
    }

    pub fn successor_request_id(&self) -> Option<String> {
        self.request
            .as_ref()
            .and_then(|row| clean_optional_string(row.superseded_by_request.as_deref()))
    }

    fn attempt_view(&self) -> Option<AttemptView> {
        let request = self.request.as_ref()?;
        let lifecycle = request.lifecycle_state?;

        Some(AttemptView {
            request: RequestSnapshot {
                request_id: request.request_id.clone(),
                retry_parent_request: clean_optional_string(
                    request.retry_parent_request.as_deref(),
                ),
                lifecycle_state: lifecycle,
                is_superseded: clean_optional_string(request.superseded_by_request.as_deref())
                    .is_some(),
            },
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphqlSessionShape {
    pub session_id: String,
    pub request_id: String,
    pub turn_state: Option<String>,
    pub request: Option<AgentRequestRow>,
    /// Headers only. Payload readiness is resolved separately by reconstruction.
    pub messages: Vec<TranscriptMessage>,
    pub tool_calls: Vec<AgentToolCallRow>,
}

/// Validate `name` against the GraphQL `Name` grammar:
/// `[_A-Za-z][_0-9A-Za-z]*` (ASCII only). Anything interpolated into a
/// GraphQL document in identifier position MUST pass this check first —
/// escaping does not exist for identifiers.
pub fn validate_graphql_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        Some(c) => {
            return Err(anyhow!(
                "invalid identifier {name:?}: must start with a letter or underscore, got {c:?}"
            ));
        }
        None => return Err(anyhow!("invalid identifier: empty string")),
    }
    if let Some(c) = name
        .chars()
        .find(|c| !c.is_ascii_alphanumeric() && *c != '_')
    {
        return Err(anyhow!(
            "invalid identifier {name:?}: only ASCII letters, digits, and underscore are allowed, got {c:?}"
        ));
    }
    Ok(())
}

/// Validate a value used as a **collection name** in identifier position
/// (e.g. an event source's collection name). On top of the Name grammar this
/// rejects the `__` prefix, which the GraphQL spec reserves for
/// introspection — a "collection" of `__Type` or `__schema` would aim a
/// query at the introspection surface instead of a document collection.
pub fn validate_collection_identifier(name: &str) -> Result<()> {
    validate_graphql_name(name)?;
    if name.starts_with("__") {
        return Err(anyhow!(
            "invalid collection name {name:?}: the __ prefix is reserved for GraphQL introspection"
        ));
    }
    Ok(())
}

/// Validate a caller-supplied GraphQL **filter-object fragment** — a value
/// spliced into a query whole, as an event-source filter is.
///
/// This is the third interpolation position, and neither of the other two
/// defenses reaches it: escaping would destroy the object syntax, and the
/// value is not an identifier. What makes it checkable is that a break-out
/// must unbalance the fragment — to escape it has to close a `]`, `}` or
/// `)` it never opened. So: require one balanced object literal, built only
/// from the tokens a filter legitimately needs.
///
/// Deliberately strict. `(`, `)` and `#` never appear in a filter object,
/// but each is load-bearing for an attack (`)` closes the enclosing field's
/// argument list, `#` comments out the query's own tail), so they are
/// rejected outright rather than balance-tracked.
pub fn validate_graphql_filter_fragment(filter: &str) -> Result<()> {
    let trimmed = filter.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("invalid filter: empty"));
    }
    if !trimmed.starts_with('{') {
        return Err(anyhow!("invalid filter: must be an object literal"));
    }

    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut closed_at: Option<usize> = None;
    let mut after_string = false;

    for (index, ch) in trimmed.char_indices() {
        if in_string {
            match ch {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => {
                    in_string = false;
                    after_string = true;
                }
                _ => {}
            }
            continue;
        }
        // GraphQL object keys are names; a JSON-style quoted key parses as a
        // string here but fails when the runtime queries with the filter.
        if after_string && !ch.is_whitespace() {
            after_string = false;
            if ch == ':' {
                return Err(anyhow!(
                    "invalid filter: object keys are unquoted GraphQL names, e.g. {{kind: {{_eq: \"review\"}}}}"
                ));
            }
        }
        if closed_at.is_some() && !ch.is_whitespace() {
            return Err(anyhow!(
                "invalid filter: trailing content after the object literal"
            ));
        }
        match ch {
            '"' => in_string = true,
            '{' | '[' => stack.push(ch),
            '}' | ']' => {
                let opener = stack
                    .pop()
                    .ok_or_else(|| anyhow!("invalid filter: unbalanced {ch:?} at byte {index}"))?;
                let expected = if ch == '}' { '{' } else { '[' };
                if opener != expected {
                    return Err(anyhow!(
                        "invalid filter: mismatched {opener:?} closed by {ch:?} at byte {index}"
                    ));
                }
                if stack.is_empty() {
                    closed_at = Some(index);
                }
            }
            ':' | ',' => {}
            c if c.is_ascii_alphanumeric() || c == '_' => {}
            '-' | '+' | '.' => {}
            c if c.is_whitespace() => {}
            c => {
                return Err(anyhow!(
                    "invalid filter: character {c:?} at byte {index} is not allowed in a filter object"
                ));
            }
        }
    }

    if in_string {
        return Err(anyhow!("invalid filter: unterminated string literal"));
    }
    if !stack.is_empty() {
        return Err(anyhow!("invalid filter: unclosed {:?}", stack.last()));
    }
    Ok(())
}

pub fn escape_graphql_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch <= '\u{1f}' => {
                use std::fmt::Write;
                write!(escaped, "\\u{:04x}", ch as u32).expect("writing to a String");
            }
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[derive(Debug, Clone, Copy)]
pub struct GraphqlRequestOptions {
    pub timeout: Duration,
    pub max_attempts: usize,
    pub retry_backoff: Duration,
}

impl Default for GraphqlRequestOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_attempts: 5,
            retry_backoff: Duration::from_millis(100),
        }
    }
}

#[cfg(feature = "native")]
async fn graphql_http_client() -> Result<&'static reqwest::Client> {
    static CLIENT: tokio::sync::OnceCell<reqwest::Client> = tokio::sync::OnceCell::const_new();
    CLIENT
        .get_or_try_init(|| async {
            // Native certificate loading can block; initialize once off the executor.
            tokio::task::spawn_blocking(|| reqwest::Client::builder().build())
                .await?
                .map_err(anyhow::Error::from)
        })
        .await
}

#[cfg(feature = "native")]
pub async fn graphql_endpoint_available(graphql: &str, options: GraphqlRequestOptions) -> bool {
    let client = match graphql_http_client().await {
        Ok(client) => client,
        Err(_) => return false,
    };
    match client
        .post(graphql)
        .timeout(options.timeout)
        .json(&serde_json::json!({ "query": "{ __typename }" }))
        .send()
        .await
    {
        Ok(response) => response.status().is_success(),
        Err(_) => false,
    }
}

/// Execute a GraphQL read. Mutation documents are rejected before network I/O.
/// `authorization` is sent verbatim as the `Authorization` header.
#[cfg(feature = "native")]
pub async fn execute_graphql_async(
    graphql: &str,
    authorization: Option<&str>,
    query: &str,
    options: GraphqlRequestOptions,
) -> Result<serde_json::Value> {
    ensure_query_document(query)?;
    execute_graphql_async_with_tx(graphql, authorization, query, options, None).await
}

#[cfg(feature = "native")]
async fn execute_graphql_async_with_tx(
    graphql: &str,
    authorization: Option<&str>,
    query: &str,
    options: GraphqlRequestOptions,
    txn_id: Option<&str>,
) -> Result<serde_json::Value> {
    let client = graphql_http_client().await?;
    let mut last_error = None;

    for attempt in 0..options.max_attempts.max(1) {
        let mut request = client
            .post(graphql)
            .timeout(options.timeout)
            .json(&serde_json::json!({ "query": query }));
        if let Some(id) = txn_id {
            request = request.header("x-defradb-tx", id);
        }
        if let Some(authorization) = authorization {
            let mut value = reqwest::header::HeaderValue::from_str(authorization)
                .context("encoding Authorization header")?;
            value.set_sensitive(true);
            request = request.header(reqwest::header::AUTHORIZATION, value);
        }
        let response = request.send().await;
        let response = match response {
            Ok(response) => response,
            Err(error)
                if graphql_transport_error_is_retryable(&error)
                    && attempt + 1 < options.max_attempts =>
            {
                tracing::warn!(
                    attempt,
                    graphql,
                    error = %error,
                    cause = ?error,
                    is_timeout = error.is_timeout(),
                    is_connect = error.is_connect(),
                    timeout_ms = options.timeout.as_millis() as u64,
                    "retrying async GraphQL request after transport error"
                );
                last_error = Some(
                    anyhow::Error::new(error).context(format!("posting GraphQL to {graphql}")),
                );
                tokio::time::sleep(scale_backoff(options.retry_backoff, attempt)).await;
                continue;
            }
            Err(error) => {
                return Err(
                    anyhow::Error::new(error).context(format!("posting GraphQL to {graphql}"))
                );
            }
        };

        let response = match response.error_for_status() {
            Ok(response) => response,
            Err(error)
                if graphql_transport_error_is_retryable(&error)
                    && attempt + 1 < options.max_attempts =>
            {
                tracing::warn!(
                    attempt,
                    graphql,
                    error = %error,
                    "retrying async GraphQL request after response status error"
                );
                last_error = Some(
                    anyhow::Error::new(error)
                        .context(format!("reading GraphQL response from {graphql}")),
                );
                tokio::time::sleep(scale_backoff(options.retry_backoff, attempt)).await;
                continue;
            }
            Err(error) => {
                return Err(anyhow::Error::new(error)
                    .context(format!("reading GraphQL response from {graphql}")));
            }
        };

        let value = match response.json().await {
            Ok(value) => value,
            Err(error)
                if graphql_transport_error_is_retryable(&error)
                    && attempt + 1 < options.max_attempts =>
            {
                tracing::warn!(
                    attempt,
                    graphql,
                    error = %error,
                    "retrying async GraphQL request after decode error"
                );
                last_error = Some(
                    anyhow::Error::new(error)
                        .context(format!("decoding GraphQL response body from {graphql}")),
                );
                tokio::time::sleep(scale_backoff(options.retry_backoff, attempt)).await;
                continue;
            }
            Err(error) => {
                return Err(anyhow::Error::new(error)
                    .context(format!("decoding GraphQL response body from {graphql}")));
            }
        };

        // DefraDB's HTTP handler already retries auto-commit conflicts up to
        // its `max_txn_retries`; a conflict reaching here exhausted that loop.
        return finish_graphql_response(graphql, value);
    }

    Err(last_error.unwrap_or_else(|| anyhow!("GraphQL request retries exhausted for {graphql}")))
}

#[cfg(feature = "native")]
fn ensure_query_document(document: &str) -> Result<()> {
    let document = document.trim_start();
    if document.starts_with('{') || document.starts_with("query") {
        Ok(())
    } else {
        anyhow::bail!("GraphQL read transport requires a query document")
    }
}

pub fn extract_mutation_doc_id(
    response: &serde_json::Value,
    collection_name: &str,
) -> Result<String> {
    let data = response
        .get("data")
        .ok_or_else(|| anyhow!("graphql response missing data: {response}"))?;
    for field_name in [
        format!("upsert_{collection_name}"),
        format!("update_{collection_name}"),
        format!("create_{collection_name}"),
        format!("add_{collection_name}"),
    ] {
        if let Some(doc_id) = data
            .get(&field_name)
            .and_then(|value| value.get("_docID"))
            .and_then(serde_json::Value::as_str)
        {
            return Ok(doc_id.to_string());
        }
        if let Some(doc_id) = data
            .get(&field_name)
            .and_then(serde_json::Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("_docID"))
            .and_then(serde_json::Value::as_str)
        {
            return Ok(doc_id.to_string());
        }
    }
    anyhow::bail!("graphql mutation returned no _docID for {collection_name}: {response}");
}

pub fn first_graphql_row<'a>(
    response: &'a serde_json::Value,
    collection_name: &str,
) -> Result<&'a serde_json::Value> {
    response
        .get("data")
        .and_then(|data| data.get(collection_name))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .ok_or_else(|| anyhow!("graphql returned no rows for {collection_name}"))
}

pub fn graphql_rows_from_response(response: &Value, collection_name: &str) -> Vec<Value> {
    response
        .get("data")
        .and_then(|data| data.get(collection_name))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Returns true only when GraphQL reports that the collection's root field is
/// absent. A missing column on an existing collection must remain an error;
/// treating every `Cannot query field` mentioning the collection type as a
/// missing collection hides schema/query drift.
pub fn is_collection_missing_error_message(collection_name: &str, message: &str) -> bool {
    let missing_root_double_quoted = format!(r#"Cannot query field \"{collection_name}\""#);
    let missing_root_single_quoted = format!("Cannot query field '{collection_name}'");
    (message.contains(collection_name) && message.contains("collection not found"))
        || message.contains(&missing_root_double_quoted)
        || message.contains(&missing_root_single_quoted)
}

pub fn graphql_string_list_literal(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Converts a `serde_json::Value` into a GraphQL input literal for use in
/// DefraDB document mutations (create/update/upsert payloads).
///
/// # Empty list handling
///
/// An empty `Value::Array` is rendered as the literal `null`, never `[]`.
/// DefraDB types a bare `[]` as `JsonArray([])`. This is incompatible with
/// `NillableStringArray` (`[String]`) columns (used for tags, skill IDs,
/// agent target IDs, and similar canonical lists). A create may appear to
/// succeed while storing the wrong type; any
/// subsequent update then fails re-validation.
///
/// All array fields that currently flow through this function for DefraDB
/// collections are list-of-string columns. The blanket rule is therefore
/// safe for the documented use cases in the apply/desired-state machinery.
pub fn graphql_input_literal(value: &Value) -> Result<String> {
    match value {
        Value::Null => Ok("null".to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) => Ok(value.to_string()),
        Value::String(value) => Ok(graphql_string_literal(value)),
        Value::Array(values) => {
            if values.is_empty() {
                return Ok("null".to_string());
            }
            let rendered = values
                .iter()
                .map(graphql_input_literal)
                .collect::<Result<Vec<_>>>()?;
            Ok(format!("[{}]", rendered.join(", ")))
        }
        Value::Object(map) => {
            let rendered = map
                .iter()
                .map(|(key, value)| {
                    // Keys are in identifier position, where escaping cannot
                    // apply; an unvalidated key is an injection into the mutation.
                    validate_graphql_name(key)?;
                    Ok(format!("{key}: {}", graphql_input_literal(value)?))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(format!("{{ {} }}", rendered.join(", ")))
        }
    }
}

pub fn nullable_string_field(name: &str, value: Option<&str>) -> String {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => format!(r#"{name}: "{}""#, escape_graphql_string(value)),
        None => format!("{name}: null"),
    }
}

pub fn graphql_bool_literal(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

pub fn normalize_optional_rfc3339(value: Option<&str>) -> Result<Option<String>> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(raw) => {
            let parsed = chrono::DateTime::parse_from_rfc3339(raw)
                .with_context(|| format!("parsing RFC3339 timestamp {raw}"))?;
            Ok(Some(
                parsed
                    .with_timezone(&chrono::Utc)
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            ))
        }
        None => Ok(None),
    }
}

pub fn optional_i64_field(name: &str, value: Option<i64>) -> Option<String> {
    value.map(|value| format!("{name}: {value}"))
}

pub fn optional_f64_field(name: &str, value: Option<f64>) -> Option<String> {
    value.map(|value| format!("{name}: {value}"))
}

pub fn optional_bool_field(name: &str, value: Option<bool>) -> Option<String> {
    value.map(|value| format!("{name}: {}", graphql_bool_literal(value)))
}

pub fn optional_i64_list_field(name: &str, value: Option<&[i64]>) -> Option<String> {
    let values = value?;
    if values.is_empty() {
        Some(format!("{name}: null"))
    } else {
        Some(format!(
            "{name}: [{}]",
            values
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

pub fn optional_string_field(name: &str, value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!(r#"{name}: "{}""#, escape_graphql_string(value)))
}

pub fn string_list_field(name: &str, values: &[String]) -> Option<String> {
    // Empty lists serialize as `null`, NOT `[]`. A bare `[]` GraphQL literal is
    // typed by DefraDB as `JsonArray([])`, incompatible with a
    // `NillableStringArray` (`[String]`) column: the create "succeeds" but
    // stores a JsonArray, and every later update fails re-validation.
    if values.is_empty() {
        Some(format!("{name}: null"))
    } else {
        Some(format!("{name}: {}", graphql_string_list_literal(values)))
    }
}

pub fn turn_state_query(request_id: &str) -> String {
    let escaped_request_id = escape_graphql_string(request_id);
    format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}, limit: 1) {{
                request_id
                retry_parent_request
                superseded_by_request
                temperature
                top_p
                top_k
                max_tokens
                max_total_tokens
                metadata
                lifecycle_state
                failure_reason
                interrupt_requested_at
                valid_until
            }}
        }}"#
    )
}

pub fn session_shape_query(session_id: &str) -> String {
    let escaped_session_id = escape_graphql_string(session_id);
    format!(
        r#"{{
            AgentMessage(filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}, order: {{ sequence: ASC }}) {{
                message_key
                session_id
                node_did
                requester_did
                request_doc_id
                publication
                outcome
                sequence
                role
                native_id
                blocks
                created_at
            }}
            AgentToolCall(filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}, order: {{ message_sequence: ASC }}) {{
                tool_call_key
                session_id
                message_sequence
                tool_name
                tool_call_id
                status
                lifecycle_state
                child_request_id
                spawn_target_did
                spawn_agent_id
                await_mode
                deadline_at
                selected_service_id
                selected_tool_name
                tool_failure_class
                denial_reason
                denied_argv
                denied_command
                denied_argument
                denied_subcommand
                denied_prefix
                policy_mode
                policy_network
                cancel_cause
                latency_ms
            }}
        }}"#
    )
}

pub fn parse_turn_state_response(
    value: &serde_json::Value,
) -> serde_json::Result<GraphqlTurnState> {
    let data = value
        .get("data")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let request = data
        .get("AgentRequest")
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .map(serde_json::from_value)
        .transpose()?;
    Ok(GraphqlTurnState { request })
}

pub fn parse_session_shape_response(
    session_id: &str,
    request_id: &str,
    turn_state: GraphqlTurnState,
    value: &serde_json::Value,
) -> serde_json::Result<GraphqlSessionShape> {
    let data = value
        .get("data")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let messages = data
        .get("AgentMessage")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    let tool_calls = data
        .get("AgentToolCall")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();

    Ok(GraphqlSessionShape {
        session_id: session_id.to_string(),
        request_id: request_id.to_string(),
        turn_state: turn_state
            .derived_turn_state()
            .map(|state| format!("{state:?}")),
        request: turn_state.request,
        messages,
        tool_calls,
    })
}

#[cfg(feature = "native")]
fn finish_graphql_response(graphql: &str, value: serde_json::Value) -> Result<serde_json::Value> {
    let errors = value
        .get("errors")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !errors.is_empty() {
        anyhow::bail!(
            "graphql returned errors from {graphql}: {}",
            serde_json::Value::Array(errors)
        );
    }
    Ok(value)
}

/// Whether GraphQL/DefraDB error text describes a retryable store conflict.
pub fn retryable_graphql_error_text(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("transaction conflict") || message.contains("please retry")
}

#[cfg(feature = "native")]
fn graphql_transport_error_is_retryable(error: &reqwest::Error) -> bool {
    if error.is_timeout() || error.is_connect() || error.is_request() {
        return true;
    }

    let message = error.to_string();
    message.contains("connection closed before message completed")
        || message.contains("connection reset")
        || message.contains("broken pipe")
        || message.contains("channel closed")
        || message.contains("unexpected eof")
        || message.contains("end of file before message length reached")
        || message.contains("error decoding response body")
}

/// Whether an anyhow error is retryable under the GraphQL transport policy.
#[cfg(feature = "native")]
pub fn graphql_error_is_retryable(error: &anyhow::Error) -> bool {
    graphql_transport_error_in_chain(error)
        || error
            .chain()
            .any(|cause| retryable_graphql_error_text(&cause.to_string()))
}

/// Whether an anyhow error wraps a retryable GraphQL transport failure.
///
/// Auto-commit reads must use this rather than [`graphql_error_is_retryable`]:
/// DefraDB's HTTP handler already retried their store conflicts.
#[cfg(feature = "native")]
pub fn graphql_transport_error_in_chain(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<reqwest::Error>()
            .is_some_and(graphql_transport_error_is_retryable)
    })
}

#[cfg(feature = "native")]
fn scale_backoff(base: Duration, attempt: usize) -> Duration {
    let multiplier = attempt.saturating_add(1) as u32;
    base.saturating_mul(multiplier)
}

fn clean_optional_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn graphql_string_literal(value: &str) -> String {
    format!(r#""{}""#, escape_graphql_string(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_graphql_filter_fragment_accepts_real_filters() {
        for filter in [
            r#"{ kind: { _eq: "signup" } }"#,
            r#"{ status: { _in: ["a", "b"] } }"#,
            r#"{ _and: [ { a: { _eq: 1 } }, { b: { _gt: -2.5 } } ] }"#,
            r#"{ name: { _like: "%needs \"quotes\"%" } }"#,
            "{ enabled: { _eq: true } }",
        ] {
            assert!(
                validate_graphql_filter_fragment(filter).is_ok(),
                "{filter:?} is a legitimate filter and must pass"
            );
        }
    }

    #[test]
    fn validate_graphql_filter_fragment_rejects_break_outs() {
        for filter in [
            r#"{} ] }, limit: 1) { _docID } Agent(filter: { _and: [ {} ] }, limit: 1) { system_prompt } PocEvent(filter: { _and: [ {}"#,
            "{ a: 1 } }",
            "{ a: 1 ]",
            "{ a: 1 }) { x } (",
            "{ a: 1 } { b: 2 }",
            "{ a: 1 } # trailing",
            "kind",
            "",
            "   ",
            r#"{ a: { _eq: "open } }"#,
            r#"{"instructions": {"_eq": "b"}}"#,
        ] {
            assert!(
                validate_graphql_filter_fragment(filter).is_err(),
                "{filter:?} must be rejected"
            );
        }
    }

    #[test]
    fn graphql_input_literal_rejects_object_keys_that_are_not_graphql_names() {
        let hostile = serde_json::json!({
            "endpoint": {
                r#"x: 1 }, api_key: "leaked" }) { _docID } #"#: 1
            }
        });
        let err = graphql_input_literal(&hostile)
            .expect_err("a non-Name object key must be rejected, not spliced");
        assert!(
            err.to_string().contains("identifier"),
            "error should name the identifier rule: {err}"
        );

        let ok = serde_json::json!({ "outer": { "inner_1": "v" } });
        assert!(graphql_input_literal(&ok)
            .expect("valid Name keys render")
            .contains("inner_1: \"v\""));
    }

    #[test]
    fn graphql_input_literal_emits_null_for_empty_array_not_bracket_literal() {
        assert_eq!(
            graphql_input_literal(&serde_json::json!([])).expect("render literal"),
            "null",
        );
        let value = serde_json::json!({
            "context_id": "c",
            "skill_ids": [],
            "target_ids": [],
        });
        let rendered = graphql_input_literal(&value).expect("render literal");
        assert!(
            rendered.contains("target_ids: null"),
            "rendered: {rendered}"
        );
        assert!(rendered.contains("skill_ids: null"), "rendered: {rendered}");
        assert!(!rendered.contains("target_ids: []"), "rendered: {rendered}");
        assert!(!rendered.contains("skill_ids: []"), "rendered: {rendered}");
    }
}

#[cfg(all(test, feature = "native"))]
mod tx_tests {
    use super::*;
    use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    #[derive(Clone, Default)]
    struct HeaderRecorder {
        last_tx_header: Arc<Mutex<Option<String>>>,
    }

    #[derive(Clone)]
    struct RetryRecorder {
        attempts: Arc<Mutex<usize>>,
        first_error_message: Arc<String>,
    }

    impl RetryRecorder {
        fn new(first_error_message: impl Into<String>) -> Self {
            Self {
                attempts: Arc::new(Mutex::new(0)),
                first_error_message: Arc::new(first_error_message.into()),
            }
        }
    }

    async fn capture_handler(
        State(state): State<HeaderRecorder>,
        headers: HeaderMap,
        Json(_body): Json<serde_json::Value>,
    ) -> Json<serde_json::Value> {
        *state.last_tx_header.lock().unwrap() = headers
            .get("x-defradb-tx")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        Json(serde_json::json!({ "data": {} }))
    }

    async fn retry_once_handler(
        State(state): State<RetryRecorder>,
        Json(_body): Json<serde_json::Value>,
    ) -> Json<serde_json::Value> {
        let mut attempts = state.attempts.lock().unwrap();
        *attempts += 1;
        if *attempts == 1 {
            return Json(serde_json::json!({
                "errors": [{
                    "message": state.first_error_message.as_str()
                }]
            }));
        }
        Json(serde_json::json!({ "data": { "ok": true } }))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn execute_graphql_async_with_tx_sets_header_when_id_provided() {
        let state = HeaderRecorder::default();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/", post(capture_handler))
            .with_state(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let endpoint = format!("http://{addr}/");
        let options = GraphqlRequestOptions {
            timeout: std::time::Duration::from_secs(2),
            max_attempts: 1,
            retry_backoff: std::time::Duration::from_millis(50),
        };

        execute_graphql_async_with_tx(&endpoint, None, "{ __typename }", options, Some("42"))
            .await
            .unwrap();
        assert_eq!(state.last_tx_header.lock().unwrap().as_deref(), Some("42"));

        execute_graphql_async_with_tx(&endpoint, None, "{ __typename }", options, None)
            .await
            .unwrap();
        assert_eq!(state.last_tx_header.lock().unwrap().as_deref(), None);
    }

    #[tokio::test]
    async fn shared_graphql_client_preserves_per_request_timeouts() {
        let first = graphql_http_client().await.unwrap();
        let second = graphql_http_client().await.unwrap();
        assert!(std::ptr::eq(first, second));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let app = Router::new().route(
            "/",
            post(|| async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                Json(serde_json::json!({ "data": { "ok": true } }))
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut options = GraphqlRequestOptions {
            timeout: Duration::from_millis(10),
            max_attempts: 1,
            retry_backoff: Duration::ZERO,
        };
        assert!(execute_graphql_async(&endpoint, None, "{ ok }", options)
            .await
            .is_err());
        options.timeout = Duration::from_secs(2);
        assert_eq!(
            execute_graphql_async(&endpoint, None, "{ ok }", options)
                .await
                .unwrap()["data"]["ok"],
            true
        );
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn public_transport_rejects_mutations_before_network_io() {
        let error = execute_graphql_async(
            "http://127.0.0.1:1/api/v0/graphql",
            None,
            "mutation { create_X(input: {}) { _docID } }",
            GraphqlRequestOptions::default(),
        )
        .await
        .expect_err("public transport is query-only")
        .to_string();
        assert!(
            error.contains("GraphQL read transport requires a query document"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn execute_graphql_async_leaves_conflict_retry_to_the_server() {
        let state = RetryRecorder::new(
            "commit error: datastore error: storage error: transaction conflict. Please retry",
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/", post(retry_once_handler))
            .with_state(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let endpoint = format!("http://{addr}/");
        let options = GraphqlRequestOptions {
            timeout: std::time::Duration::from_secs(2),
            max_attempts: 3,
            retry_backoff: std::time::Duration::from_millis(1),
        };

        let error = execute_graphql_async(&endpoint, None, "{ __typename }", options)
            .await
            .expect_err("a conflict the server returned is final");

        assert!(
            error.to_string().contains("transaction conflict"),
            "{error}"
        );
        assert_eq!(*state.attempts.lock().unwrap(), 1);
    }

    #[test]
    fn retryable_graphql_error_text_matches_store_conflict_variants() {
        assert!(retryable_graphql_error_text("Transaction conflict"));
        assert!(retryable_graphql_error_text(
            "wrapped backend: PLEASE RETRY the TRANSACTION CONFLICT"
        ));
        assert!(retryable_graphql_error_text("please retry"));
        assert!(!retryable_graphql_error_text(
            "validation error: unknown collection"
        ));
    }

    #[test]
    fn retryable_graphql_error_walks_wrapped_cause_chain_case_insensitively() {
        let error = anyhow::anyhow!("TRANSACTION CONFLICT")
            .context("request submit failed")
            .context("outer shim context");
        assert!(graphql_error_is_retryable(&error));
        assert!(!graphql_transport_error_in_chain(&error));
    }
}
