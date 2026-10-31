use super::fixtures::exec;
use super::*;

pub(super) async fn seed_agent_request(node: &EmbeddedNode, request_id: &str, requester_did: &str) {
    let session_id = format!("{request_id}-session");
    seed_agent_request_for_session(node, request_id, &session_id, Some(requester_did)).await;
}

async fn seed_agent_request_for_session(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    requester_did: Option<&str>,
) {
    let request_id = escape_graphql_string(request_id);
    let node_did = "did:key:host";
    let requester_did_field = crate::session::requester_did_create_field(requester_did);
    let session_id = escape_graphql_string(session_id);
    let agent_id = escape_graphql_string(&format!("{node_did}:default"));
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                purpose: "normal",
                node_did: "{node_did}",
                {requester_did_field}
                agent_id: "{agent_id}",
                session_id: "{session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "issue 604 filtered replay",
                lifecycle_state: "processing",
                backend_id: "",
                execution_origin: "interactive",
                failure_reason: "",
                created_at: "2026-07-06T00:00:00Z",
                deadline: "2026-07-06T01:00:00Z",
                retry_count: 0,
                max_retries: 3,
                request_hop: 0
            }}) {{ _docID }}
        }}"#
    );
    exec(node, &mutation, "seed AgentRequest").await;
}

pub(super) async fn seed_agent_tool_call(
    node: &EmbeddedNode,
    tool_call_id: &str,
    requester_did: &str,
) {
    let tool_call_id = escape_graphql_string(tool_call_id);
    let requester_did = escape_graphql_string(requester_did);
    let tool_call_key = escape_graphql_string(&format!("issue-604-session:{tool_call_id}"));
    let mutation = format!(
        r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "{tool_call_key}",
                request_id: "parent-match",
                session_id: "issue-604-session",
                node_did: "did:key:coord",
                requester_did: "{requester_did}",
                message_sequence: 1,
                tool_name: "agent_new",
                tool_call_id: "{tool_call_id}",
                lifecycle_state: "running",
                started_at: "2026-07-06T00:00:00Z",
                await_mode: "background"
            }}) {{ _docID }}
        }}"#
    );
    exec(node, &mutation, "seed AgentToolCall").await;
}

pub(super) async fn seed_agent_target_return_artifacts(
    node: &Arc<EmbeddedNode>,
    suffix: &str,
    requester_did: Option<&str>,
) -> (String, String, String, u32) {
    let request_id = format!("return-{suffix}-response");
    let session_id = format!("return-{suffix}-session");
    let node_did = "did:key:host";
    let agent_id = "did:key:host:default";

    seed_agent_request_for_session(node, &request_id, &session_id, requester_did).await;

    crate::session::ensure_session_with_agent_id_and_requester_did(
        node,
        &session_id,
        node_did,
        agent_id,
        requester_did,
    )
    .await
    .expect("create routed AgentSession");
    let response = node.execute(&format!(
        r#"mutation {{ update_AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "pending" }}) {{ {} }} }}"#,
        escape_graphql_string(&request_id),
        crate::request::watcher::AGENT_REQUEST_FIELDS,
    )).await;
    let request = crate::request::watcher::agent_request_from_mutation_response(
        &response,
        "update_AgentRequest",
    )
    .expect("decode routed request")
    .expect("routed request");
    let mut lifecycle = crate::request::lifecycle::RequestLifecycle::new_with_node_did(
        Arc::clone(node),
        "default",
        node_did,
        request,
        60,
    );
    assert_eq!(
        lifecycle.claim().await.expect("claim routed request"),
        crate::request::lifecycle::ClaimOutcome::Claimed
    );
    let writer = crate::request::streaming::DefraStreamWriter::new(
        Arc::clone(node),
        node_did,
        Duration::from_secs(60),
    );
    lifecycle
        .begin_owned_execution(&writer)
        .await
        .expect("begin routed execution");
    writer
        .start_provider_attempt(
            &lifecycle.request().doc_id,
            0,
            0,
            gents_protocol::rendered_request::CaptureScope {
                kind: gents_protocol::rendered_request::CaptureScopeKind::Inference,
                seq: 0,
            },
        )
        .await;
    let published = writer
        .publish_native_turn(
            &lifecycle,
            0,
            0,
            &gents_protocol::message::Message::assistant("child result"),
        )
        .await
        .expect("publish routed child result turn");
    lifecycle
        .terminalize_owned(
            crate::request::lifecycle::RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::Message {
                message_doc_id: published.message_doc_id.clone(),
            },
            None,
        )
        .await
        .expect("complete routed request");
    (
        request_id,
        session_id,
        published.message_doc_id,
        published.sequence,
    )
}
