use crate::store::identity::NodeIdentity;

pub(crate) const PIN_FIXED_KEY_HEX: &str = "4cbf8c1186d2fcb70559342fd142650a5ec5938d26a187d87e2c061b530d7be46edb79d5f548207182f7911b55709c9e4b9961c709486e5ce920e306470fe6d6";
pub(crate) const PIN_FIXED_DID: &str = "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7";

pub(crate) fn pin_fixed_signing_identity(
    dir: &std::path::Path,
) -> crate::store::identity::KeyIdentity {
    let key_bytes: Vec<u8> = (0..PIN_FIXED_KEY_HEX.len())
        .step_by(2)
        .map(|offset| u8::from_str_radix(&PIN_FIXED_KEY_HEX[offset..offset + 2], 16).unwrap())
        .collect();
    let mut key_file = tempfile::NamedTempFile::new_in(dir).expect("create private fixed key");
    std::io::Write::write_all(&mut key_file, &key_bytes).expect("write fixed pinning key");
    let identity = crate::store::identity::KeyIdentity::load_existing(key_file.path(), None)
        .expect("load fixed identity");
    assert_eq!(
        identity.did(),
        PIN_FIXED_DID,
        "fixed pinning key derives a stable DID"
    );
    identity
}

pub(crate) async fn native_queue_image(
    node: &std::sync::Arc<defra_node::Node>,
    node_did: &str,
    session_id: &str,
    selection: &[crate::request::lifecycle::queue::FoldedInput],
) -> gents_lean_vocab::QueueImage {
    native_queue_image_with_access(
        &crate::store::config_client::ConfigAccess::local(node.clone()),
        node_did,
        session_id,
        selection,
    )
    .await
}

/// The queue image observed by `access`, such as the Agent that owns the rows.
pub(crate) async fn native_queue_image_with_access(
    access: &crate::store::config_client::ConfigAccess,
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
            crate::store::graphql::escape_graphql_string(session_id)
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
