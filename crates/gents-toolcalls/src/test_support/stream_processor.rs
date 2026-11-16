use std::sync::Arc;
use std::time::Duration;

use gents_inference::llm::message::{
    Message, Text, ToolCall, ToolFunction, ToolResultContent, UserContent,
};
use gents_store::identity::NodeIdentity;

use crate::call::hook::DefraSessionHook;
use crate::call::hook::FailurePolicy;
use gents_loop::loop_stream::LoopStreamItem;
use gents_session::request::lifecycle::{ClaimOutcome, RequestLifecycle};
use gents_session::request::streaming::DefraStreamWriter;

pub fn user_text_message(text: &str) -> Message {
    Message::User {
        content: vec![UserContent::Text(Text {
            text: text.to_string(),
        })],
    }
}

/// A request claimed by its signed client and executed by its signed Agent.
/// The hook and stream writer act under the Agent's managed access; reads of
/// Agent-owned rows go through `access`, never the node identity.
pub struct SignedRun {
    pub db: gents_inference::test_support::TestDb,
    pub user: gents_store::identity::KeyIdentity,
    pub node: Arc<defra_node::Node>,
    pub runtime_did: String,
    pub access: gents_store::config_client::ConfigAccess,
    pub hook: DefraSessionHook,
    pub lifecycle: RequestLifecycle,
    pub writer: DefraStreamWriter,
    pub session_id: String,
    pub request_id: String,
}

pub async fn signed_run(name: &str, batch_interval: Duration) -> SignedRun {
    use gents_protocol::request_admission::{
        AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
    };

    let (db, user, owner, binding) = gents_inference::test_support::signed_test_node(
        name,
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = db.node.clone();
    let runtime_did = db.node_identity.did().to_owned();
    let agent =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding).unwrap();
    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let request_id = format!("request-{name}-{}", uuid::Uuid::new_v4());
    let session_id = format!("session-{name}-{}", uuid::Uuid::new_v4());
    let create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        &request_id,
        runtime_did.clone(),
        user.did(),
        agent.did(),
        &session_id,
        "spawn",
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        AgentRequestAdmissionRecord::client(user.did()),
    );
    let mut lifecycle = crate::call::lifecycle::admission_fixture::pending_client_fixture_request(
        &node,
        &owner,
        db.node_identity.clone(),
        &user,
        agent.as_ref(),
        create,
    )
    .await
    .unwrap();
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    let hook = DefraSessionHook::resume_with_actor_policy(
        node.clone(),
        &session_id,
        &runtime_did,
        Some(user.did()),
        agent.did().parse().unwrap(),
        FailurePolicy::default(),
    )
    .await
    .unwrap()
    .with_managed_identity(agent.as_ref())
    .unwrap();
    let writer =
        DefraStreamWriter::new_for_lifecycle(node.clone(), &lifecycle, batch_interval).unwrap();
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    SignedRun {
        db,
        user,
        node,
        runtime_did,
        access,
        hook,
        lifecycle,
        writer,
        session_id,
        request_id,
    }
}

/// Binds the hook to the claimed request before its first provider attempt.
/// Mirrors the daemon: tool-call persistence requires the claimed deadline.
pub async fn bind_active_request(hook: &DefraSessionHook, lifecycle: &RequestLifecycle) {
    let request = lifecycle.request();
    hook.set_active_request_binding(
        Some(request.request_id.clone()),
        Some(request.doc_id.clone()),
        request.requester_did.clone(),
    )
    .await;
    hook.set_request_deadline_at(lifecycle.claimed_deadline_at())
        .await;
}

/// A read through the Agent's managed access, shaped like the node's GraphQL response.
pub struct ReadResponse {
    pub data: Option<serde_json::Value>,
    pub errors: Vec<serde_json::Value>,
}

impl ReadResponse {
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }
}

pub async fn exec(access: &gents_store::config_client::ConfigAccess, query: &str) -> ReadResponse {
    let response = access.execute(query).await.expect("agent-scoped read");
    ReadResponse {
        data: response.get("data").cloned(),
        errors: response
            .get("errors")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default(),
    }
}

pub async fn count_segments(access: &gents_store::config_client::ConfigAccess, doc: &str) -> usize {
    let response = access
        .execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            gents_store::graphql::escape_graphql_string(doc)
        ))
        .await
        .unwrap();
    response["data"]["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .len()
}

#[derive(Debug)]
pub struct PersistedMessageShape {
    pub sequence: u32,
    pub role: String,
    pub content: String,
}

pub async fn load_message_shapes_in_scope(
    access: &gents_store::config_client::ConfigAccess,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Vec<PersistedMessageShape> {
    let scope_node_did = gents_store::graphql::escape_graphql_string(node_did);
    let scope_requester = requester_did.map_or_else(
        || "null".to_owned(),
        |did| format!("\"{}\"", gents_store::graphql::escape_graphql_string(did)),
    );
    let session_id = gents_store::graphql::escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{session_id}" }},
                    node_did: {{ _eq: "{scope_node_did}" }}, requester_did: {{ _eq: {scope_requester} }} }},
                order: {{ sequence: ASC }}
            ) {{ _docID }}
        }}"#
    );
    let response = access.execute(&query).await.unwrap();
    let rows = response["data"]["AgentMessage"].as_array().unwrap();
    let mut shapes = Vec::with_capacity(rows.len());
    for row in rows {
        let doc_id = row["_docID"].as_str().expect("physical message identity");
        let (header, message) =
            gents_session::session::load_canonical_message(access, doc_id, node_did, requester_did)
                .await
                .expect("exact canonical message reconstructs");
        shapes.push(PersistedMessageShape {
            sequence: header.sequence,
            role: serde_json::to_value(header.role)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned(),
            content: serde_json::to_string(&message).unwrap(),
        });
    }
    shapes
}

pub fn text_item(text: &str) -> Result<LoopStreamItem, gents_loop::error::LoopStreamError> {
    Ok(LoopStreamItem::Text(text.to_string()))
}

pub fn tool_call_item(
    name: &str,
    args_json: &str,
    internal_id: &str,
) -> Result<LoopStreamItem, gents_loop::error::LoopStreamError> {
    tool_call_item_with_ids(name, args_json, internal_id, internal_id, None)
}

pub fn tool_call_item_with_ids(
    name: &str,
    args_json: &str,
    tool_id: &str,
    internal_id: &str,
    call_id: Option<&str>,
) -> Result<LoopStreamItem, gents_loop::error::LoopStreamError> {
    Ok(LoopStreamItem::ToolCall {
        tool_call: ToolCall {
            raw_arguments: None,
            id: tool_id.to_string(),
            call_id: call_id.map(ToOwned::to_owned),
            function: ToolFunction {
                name: name.to_string(),
                arguments: serde_json::from_str(args_json).unwrap(),
            },
            signature: None,
            additional_params: None,
        },
        internal_call_id: internal_id.to_string(),
    })
}

pub fn tool_result_item(
    tool_id: &str,
    result_json: &str,
    internal_id: &str,
) -> Result<LoopStreamItem, gents_loop::error::LoopStreamError> {
    tool_result_item_with_call_id(tool_id, None, result_json, internal_id)
}

pub fn tool_result_item_with_call_id(
    tool_id: &str,
    call_id: Option<&str>,
    result_json: &str,
    internal_id: &str,
) -> Result<LoopStreamItem, gents_loop::error::LoopStreamError> {
    Ok(LoopStreamItem::ToolResult {
        tool_result: gents_inference::llm::message::ToolResult {
            id: tool_id.to_string(),
            call_id: call_id.map(ToOwned::to_owned),
            content: vec![ToolResultContent::text(result_json)],
        },
        internal_call_id: internal_id.to_string(),
    })
}

pub fn final_item(
    response_text: &str,
) -> Result<LoopStreamItem, gents_loop::error::LoopStreamError> {
    Ok(LoopStreamItem::Final {
        text: response_text.to_string(),
    })
}

pub fn turn_retracted_item(
    turn: usize,
    attempt: u32,
) -> Result<LoopStreamItem, gents_loop::error::LoopStreamError> {
    Ok(LoopStreamItem::TurnRetracted {
        turn,
        attempt,
        backoff: std::time::Duration::ZERO,
    })
}
