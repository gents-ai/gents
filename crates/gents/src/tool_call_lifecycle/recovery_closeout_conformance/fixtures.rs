use crate::tool_call_lifecycle::admission_fixture::PublishedAdmission;

/// Fixture writes to a `@branchable` collection also advance its
/// collection head, so two back-to-back writes can conflict; retry those,
/// bounded.
async fn execute_fixture_write(node: &crate::defra_node::EmbeddedNode, mutation: &str) {
    let mut backoff = std::time::Duration::from_millis(5);
    for _ in 0..8 {
        let result = node.execute(mutation).await;
        let conflicted = result.errors.iter().any(|error| {
            error
                .extensions
                .as_ref()
                .is_some_and(|extensions| extensions.code == "TXN_CONFLICT")
        });
        if !conflicted {
            assert!(!result.has_errors(), "{:?}", result.errors);
            return;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(std::time::Duration::from_millis(200));
    }
    panic!("fixture write kept conflicting: {mutation}");
}

pub(super) async fn update(node: &crate::defra_node::EmbeddedNode, doc_id: &str, fields: &str) {
    let doc_id = crate::graphql::escape_graphql_string(doc_id);
    execute_fixture_write(node, &format!(
        r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, input: {{ {fields} }}) {{ _docID }} }}"#
    ))
    .await;
}

pub(super) async fn update_request(
    node: &crate::defra_node::EmbeddedNode,
    doc_id: &str,
    fields: &str,
) {
    let doc_id = crate::graphql::escape_graphql_string(doc_id);
    execute_fixture_write(node, &format!(
        r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, input: {{ {fields} }}) {{ _docID }} }}"#
    ))
    .await;
}

pub(super) async fn remove_parent(node: &crate::defra_node::EmbeddedNode, doc_id: &str) {
    let doc_id = crate::graphql::escape_graphql_string(doc_id);
    let result = node
        .execute(&format!(
            r#"mutation {{ delete_AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(!result.has_errors(), "{:?}", result.errors);
}

pub(super) async fn completion_obligations(
    node: &crate::defra_node::EmbeddedNode,
    session_id: &str,
    node_did: &str,
) -> (Vec<String>, Vec<serde_json::Value>) {
    let session_id = crate::graphql::escape_graphql_string(session_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ _docID }} AgentRequest(filter: {{ session_id: {{ _eq: "{session_id}" }}, execution_origin: {{ _eq: "scheduled" }} }}) {{ input }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let data = response.data.unwrap();
    let mut notifications = Vec::new();
    for row in data["AgentMessage"].as_array().unwrap() {
        let (_, message) = crate::session::load_canonical_message_from_node(
            node,
            row["_docID"].as_str().unwrap(),
            node_did,
            Some(node_did),
        )
        .await
        .expect("reconstruct canonical recovery notification");
        if let crate::llm::message::Message::User { content } = message {
            for item in content {
                if let crate::llm::message::UserContent::Text(text) = item {
                    if text.text.contains("<tool-completion") {
                        notifications.push(text.text);
                    }
                }
            }
        }
    }
    let wakes = data["AgentRequest"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["input"].clone())
        .collect();
    (notifications, wakes)
}

pub(super) async fn settle(admission: &PublishedAdmission) -> usize {
    crate::background_completion::settle_running_session_message_rows(
        &admission.node,
        &admission.node_did,
    )
    .await
    .unwrap()
}
