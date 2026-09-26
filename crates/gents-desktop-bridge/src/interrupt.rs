// The operator's request interrupt: latches `interrupt_requested_at` on one
// AgentRequest. It never reaches another request; a session started by this
// one's tool call keeps running until it is interrupted itself.

use std::sync::Arc;

use gents::graphql::escape_graphql_string;
use gents_desktop_core::client::ClientCore;
use gents_protocol::row::AgentRequestRow;
use serde_json::Value;

use crate::types::{DesktopInterruptRequest, InterruptRequestResult};

struct GraphqlAccess;

impl GraphqlAccess {
    async fn execute(
        &self,
        core: &Arc<ClientCore>,
        document: &str,
        operation: &str,
    ) -> Result<Value, String> {
        let response =
            gents::graphql::graphql_with_transaction_retry(core.node(), document, operation)
                .await
                .map_err(|error| error.to_string())?;
        Ok(response.data.unwrap_or(Value::Null))
    }

    async fn write(
        &self,
        core: &Arc<ClientCore>,
        operation: &'static str,
        mutation: &str,
    ) -> Result<Value, String> {
        gents::config_client::ConfigAccess::Local(core.node_arc())
            .write(operation, mutation)
            .await
            .map(|response| response.get("data").cloned().unwrap_or(Value::Null))
            .map_err(|error| format!("{operation} failed: {error}"))
    }
}

/// Fetch a single AgentRequest row by `request_id`. Returns Err if not found.
///
/// When `agent_did` is `Some(did)`, an additional `agent_did` filter is applied
/// so only rows owned by that operator are visible.
async fn fetch_request(
    core: &Arc<ClientCore>,
    access: &GraphqlAccess,
    request_id: &str,
    agent_did: Option<&str>,
) -> Result<AgentRequestRow, String> {
    let escaped = escape_graphql_string(request_id);
    let agent_did_clause = agent_did
        .map(|did| {
            let escaped_did = escape_graphql_string(did);
            format!(r#", agent_did: {{ _eq: "{escaped_did}" }}"#)
        })
        .unwrap_or_default();
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }}{agent_did_clause} }},
                limit: 1
            ) {{
                request_id
                agent_did
                behavior_id
                session_id
                lifecycle_state
                interrupt_requested_at
            }}
        }}"#
    );

    let data = access
        .execute(
            core,
            &query,
            &format!("AgentRequest query for {request_id}"),
        )
        .await?;
    let row = data
        .get("AgentRequest")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .ok_or_else(|| format!("request {request_id} not found in AgentRequest collection"))?;
    serde_json::from_value(row)
        .map_err(|error| format!("invalid AgentRequest row for {request_id}: {error}"))
}

/// Result returned by `latch_request_interrupt`.
#[derive(Debug, Clone)]
pub struct LatchResult {
    /// The RFC-3339 timestamp stored (or already present) in
    /// `interrupt_requested_at`.
    pub interrupt_requested_at: String,
    /// `true` if this call was the first to write the field; `false` if it
    /// was already set (idempotent no-op path).
    pub was_first: bool,
}

/// Latches `interrupt_requested_at` on the `AgentRequest` identified by
/// `request_id`.
///
/// - If the field is already present, returns `LatchResult { was_first: false,
///   interrupt_requested_at: <existing> }` without issuing a mutation.
/// - Otherwise writes `chrono::Utc::now().to_rfc3339()` and returns
///   `LatchResult { was_first: true, interrupt_requested_at: <now> }`.
pub async fn latch_request_interrupt(
    core: &Arc<ClientCore>,
    request_id: &str,
    agent_did: Option<&str>,
) -> Result<LatchResult, String> {
    let access = GraphqlAccess;

    let row = fetch_request(core, &access, request_id, agent_did)
        .await
        .map_err(|e| format!("latch_request_interrupt: {e}"))?;

    // Request.Transition permits interrupt edges only from pending, claimed,
    // and processing. A stale phone button must not write a fresh interrupt
    // latch onto a terminal row: besides corrupting the audit trail, that
    // falsely reports an accepted operator action after the work is done.
    if row.is_terminal() {
        return Err(format!(
            "request {request_id} is already terminal and cannot be interrupted"
        ));
    }

    // 2. If already interrupted, return idempotent result.
    if let Some(existing) = row.interrupt_requested_at {
        return Ok(LatchResult {
            interrupt_requested_at: existing,
            was_first: false,
        });
    }

    // 3. Compute timestamp and write.
    let now = chrono::Utc::now().to_rfc3339();
    let escaped_id = escape_graphql_string(request_id);
    let escaped_now = escape_graphql_string(&now);
    let agent_did_clause = agent_did
        .map(str::trim)
        .filter(|agent_did| !agent_did.is_empty())
        .map(|agent_did| {
            let escaped_agent_did = escape_graphql_string(agent_did);
            format!(r#", agent_did: {{ _eq: "{escaped_agent_did}" }}"#)
        })
        .unwrap_or_default();
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{
                    request_id: {{ _eq: "{escaped_id}" }}{agent_did_clause},
                    lifecycle_state: {{ _in: ["pending", "claimed", "processing"] }},
                    interrupt_requested_at: {{ _eq: null }}
                }},
                input: {{ interrupt_requested_at: "{escaped_now}" }}
            ) {{ _docID }}
        }}"#
    );

    let data = access
        .write(core, "desktop.interrupt.latch_request", &mutation)
        .await?;
    let updated = data
        .get("update_AgentRequest")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default();
    if updated == 0 {
        let current = fetch_request(core, &access, request_id, agent_did)
            .await
            .map_err(|error| format!("latch_request_interrupt recheck: {error}"))?;
        if current.is_terminal() {
            return Err(format!(
                "request {request_id} became terminal before the interrupt could be latched"
            ));
        }
        if let Some(existing) = current.interrupt_requested_at {
            return Ok(LatchResult {
                interrupt_requested_at: existing,
                was_first: false,
            });
        }
        return Err(format!(
            "request {request_id} did not accept an interrupt latch"
        ));
    }

    Ok(LatchResult {
        interrupt_requested_at: now,
        was_first: true,
    })
}

/// Orchestrates the operator's interrupt of one request.
///
/// Only `"userCancelled"` is an operator-authentic cause. Any other value is
/// rejected — the runtime owns deadline/interrupted derivation.
pub async fn interrupt_request(
    core: &Arc<ClientCore>,
    req: &DesktopInterruptRequest,
) -> Result<InterruptRequestResult, String> {
    if req.cause != "userCancelled" {
        return Err(format!(
            "operator may only authentically produce cause=\"userCancelled\", got {:?}",
            req.cause
        ));
    }
    let latched = latch_request_interrupt(core, &req.request_id, req.agent_did.as_deref()).await?;
    Ok(InterruptRequestResult {
        request_id: req.request_id.clone(),
        // idempotent success — always accepted when latched or already latched
        accepted: true,
        interrupt_requested_at: Some(latched.interrupt_requested_at),
        already_interrupted: !latched.was_first,
    })
}
