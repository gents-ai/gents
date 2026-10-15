//! Physical request-scope checks that require the private accepted-call seam.

use super::AwaitMode;
use crate::identity::NodeIdentity;
use crate::tool_call_lifecycle::admission_fixture::{
    claimed_signed_request, complete_child, materialize_session_message,
    publish_accepted_on_claimed_request,
};
use defra_node::EmbeddedNode;
use std::sync::Arc;

async fn exec(node: &EmbeddedNode, statement: &str) {
    let response = node.execute(statement).await;
    assert!(
        !response.has_errors(),
        "GraphQL errors: {:?}",
        response.errors
    );
}

async fn scope_row(node: &EmbeddedNode, collection: &str, doc_id: &str) -> serde_json::Value {
    let fields = match collection {
        "AgentRequest" => "_docID request_id node_did lifecycle_state failure_reason deadline execution_generation execution_lease_expires_at",
        "AgentToolCall" => "_docID request_doc_id node_did tool_call_id lifecycle_state cancel_cause tool_failure_class",
        other => panic!("unsupported scope collection {other}"),
    };
    let doc_id = crate::graphql::escape_graphql_string(doc_id);
    let response = node.execute(&format!(
        r#"{{ {collection}(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{ {fields} }} }}"#,
    )).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    response.data.unwrap()[collection][0].clone()
}

async fn queue_snapshot(
    node: &EmbeddedNode,
    session_id: &str,
    queue_key: Option<&str>,
) -> (Vec<String>, usize) {
    let session_id = crate::graphql::escape_graphql_string(session_id);
    let response = node.execute(&format!(r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ request_id lifecycle_state input }} }}"#)).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let rows = response.data.unwrap()["AgentRequest"]
        .as_array()
        .unwrap()
        .clone();
    let pending = rows
        .iter()
        .filter(|row| row["lifecycle_state"] == "pending")
        .filter_map(|row| row["request_id"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let coalesced = rows
        .iter()
        .filter(|row| {
            row["lifecycle_state"] == "pending"
                && row["input"]["queue"]["source"] == "background_completion"
                && row["input"]["queue"]["policy"] == "coalesce"
                && queue_key.is_some_and(|key| row["input"]["queue"]["key"] == key)
        })
        .count();
    (pending, coalesced)
}

#[tokio::test]
async fn generated_background_completion_queue_case_uses_accepted_session_messages() {
    let case = crate::lean_vocab_test::lean_queue_deadline_case(
        "background_completion_notification_creates_no_agent_request",
    );
    assert_eq!(case.group, "completion_delivery");
    assert_eq!(case.action, "appendNotification");
    assert!(case.legal);
    assert_eq!(case.pre_active_request_id, case.post_active_request_id);
    let path = std::env::temp_dir().join(format!("queue-scope-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    let identity = crate::KeyIdentity::load_or_create(path.join("queue-agent.key"), None).unwrap();
    let node_did = identity.did();
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(&path)
            .with_node_identity_did(node_did)
            .build()
            .await
            .unwrap(),
    );
    crate::schema::ensure_runtime_schemas(&node).await.unwrap();
    crate::test_support::install_test_agent(node.as_ref(), node_did, "general").await;
    let parent_id = format!("queue-deadline-coalesce-parent-{}", uuid::Uuid::new_v4());
    let session_id = case.session_id.to_string();
    let mut parent = claimed_signed_request(&node, &parent_id, &session_id, &identity, None).await;
    let parent_doc_id = parent.request().doc_id.clone();
    let queue_key = case.queue_key.as_deref();
    let pre = queue_snapshot(node.as_ref(), &session_id, queue_key).await;
    assert_eq!(
        pre.0.len(),
        case.pre_pending_request_ids.len(),
        "{} pre pending queue drifted",
        case.name
    );
    assert_eq!(pre.1, 0, "{} pre coalesced count drifted", case.name);

    let mut children = Vec::new();
    for turn in 0..2 {
        let tool_id = format!("queue-deadline-coalesce-{}-{turn}", uuid::Uuid::new_v4());
        let mut row = publish_accepted_on_claimed_request(
            node.clone(),
            &mut parent,
            node_did,
            turn,
            crate::toolset::AGENT_NEW_TOOL_NAME,
            &tool_id,
            serde_json::json!({"agent": "general", "prompt": format!("prompt for {tool_id}")}),
            AwaitMode::Background,
            false,
        )
        .await
        .unwrap();
        let receipt = materialize_session_message(
            &node,
            &parent,
            &mut row,
            node_did,
            &format!("prompt for {tool_id}"),
        )
        .await
        .unwrap();
        let tool_doc_id = row.doc_id().unwrap().to_owned();
        assert_eq!(row.request_doc_id.as_deref(), Some(parent_doc_id.as_str()));
        let persisted = scope_row(node.as_ref(), "AgentToolCall", &tool_doc_id).await;
        assert_eq!(persisted["request_doc_id"], parent_doc_id);
        assert_eq!(persisted["node_did"], node_did);
        assert_eq!(persisted["lifecycle_state"], "running");
        children.push(receipt.request_id);
    }
    // Both provider turns must be accepted while the parent still owns its
    // execution. The settlement premise observes it as terminal afterwards.
    let escaped_parent_doc = crate::graphql::escape_graphql_string(&parent_doc_id);
    exec(node.as_ref(), &format!(r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_parent_doc}" }} }}, input: {{ lifecycle_state: "completed" }}) {{ _docID }} }}"#)).await;
    for (index, caused_request_id) in children.iter().enumerate() {
        complete_child(
            &node,
            caused_request_id,
            node_did,
            &format!("child {} complete", index + 1),
        )
        .await;
    }
    let settled =
        crate::background_completion::settle_running_session_message_rows(&node, node_did)
            .await
            .unwrap();
    assert_eq!(settled, 2);
    let post = queue_snapshot(node.as_ref(), &session_id, queue_key).await;
    assert!(
        case.post_pending_request_ids.is_empty(),
        "{} generated notification must add no modeled request",
        case.name
    );
    assert_eq!(
        post.0.len(),
        1,
        "two caused-request completions must enqueue one wake request"
    );
    assert_eq!(
        post.1, case.post_coalesced_pending_count,
        "{} coalesced count drifted",
        case.name
    );
    node.shutdown().await;
    std::fs::remove_dir_all(path).unwrap();
}
