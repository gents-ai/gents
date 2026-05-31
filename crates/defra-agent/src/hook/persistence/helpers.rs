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

pub(super) fn background_receipt_payload(
    child_request_id: &str,
    child_session_id: Option<&str>,
    behavior_id: &str,
) -> String {
    json_string(json!({
        "ok": true,
        "child_request_id": child_request_id,
        "child_session_id": child_session_id,
        "behavior_id": behavior_id,
        "await_mode": "background",
        "status": "running"
    }))
}

pub(super) fn backgrounded_receipt_payload(
    child_request_id: &str,
    child_session_id: &str,
    behavior_id: &str,
) -> String {
    json_string(json!({
        "ok": true,
        "child_request_id": child_request_id,
        "child_session_id": child_session_id,
        "behavior_id": behavior_id,
        "await_mode": "background",
        "status": "running",
        "backgrounded": true
    }))
}

pub(super) async fn wait_for_external_lifecycle_owner(
    missing_owner_since: &mut Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
    internal_call_id: &str,
) -> anyhow::Result<()> {
    let first_missing_at = *missing_owner_since.get_or_insert(now);
    if now - first_missing_at >= chrono::Duration::seconds(5) {
        anyhow::bail!(
            "spawn_subagent foreground wait lost lifecycle ownership for tool_call_id={internal_call_id}"
        );
    }

    tokio::time::sleep(Duration::from_millis(50)).await;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct RunningSubagentBridgeRow {
    tool_call_id: String,
    child_request_id: Option<String>,
}

pub(super) async fn running_subagent_bridge_ids(
    node: &defra_node::EmbeddedNode,
    session_id: &str,
) -> anyhow::Result<Vec<String>> {
    let escaped_session_id = crate::graphql::escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    lifecycle_state: {{ _eq: "running" }},
                    cancel_policy: {{ _eq: "cascade" }}
                }},
                order: [{{ started_at: ASC }}, {{ tool_call_id: ASC }}]
            ) {{
                tool_call_id
                child_request_id
            }}
        }}"#
    );

    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query running subagent bridges for session {session_id} failed: {:?}",
            response.errors
        );
    }

    let rows: Vec<RunningSubagentBridgeRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();

    Ok(rows
        .into_iter()
        .filter(|row| {
            row.child_request_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
        })
        .map(|row| row.tool_call_id)
        .collect())
}

pub(super) fn truncation_mode_for(tool_name: &str) -> TruncationMode {
    match tool_name {
        "bash" | "shell" | "command" => TruncationMode::Tail,
        _ => TruncationMode::Head,
    }
}

pub(super) fn render_tool_result_text(tool_result: &ToolResult) -> String {
    tool_result
        .content
        .iter()
        .filter_map(|content| match content {
            ToolResultContent::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn tool_result_message_key(
    session_id: &str,
    message: &Message,
) -> anyhow::Result<Option<String>> {
    let Message::User { content } = message else {
        return Ok(None);
    };
    if content.len() != 1 {
        return Ok(None);
    }
    let UserContent::ToolResult(tool_result) = content.first_ref() else {
        return Ok(None);
    };

    let Some(logical_id) = non_empty(Some(tool_result.id.as_str()))
        .or_else(|| non_empty(tool_result.call_id.as_deref()))
    else {
        return Ok(None);
    };
    let content_json = serde_json::to_string(&tool_result.content)?;
    Ok(Some(format!(
        "{session_id}:tool-result:{:016x}:{:016x}",
        stable_hash(logical_id.as_bytes()),
        stable_hash(content_json.as_bytes())
    )))
}

pub(super) fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

pub(super) fn is_subagent_tool_result_payload(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return false;
    };
    value
        .get("service_id")
        .and_then(|value| value.as_str())
        .is_some_and(|service_id| service_id == "subagent")
        || (value.get("child_request_id").is_some() && value.get("await_mode").is_some())
}

pub(super) fn json_string(value: serde_json::Value) -> String {
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
}

pub(super) fn child_terminal_status(terminal: &ChildTerminal) -> &'static str {
    match terminal {
        ChildTerminal::Failed { .. } => "failed",
        ChildTerminal::Dead => "dead",
        ChildTerminal::Interrupted => "interrupted",
        ChildTerminal::Superseded => "superseded",
    }
}

pub(super) fn child_terminal_error(terminal: &ChildTerminal) -> (String, FailureClass) {
    match terminal {
        ChildTerminal::Failed {
            reason,
            failure_class,
        } => (reason.clone(), *failure_class),
        ChildTerminal::Dead => (
            "child request reached terminal state dead".to_string(),
            FailureClass::External,
        ),
        ChildTerminal::Interrupted => (
            "child request was interrupted".to_string(),
            FailureClass::External,
        ),
        ChildTerminal::Superseded => (
            "child request was superseded".to_string(),
            FailureClass::External,
        ),
    }
}

pub(super) fn foreground_terminal_failure_payload(
    child_request_id: &str,
    child_session_id: &str,
    status: &str,
    reason: impl Into<String>,
    failure_class: FailureClass,
) -> String {
    json_string(json!({
        "ok": false,
        "child_request_id": child_request_id,
        "child_session_id": child_session_id,
        "await_mode": "foreground",
        "status": status,
        "final_response": null,
        "error": {
            "reason": reason.into(),
            "failure_class": failure_class.as_str()
        }
    }))
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
        "service_id": "subagent",
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
        "service_id": "background",
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
        "service_id": "background",
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
        "service_id": "background",
        "tool_name": BACKGROUND_TOOL_NAME,
        "current_backgrounded": current_backgrounded,
        "max_backgrounded": MAX_BACKGROUNDED_TOOLS_PER_PARENT
    }))
}

pub(super) fn depth_exceeded_payload(parent_subagent_depth: u32) -> String {
    json_string(json!({
        "ok": false,
        "failure_class": "invalid_tool_arguments",
        "code": "subagent_depth_exceeded",
        "path": "/behavior_id",
        "message": "subagent depth ceiling would be exceeded",
        "retryable": false,
        "service_id": "subagent",
        "tool_name": SPAWN_SUBAGENT_TOOL_NAME,
        "parent_subagent_depth": parent_subagent_depth,
        "max_subagent_depth": MAX_SUBAGENT_DEPTH
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
        "service_id": "subagent",
        "tool_name": tool_name,
        "requested_tool_name": requested,
        "allowed_subagent_targets": allowed_targets
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
        "service_id": "subagent",
        "tool_name": tool_name
    }))
}

/// Classify a runtime error string into a FailureClass. Defaults to
/// ToolReturnedError for unknown shapes; managed timeout/cancel markers are
/// handled before this helper so terminal outcomes stay distinct.
#[allow(dead_code)]
pub(super) fn classify_runtime_error(err: &str) -> crate::tool_call_lifecycle::FailureClass {
    use crate::tool_call_lifecycle::FailureClass;
    if err.contains("timeout") || err.contains("deadline") {
        FailureClass::External // R3 will reroute to lifecycle.timeout()
    } else if err.contains("invalid argument") || err.contains("parse") {
        FailureClass::ArgumentInvalid
    } else if err.contains("unavailable") || err.contains("not found") {
        FailureClass::ServiceUnavailable
    } else if err.contains("transport") || err.contains("connection") {
        FailureClass::Transport
    } else {
        FailureClass::ToolReturnedError
    }
}

pub(super) struct RuntimeFailure {
    pub(super) failure_class: crate::tool_call_lifecycle::FailureClass,
    pub(super) command_denial: Option<CommandPolicyDenial>,
}

pub(super) fn classify_runtime_failure(result: &str) -> Option<RuntimeFailure> {
    if result.starts_with("JsonError:") {
        return Some(RuntimeFailure {
            failure_class: crate::tool_call_lifecycle::FailureClass::ArgumentInvalid,
            command_denial: None,
        });
    }
    if result.starts_with("ToolCallError:") {
        if let Some(denial) = parse_command_policy_denial(result) {
            return Some(RuntimeFailure {
                failure_class: crate::tool_call_lifecycle::FailureClass::PolicyDenied,
                command_denial: Some(denial),
            });
        }
        return Some(RuntimeFailure {
            failure_class: classify_runtime_error(result),
            command_denial: None,
        });
    }
    None
}

fn parse_command_policy_denial(result: &str) -> Option<CommandPolicyDenial> {
    let payload = strip_error_prefixes(result);
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    if value
        .get("failure_class")
        .and_then(serde_json::Value::as_str)
        != Some("policyDenied")
    {
        return None;
    }
    CommandPolicyDenial::from_payload_value(&value)
}

fn strip_error_prefixes(mut value: &str) -> &str {
    loop {
        let stripped = value
            .strip_prefix("ToolCallError:")
            .or_else(|| value.strip_prefix("error:"))
            .or_else(|| value.strip_prefix("Error:"))
            .or_else(|| value.strip_prefix("ERROR:"));
        let Some(stripped) = stripped else {
            return value.trim();
        };
        value = stripped.trim();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_failure_extracts_structured_command_policy_denial() {
        let result = r#"ToolCallError: {"ok":false,"failure_class":"policyDenied","denial_reason":"readOnlySubcommandNotAllowlisted","denied_argv":null,"denied_command":"git","denied_argument":null,"denied_subcommand":"commit","denied_prefix":null,"policy_mode":"read_only","policy_network":"inherit","message":"git subcommand is not allowed by the read-only bash tool: commit"}"#;

        let failure = classify_runtime_failure(result).expect("runtime failure");
        let denial = failure.command_denial.expect("command denial");

        assert_eq!(failure.failure_class, FailureClass::PolicyDenied);
        assert_eq!(denial.to_contract(), "readOnlySubcommandNotAllowlisted");
        assert_eq!(denial.reason.denied_command(), Some("git"));
        assert_eq!(denial.reason.denied_subcommand(), Some("commit"));
        assert_eq!(denial.policy_mode, "read_only");
        assert_eq!(denial.policy_network, "inherit");
    }
}
