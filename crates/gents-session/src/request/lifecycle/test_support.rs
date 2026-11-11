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
pub(crate) async fn native_queue_image_with_access(
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
