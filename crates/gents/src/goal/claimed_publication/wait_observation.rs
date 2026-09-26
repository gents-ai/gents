use anyhow::{Context, Result};
use gents_protocol::output::{MessageBlock, MessagePublication, MessageRole};
use serde_json::Value;

use crate::background_tools::WaitToolArgs;
use crate::config_client::ConfigApplyTxn;
use crate::graphql::escape_graphql_string;
use crate::tool_call_lifecycle::{query, ToolCallState};

/// Goal wait evidence read inside the publication transaction.
pub(super) enum WaitEvidence {
    Absent,
    Running,
    /// Fail-closed: the evidence cannot be interpreted.
    Invalid(anyhow::Error),
}

fn rows<'a>(response: &'a Value, name: &str) -> Result<&'a Vec<Value>> {
    response
        .get("data")
        .and_then(|data| data.get(name))
        .and_then(Value::as_array)
        .with_context(|| format!("Goal wait observation omitted {name} rows"))
}

fn required<'a>(row: &'a Value, field: &str) -> Result<&'a str> {
    row.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("Goal wait observation omitted {field}"))
}

fn valid_argument_error(value: &Value) -> bool {
    value["failure_class"] == "argument_invalid"
        && value["ok"] == false
        && value["service_id"] == "process"
        && value["tool_name"] == "wait_process"
        && matches!(value["path"].as_str(), Some("/" | "/tool_call_id"))
        && value["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty())
        && value["retryable"] == false
}

/// Only the canonical bounded-timeout envelope says that the agent deliberately
/// waited for this exact handle while it was running. A normal process-control
/// tool error or another valid wait outcome is not a Goal suspension receipt.
fn timed_out_running_handle(result: &str, accepted_handle: &str) -> Result<bool> {
    let value: Value =
        serde_json::from_str(result).context("decode canonical wait_process result")?;
    if value["failure_class"] == "argument_invalid" {
        anyhow::ensure!(
            valid_argument_error(&value),
            "malformed canonical wait_process tool error"
        );
        return Ok(false);
    }
    anyhow::ensure!(
        value["tool_call_id"].as_str() == Some(accepted_handle)
            && value["await_mode"] == "background"
            && value["tool_name"]
                .as_str()
                .is_some_and(|name| !name.is_empty())
            && value["result"].is_string(),
        "canonical wait_process reply conflicts with accepted handle"
    );
    let status = value["status"]
        .as_str()
        .and_then(ToolCallState::from_persisted)
        .context("canonical wait_process reply has unknown status")?;
    anyhow::ensure!(
        value["ok"] == (status == ToolCallState::Completed)
            && if status == ToolCallState::Completed {
                value["error"].is_null()
            } else {
                value["error"]["failure_class"] == "external"
            },
        "canonical wait_process reply conflicts with target state"
    );
    let reason = value["error"].get("reason").and_then(Value::as_str);
    if status == ToolCallState::Running && reason == Some("wait_timeout") {
        return Ok(true);
    }
    anyhow::ensure!(
        matches!(
            reason,
            Some("caller_interrupted" | "caller_deadline_exceeded" | "wait_timeout" | "terminal")
        ) || (status == ToolCallState::Completed && value["error"].is_null()),
        "canonical wait_process reply has unknown outcome"
    );
    Ok(false)
}

/// Re-read exact accepted wait controls and targets inside the transaction
/// that will CAS the Goal and publish its child. A missing target in a complete
/// scan is a lost/finished process; an unreadable or ambiguous scan is an error.
pub(super) async fn observe_intentional_wait(
    txn: &ConfigApplyTxn<'_>,
    parent_doc_id: &str,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> WaitEvidence {
    match observe_waits(txn, parent_doc_id, agent_did, session_id, requester_did).await {
        Ok(true) => WaitEvidence::Running,
        Ok(false) => WaitEvidence::Absent,
        Err(error) => WaitEvidence::Invalid(error),
    }
}

async fn observe_waits(
    txn: &ConfigApplyTxn<'_>,
    parent_doc_id: &str,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<bool> {
    let scope = crate::session::session_scope_filter(agent_did, session_id, requester_did);
    let headers = crate::session::load_request_headers_in_txn(
        txn,
        session_id,
        agent_did,
        requester_did,
        parent_doc_id,
    )
    .await?;
    let mut controls = std::collections::BTreeSet::new();
    for header in &headers {
        if header.message.role != MessageRole::Assistant
            || !matches!(
                &header.message.publication,
                MessagePublication::RequestExecution { .. }
            )
        {
            continue;
        }
        for block in &header.message.blocks {
            if let MessageBlock::ToolCall {
                tool_call_doc_id,
                name,
                ..
            } = block
            {
                if name == "wait_process" {
                    anyhow::ensure!(
                        controls.insert(tool_call_doc_id.clone()),
                        "duplicate accepted wait_process physical binding"
                    );
                }
            }
        }
    }
    let mut running = false;
    for control_doc_id in controls {
        let accepted = query::load_tool_call_read_in_txn(
            txn,
            &control_doc_id,
            agent_did,
            session_id,
            requester_did,
        )
        .await?;
        anyhow::ensure!(
            accepted.request_doc_id == parent_doc_id && accepted.tool_name == "wait_process",
            "accepted wait_process control crossed its physical parent"
        );
        anyhow::ensure!(
            accepted.lifecycle_state.is_terminal(),
            "terminal Goal parent has an unsettled wait_process control"
        );
        if accepted.lifecycle_state != ToolCallState::Completed {
            // Deadline/cancel/failure settlement is a canonical diagnostic, not
            // evidence that an intentional bounded wait observed a running tool.
            // Terminalization cancels a never-started control without any reply.
            continue;
        }
        accepted
            .result
            .as_ref()
            .context("completed wait_process lacks canonical invocation reply")?;
        let result = accepted
            .raw_result
            .as_deref()
            .context("completed wait_process lacks exact raw invocation reply")?;
        let args: WaitToolArgs = match serde_json::from_str(&accepted.arguments) {
            Ok(args) => args,
            Err(_) => {
                let value: Value = serde_json::from_str(result)
                    .context("decode canonical invalid wait arguments result")?;
                anyhow::ensure!(
                    valid_argument_error(&value),
                    "invalid accepted wait arguments lack canonical argument error"
                );
                continue;
            }
        };
        let handle = args.tool_call_id.trim();
        if handle.is_empty() {
            anyhow::ensure!(
                !timed_out_running_handle(result, handle)?,
                "empty wait handle cannot be a running timeout"
            );
            continue;
        }
        if !timed_out_running_handle(result, handle)? {
            continue;
        }
        let handle_escaped = escape_graphql_string(handle);
        let targets = txn
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ {scope}, tool_call_id: {{ _eq: "{handle_escaped}" }} }},
                    limit: 2) {{ _docID tool_call_id request_doc_id tool_call_key
                    lifecycle_state await_mode child_request_id spawned_by_tool_call_doc_id }} }}"#
            ))
            .await?;
        let targets = rows(&targets, "AgentToolCall")?;
        anyhow::ensure!(targets.len() <= 1, "ambiguous waited background handle");
        let Some(target) = targets.first() else {
            continue;
        };
        required(target, "_docID")?;
        anyhow::ensure!(
            required(target, "tool_call_id")? == handle
                && target["await_mode"] == "background"
                && target["child_request_id"].is_null(),
            "waited target is not an authorized background tool"
        );
        let target_state = required(target, "lifecycle_state")?;
        let target_state = ToolCallState::from_persisted(target_state)
            .context("waited target has unknown lifecycle state")?;
        let spawn_parent_doc_id = required(target, "spawned_by_tool_call_doc_id")?;
        anyhow::ensure!(
            handle == format!("spawned:{spawn_parent_doc_id}")
                && required(target, "tool_call_key")?
                    == format!("{spawn_parent_doc_id}:spawned-background"),
            "spawned background handle conflicts with its physical parent"
        );
        let spawn_parent = query::load_tool_call_read_in_txn(
            txn,
            spawn_parent_doc_id,
            agent_did,
            session_id,
            requester_did,
        )
        .await?;
        anyhow::ensure!(
            spawn_parent.tool_name == "spawn_process"
                && spawn_parent.request_doc_id == required(target, "request_doc_id")?,
            "spawned background lacks accepted spawn_process parent"
        );
        if target_state == ToolCallState::Running {
            running = true;
        }
    }
    Ok(running)
}
