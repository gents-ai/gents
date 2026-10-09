// The operator's request interrupt: an adapter over the canonical interrupt
// owner (`gents::interrupt`). It resolves the one physical request the
// operator named within the selected node, then hands that exact scope
// to the owner, which latches `interrupt_requested_at` and drains pending
// automated wakes in the same transaction. It never reaches another
// request; a session started by this one's tool call keeps running.
//
// The owner deliberately latches an exact physical row whatever its
// lifecycle state. The refusal of a finished request below is this
// adapter's best-effort pre-check, read before the owner's transaction: a
// request that becomes terminal concurrently may still be latched, which
// the runtime tolerates (the latch then only drains pending wakes).

use std::sync::Arc;

use gents::config_client::ConfigAccess;
use gents::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use gents_desktop_core::client::ClientCore;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::Value;

use crate::types::{DesktopInterruptRequest, InterruptRequestResult};

/// The physical request an operator's interrupt names.
struct Target {
    doc_id: String,
    node_did: String,
    requester_did: Option<String>,
    interrupt_requested_at: Option<String>,
    terminal: bool,
}

/// The one request with this logical id within `node_did`'s documents. Two
/// physical requests under one logical id are ambiguous and refused.
async fn resolve(
    core: &Arc<ClientCore>,
    request_id: &str,
    node_did: Option<&str>,
) -> Result<Target, String> {
    let node_clause = node_did
        .map(str::trim)
        .filter(|did| !did.is_empty())
        .map(|did| format!(r#", node_did: {{ _eq: "{}" }}"#, escape_graphql_string(did)))
        .unwrap_or_default();
    let query = format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }}{node_clause} }}, limit: 2) {{
            _docID node_did requester_did lifecycle_state interrupt_requested_at
        }} }}"#,
        escape_graphql_string(request_id)
    );
    let response = graphql_with_transaction_retry(core.node(), &query, "interrupt target")
        .await
        .map_err(|error| format!("request {request_id} lookup failed: {error:#}"))?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let row = match rows.as_slice() {
        [row] => row,
        [] => {
            return Err(format!(
                "request {request_id} not found in AgentRequest collection"
            ))
        }
        _ => {
            return Err(format!(
                "request {request_id} is ambiguous: more than one physical request carries it"
            ))
        }
    };
    let text = |field: &str| {
        row.get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    };
    Ok(Target {
        doc_id: text("_docID").ok_or("interrupt target missing physical identity")?,
        node_did: text("node_did").ok_or("interrupt target missing node identity")?,
        requester_did: text("requester_did"),
        interrupt_requested_at: text("interrupt_requested_at"),
        terminal: RequestLifecycleState::is_terminal_str(text("lifecycle_state").as_deref()),
    })
}

/// Interrupt exactly `req.request_id`. Only `"userCancelled"` is
/// operator-authentic; the runtime derives every other cause.
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
    let target = resolve(core, &req.request_id, req.node_did.as_deref()).await?;
    let result = |interrupt_requested_at, already_interrupted| InterruptRequestResult {
        request_id: req.request_id.clone(),
        accepted: true,
        interrupt_requested_at,
        already_interrupted,
    };
    if let Some(existing) = target.interrupt_requested_at {
        return Ok(result(Some(existing), true));
    }
    // Best-effort, not atomic: a request already observed finished is not
    // offered to the owner, but one that finishes between this read and the
    // owner's transaction may still be latched.
    if target.terminal {
        return Err(format!(
            "request {} is already terminal and cannot be interrupted",
            req.request_id
        ));
    }
    gents::interrupt::interrupt_request_by_doc_id_with_access(
        &ConfigAccess::Local(core.node_arc()),
        &target.doc_id,
        &target.node_did,
        target.requester_did.as_deref(),
    )
    .await
    .map_err(|error| format!("interrupt of {} failed: {error:#}", req.request_id))?;
    let latched =
        gents::interrupt::fetch_interrupt_requested_at_by_doc_id(core.node(), &target.doc_id)
            .await
            .map_err(|error| {
                format!("interrupt readback of {} failed: {error:#}", req.request_id)
            })?;
    Ok(result(latched, false))
}
