use crate::tools::call::lifecycle::admission_fixture::PublishedAdmission;

/// Fixture writes to a `@branchable` collection also advance its
/// collection head, so two back-to-back writes can conflict; retry those,
/// bounded. Writes go through the owning actor's access: ACP hides and
/// refuses the Agent-owned rows for any other identity. A client-submitted
/// parent request is owned by its client, not its Agent.
async fn execute_fixture_write(access: &crate::ConfigAccess, operation: &'static str, mutation: &str) {
    let mut backoff = std::time::Duration::from_millis(5);
    let mut last_error = None;
    for _ in 0..8 {
        match access.write(operation, mutation).await {
            Ok(_) => return,
            Err(error) => last_error = Some(error),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(std::time::Duration::from_millis(200));
    }
    panic!("fixture write kept failing: {mutation}: {last_error:?}");
}

pub(super) async fn update(access: &crate::ConfigAccess, doc_id: &str, fields: &str) {
    let doc_id = crate::store::graphql::escape_graphql_string(doc_id);
    execute_fixture_write(access, "fixture.tool_call.update", &format!(
        r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, input: {{ {fields} }}) {{ _docID }} }}"#
    ))
    .await;
}

pub(super) async fn update_request(access: &crate::ConfigAccess, doc_id: &str, fields: &str) {
    let doc_id = crate::store::graphql::escape_graphql_string(doc_id);
    execute_fixture_write(access, "fixture.request.update", &format!(
        r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, input: {{ {fields} }}) {{ _docID }} }}"#
    ))
    .await;
}

/// The parent is client-owned, so only its client may delete it, and a
/// client endpoint deletes through its signed native graph.
pub(super) async fn remove_parent(client: &crate::ConfigAccess, doc_id: &str) {
    client
        .transact_signed_conflict_rebuild("fixture.request.remove_parent", |txn| {
            Box::pin(async move { txn.delete_request_for_native_fixture(doc_id).await })
        })
        .await
        .expect("client deletes its parent request");
}

pub(super) async fn completion_obligations(
    access: &crate::ConfigAccess,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> (Vec<String>, Vec<serde_json::Value>) {
    let session_id = crate::store::graphql::escape_graphql_string(session_id);
    let response = access
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ _docID }} AgentRequest(filter: {{ session_id: {{ _eq: "{session_id}" }}, execution_origin: {{ _eq: "scheduled" }} }}) {{ input }} }}"#
        ))
        .await
        .expect("read recovery completion obligations");
    let data = &response["data"];
    let mut notifications = Vec::new();
    for row in data["AgentMessage"].as_array().unwrap() {
        let (_, message) = crate::session::load_canonical_message(
            access,
            row["_docID"].as_str().unwrap(),
            node_did,
            requester_did,
        )
        .await
        .expect("reconstruct canonical recovery notification");
        if let crate::inference::llm::message::Message::User { content } = message {
            for item in content {
                if let crate::inference::llm::message::UserContent::Text(text) = item {
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
    crate::tools::background::completion::settle_running_session_message_rows_with_access(
        &admission.node,
        &admission.tool.access().unwrap(),
        &admission.node_did,
    )
    .await
    .unwrap()
}
