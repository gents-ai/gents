use anyhow::Context;
use gents_protocol::output::{MessageBlock, MessagePublication, MessageRole};
use serde_json::Value;

use crate::background_tools::WaitToolArgs;
use crate::config_client::ConfigApplyTxn;
use crate::graphql::escape_graphql_string;
use crate::tool_call_lifecycle::{query, ToolCallState};

#[derive(Debug, thiserror::Error)]
enum WaitObservationError {
    #[error("Goal wait observation omitted {name} rows")]
    MissingRows { name: String },
    #[error("Goal wait observation omitted {field}")]
    MissingField { field: String },
    #[error("decode canonical wait_process result")]
    DecodeResult(#[source] serde_json::Error),
    #[error("malformed canonical wait_process tool error")]
    MalformedArgumentError,
    #[error("canonical wait_process reply conflicts with accepted handle")]
    ConflictsWithAcceptedHandle,
    #[error("canonical wait_process reply has unknown status")]
    UnknownStatus,
    #[error("canonical wait_process reply conflicts with target state")]
    ConflictsWithTargetState,
    #[error("canonical wait_process reply has unknown outcome")]
    UnknownOutcome,
}

type Result<T, E = WaitObservationError> = std::result::Result<T, E>;

pub(super) enum WaitEvidence {
    Absent,
    Running,
    Invalid(anyhow::Error),
    Unavailable(anyhow::Error),
}

fn rows<'a>(response: &'a Value, name: &str) -> Result<&'a Vec<Value>> {
    response
        .get("data")
        .and_then(|data| data.get(name))
        .and_then(Value::as_array)
        .ok_or_else(|| WaitObservationError::MissingRows {
            name: name.to_owned(),
        })
}

fn required<'a>(row: &'a Value, field: &str) -> Result<&'a str> {
    row.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| WaitObservationError::MissingField {
            field: field.to_owned(),
        })
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
/// waited for this exact handle while it was running.
fn timed_out_running_handle(result: &str, accepted_handle: &str) -> Result<bool> {
    let value: Value = serde_json::from_str(result).map_err(WaitObservationError::DecodeResult)?;
    if value["failure_class"] == "argument_invalid" {
        if !valid_argument_error(&value) {
            return Err(WaitObservationError::MalformedArgumentError);
        }
        return Ok(false);
    }
    if !(value["tool_call_id"].as_str() == Some(accepted_handle)
        && value["await_mode"] == "background"
        && value["tool_name"]
            .as_str()
            .is_some_and(|name| !name.is_empty())
        && value["result"].is_string())
    {
        return Err(WaitObservationError::ConflictsWithAcceptedHandle);
    }
    let status = value["status"]
        .as_str()
        .and_then(ToolCallState::from_persisted)
        .ok_or(WaitObservationError::UnknownStatus)?;
    if !(value["ok"] == (status == ToolCallState::Completed)
        && if status == ToolCallState::Completed {
            value["error"].is_null()
        } else {
            value["error"]["failure_class"] == "external"
        })
    {
        return Err(WaitObservationError::ConflictsWithTargetState);
    }
    let reason = value["error"].get("reason").and_then(Value::as_str);
    if status == ToolCallState::Running && reason == Some("wait_timeout") {
        return Ok(true);
    }
    if !(matches!(
        reason,
        Some("caller_interrupted" | "caller_deadline_exceeded" | "wait_timeout" | "terminal")
    ) || (status == ToolCallState::Completed && value["error"].is_null()))
    {
        return Err(WaitObservationError::UnknownOutcome);
    }
    Ok(false)
}

/// Re-read exact accepted wait controls and targets inside the transaction
/// that will CAS the Goal and publish its child.
pub(super) async fn observe_intentional_wait(
    txn: &ConfigApplyTxn<'_>,
    parent_doc_id: &str,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> WaitEvidence {
    match observe_waits(txn, parent_doc_id, node_did, session_id, requester_did).await {
        Ok(true) => WaitEvidence::Running,
        Ok(false) => WaitEvidence::Absent,
        Err(error) if crate::config_client::is_transaction_step_unavailable(&error) => {
            WaitEvidence::Unavailable(error)
        }
        Err(error) => WaitEvidence::Invalid(error),
    }
}

async fn observe_waits(
    txn: &ConfigApplyTxn<'_>,
    parent_doc_id: &str,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> anyhow::Result<bool> {
    let scope = crate::session::session_scope_filter(node_did, session_id, requester_did);
    let headers = crate::session::load_request_headers_in_txn(
        txn,
        session_id,
        node_did,
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
            node_did,
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
                    lifecycle_state status await_mode spawned_by_tool_call_doc_id }} }}"#
            ))
            .await?;
        let targets = rows(&targets, "AgentToolCall")?;
        anyhow::ensure!(targets.len() <= 1, "ambiguous waited background handle");
        let Some(target) = targets.first() else {
            continue;
        };
        required(target, "_docID")?;
        anyhow::ensure!(
            required(target, "tool_call_id")? == handle && target["await_mode"] == "background",
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
            node_did,
            session_id,
            requester_did,
        )
        .await?;
        anyhow::ensure!(
            spawn_parent.tool_name == "spawn_process"
                && spawn_parent.request_doc_id == required(target, "request_doc_id")?,
            "spawned background lacks accepted spawn_process parent"
        );
        let delivery_owed = target_state.is_terminal()
            && target["status"]
                .as_str()
                .is_some_and(|status| status.starts_with("completionPending"));
        if target_state == ToolCallState::Running || delivery_owed {
            running = true;
        }
    }
    Ok(running)
}
