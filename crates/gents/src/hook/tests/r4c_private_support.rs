//! Test-only adapter from a real accepted provider turn to the existing hook.
//! It reuses the publication owner; it never constructs an AcceptedToolCall.

use super::*;

/// Bind the exact configured parent agent to a signed, claimed request.
/// The direct-hook R4C tests retain their assertions but no longer invent a
/// processing row without the execution owner or an accepted provider turn.
pub(super) async fn bind_accepted_request(
    db: &crate::support::TestDb,
    hook: &DefraSessionHook,
    agent_id: &str,
    request_id: &str,
    session_id: &str,
    deadline_at: chrono::DateTime<chrono::Utc>,
) {
    let did = db.node_identity.did();
    assert_eq!(hook.node_did, did, "hook and signed request owner differ");
    crate::session::ensure_session_with_agent_id_and_requester_did(
        db.node.as_ref(),
        session_id,
        did,
        agent_id,
        Some(did),
    )
    .await
    .expect("ensure canonical R4C parent session");
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        request_id,
        did,
        did,
        agent_id,
        session_id,
        "R4C accepted control fixture",
        "interactive",
        now,
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(did),
    );
    crate::sign_agent_request_create(db.node_identity.as_ref(), &mut create)
        .await
        .expect("sign canonical R4C parent request");
    let created = db.node.execute(&create.graphql_mutation().unwrap()).await;
    assert!(
        !created.has_errors(),
        "create canonical R4C parent request: {:?}",
        created.errors
    );
    let request_doc_id = crate::graphql::single_mutation_document(&created, "create_AgentRequest")
        .unwrap()
        .unwrap()["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    let request_doc = crate::graphql::escape_graphql_string(&request_doc_id);
    let loaded = db.node.execute(&format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{request_doc}" }} }}, limit: 1) {{ {} }} }}"#, crate::watcher::AGENT_REQUEST_FIELDS)).await;
    let row: gents_protocol::row::AgentRequestRow =
        crate::graphql::first_row(&loaded, "AgentRequest")
            .unwrap()
            .expect("created R4C request row");
    let mut lifecycle = crate::lifecycle::RequestLifecycle::new_with_node_did(
        db.node.clone(),
        agent_id,
        did,
        row.try_into().expect("canonical R4C request"),
        60,
    );
    assert_eq!(
        lifecycle.claim().await.unwrap(),
        crate::lifecycle::ClaimOutcome::Claimed
    );
    let writer =
        crate::streaming::DefraStreamWriter::new(db.node.clone(), did, std::time::Duration::ZERO);
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    hook.set_active_request_lineage(Some(request_id.to_owned()), Some(did.to_owned()))
        .await
        .expect("bind accepted R4C request lineage");
    hook.set_request_deadline_at(Some(deadline_at)).await;
    hook_execution_fixtures().lock().await.insert(
        hook_execution_fixture_key(hook, request_id),
        HookExecutionFixture {
            lifecycle,
            writer,
            turn: 0,
        },
    );
}

pub(super) async fn accepted_call(
    hook: &DefraSessionHook,
    name: &str,
    provider_call_id: Option<String>,
    internal_call_id: &str,
    args: &str,
) -> ToolCallHookAction {
    accept_hook_tool_call(
        hook,
        internal_call_id,
        name,
        args,
        provider_call_id.as_deref(),
    )
    .await;
    hook.on_tool_call(name, provider_call_id, internal_call_id, args)
        .await
}

pub(super) async fn assert_accepted_control_rows(
    hook: &DefraSessionHook,
    tool_name: &str,
    expected: usize,
) {
    let session_id = hook.session_id().await.expect("accepted control session");
    let session = crate::graphql::escape_graphql_string(&session_id);
    let name = crate::graphql::escape_graphql_string(tool_name);
    let response = hook.node.execute(&format!(r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session}" }}, tool_name: {{ _eq: "{name}" }} }}) {{ _docID request_doc_id message_sequence lifecycle_state }} }}"#)).await;
    assert!(
        !response.has_errors(),
        "accepted control row query: {:?}",
        response.errors
    );
    let rows = response.data.as_ref().unwrap()["AgentToolCall"]
        .as_array()
        .unwrap();
    assert_eq!(rows.len(), expected, "exact accepted {tool_name} row count");
    for row in rows {
        let tool_doc_id = row["_docID"].as_str().unwrap();
        let request_doc_id = row["request_doc_id"].as_str().unwrap();
        let sequence = row["message_sequence"].as_u64().unwrap();
        let request_doc = crate::graphql::escape_graphql_string(request_doc_id);
        let headers = hook.node.execute(&format!(r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{session}" }}, request_doc_id: {{ _eq: "{request_doc}" }}, sequence: {{ _eq: {sequence} }} }}) {{ _docID }} }}"#)).await;
        assert!(
            !headers.has_errors(),
            "accepted control header query: {:?}",
            headers.errors
        );
        let header_rows = headers.data.as_ref().unwrap()["AgentMessage"]
            .as_array()
            .unwrap();
        assert_eq!(header_rows.len(), 1, "one exact accepted assistant header");
        let header_doc_id = header_rows[0]["_docID"].as_str().unwrap();
        let requester = hook.active_requester_did().await;
        let (header, _) = crate::session::load_canonical_message_from_node(
            hook.node.as_ref(),
            header_doc_id,
            &hook.node_did,
            requester.as_deref(),
        )
        .await
        .expect("load canonical accepted control header");
        assert!(
            header.blocks.iter().any(|block| matches!(block,
                gents_protocol::output::MessageBlock::ToolCall { tool_call_doc_id, name, .. }
                    if tool_call_doc_id == tool_doc_id && name == tool_name
            )),
            "accepted header must bind exact physical {tool_name} row"
        );
    }
}
