// Adapter over the canonical interrupt owner (`gents::request::interrupt`), which
// latches `interrupt_requested_at` and drains pending automated wakes in one
// transaction for the exact physical row handed to it. This pre-check reads
// terminality before the owner's transaction; a request that becomes terminal
// concurrently may still be latched, which the runtime tolerates.

use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use serde_json::Value;

/// The request's interrupt latch, and whether an earlier interrupt set it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestInterrupt {
    pub interrupt_requested_at: Option<String>,
    pub already_interrupted: bool,
}

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
    access: &ConfigAccess,
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
    let response = access
        .execute(&query)
        .await
        .map_err(|error| format!("request {request_id} lookup failed: {error:#}"))?;
    let rows = response
        .pointer("/data/AgentRequest")
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

/// Interrupt exactly `request_id` within `node_did`'s documents over `access`.
pub async fn interrupt_request_on(
    access: &ConfigAccess,
    node_did: &str,
    request_id: &str,
) -> Result<RequestInterrupt, String> {
    gents::runtime::lifecycle_hooks::compose_client();
    let target = resolve(access, request_id, Some(node_did)).await?;
    if let Some(existing) = target.interrupt_requested_at {
        return Ok(RequestInterrupt {
            interrupt_requested_at: Some(existing),
            already_interrupted: true,
        });
    }
    // Best-effort, not atomic: a request that finishes between this read and
    // the owner's transaction may still be latched.
    if target.terminal {
        return Err(format!(
            "request {request_id} is already terminal and cannot be interrupted"
        ));
    }
    gents_session::request::interrupt::interrupt_request_by_doc_id_with_access(
        access,
        &target.doc_id,
        &target.node_did,
        target.requester_did.as_deref(),
    )
    .await
    .map_err(|error| format!("interrupt of {request_id} failed: {error:#}"))?;
    let observed = resolve(access, request_id, Some(node_did)).await?;
    if observed.doc_id != target.doc_id {
        return Err("interrupt readback changed physical request identity".into());
    }
    Ok(RequestInterrupt {
        interrupt_requested_at: observed.interrupt_requested_at,
        already_interrupted: false,
    })
}
