pub use gents_store::test_support::{pin_fixed_signing_identity, PIN_FIXED_DID};

pub async fn native_queue_image(
    node: &std::sync::Arc<defra_node::Node>,
    node_did: &str,
    session_id: &str,
    selection: &[crate::request::lifecycle::queue::FoldedInput],
) -> gents_lean_vocab::QueueImage {
    native_queue_image_with_access(
        &gents_store::config_client::ConfigAccess::local(node.clone()),
        node_did,
        session_id,
        selection,
    )
    .await
}

/// The queue image observed by `access`, such as the Agent that owns the rows.
pub async fn native_queue_image_with_access(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    session_id: &str,
    selection: &[crate::request::lifecycle::queue::FoldedInput],
) -> gents_lean_vocab::QueueImage {
    use gents_protocol::request_lifecycle::RequestLifecycleState;
    #[derive(serde::Deserialize)]
    struct Row {
        request_id: String,
        lifecycle_state: RequestLifecycleState,
    }
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ request_id lifecycle_state }} }}"#,
            gents_store::graphql::escape_graphql_string(session_id)
        ))
        .await
        .unwrap();
    let rows: Vec<Row> = serde_json::from_value(response["data"]["AgentRequest"].clone()).unwrap();
    let state = |id: &str| {
        rows.iter()
            .find(|row| row.request_id == id)
            .map(|row| row.lifecycle_state)
    };
    let active = rows
        .iter()
        .filter(|row| {
            matches!(
                row.lifecycle_state,
                RequestLifecycleState::Claimed | RequestLifecycleState::Processing
            )
        })
        .map(|row| row.request_id.clone())
        .collect::<Vec<_>>();
    let folding = selection
        .iter()
        .map(|folded| folded.request_id.clone())
        .filter(|id| state(id) == Some(RequestLifecycleState::Pending))
        .collect::<Vec<_>>();
    let pending =
        crate::request::lifecycle::pending_user_queue(access, node_did, session_id, node_did)
            .await
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| entry.request_id)
            .filter(|id| !folding.contains(id))
            .collect::<Vec<_>>();
    let terminal = rows
        .iter()
        .filter(|row| row.lifecycle_state.is_terminal())
        .map(|row| row.request_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(active.len() <= 1, "one active request: {active:?}");
    assert_eq!(
        rows.len(),
        active.len() + folding.len() + pending.len() + terminal.len(),
        "every request is active, queued or terminal"
    );
    gents_lean_vocab::QueueImage {
        active: active.into_iter().next(),
        folding,
        pending,
        terminal,
    }
}

pub fn hints(source: gents_protocol::request_input::QueueSource, policy: gents_protocol::request_input::QueuePolicy) -> gents_protocol::request_input::RequestQueue {
    gents_protocol::request_input::RequestQueue {
        delivery: Default::default(),
        position: None,
        source,
        policy,
        key: Some("session:sess-1".to_string()),
        queued_after_request_id: Some("req-1".to_string()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    }
}

pub fn wake_queue_input(queue: gents_protocol::request_input::RequestQueue) -> gents_protocol::request_input::RequestInput {
    gents_protocol::request_input::RequestInput {
        queue: Some(queue),
        ..Default::default()
    }
}

pub fn parent_request(node_did: &str, agent_did: &str, session_id: &str) -> gents_protocol::row::AgentRequest {
    gents_protocol::row::AgentRequest {
        retry_parent_request_doc_id: None,
        purpose: gents_protocol::request_admission::RequestPurpose::Normal,
        doc_id: "parent-doc".to_string(),
        request_id: "parent-request".to_string(),
        node_did: node_did.to_string(),
        requester_did: None,
        agent_did: agent_did.to_string(),
        session_id: session_id.to_string(),
        content: "parent".to_string(),
        max_total_tokens: None,
        input: gents_protocol::request_input::RequestInput::default(),
        execution_origin: Some("interactive".to_string()),
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        deadline: None,
        execution_generation: None,
        execution_lease_expires_at: None,
        execution_lease_secs: None,
        request_hop: 2,
        caused_by_parent_request_id: Some("root-parent-request".to_string()),
        caused_by_parent_request_doc_id: Some("root-parent-request-doc".to_string()),
        caused_by_parent_tool_call_id: Some("root-parent-tool-call".to_string()),
        caused_by_parent_tool_call_doc_id: Some("root-parent-tool-call-doc".to_string()),
        caused_by_trigger_id: None,
        caused_by_trigger_kind: None,
        caused_by_source_doc_id: None,
        caused_by_correlation: None,
        caused_by_trigger_context: None,
        workspace_id: None,
        workspace_owner_node_did: None,
        workspace_authority: None,
        workspace_seal_hash: None,
    }
}


pub fn normalize_pin_fields(fields: &str) -> String {
    let start = fields
        .find("admission_signature: \"")
        .expect("admission signature field present");
    let rest = &fields[start + "admission_signature: \"".len()..];
    let end = rest.find('"').expect("signature value closes");
    let mut normalized = String::with_capacity(fields.len());
    normalized.push_str(&fields[..start + "admission_signature: \"".len()]);
    normalized.push_str("<SIGNATURE>");
    normalized.push_str(&rest[end..]);
    normalized
}


    pub fn pin_parent_request() -> gents_protocol::row::AgentRequest {
        let mut parent = parent_request(PIN_FIXED_DID, PIN_FIXED_DID, "sess-pin-parent");
        parent.request_id = "pin-parent-request".to_string();
        parent.doc_id = "pin-parent-doc".to_string();
        parent.request_hop = 2;
        parent.caused_by_correlation = Some("corr-parent".to_string());
        parent.caused_by_trigger_context = Some(r#"{"a":"b"}"#.to_string());
        parent
    }

