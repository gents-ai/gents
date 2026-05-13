mod support;

use defra_agent::graphql::escape_graphql_string;
use defra_agent::interrupt_request;

use support::snapshots::fetch_request_snapshot;
use support::{
    create_request, first_row, set_interrupt_requested_at, test_db, DocIdRow, AGENT_DID, AGENT_NAME,
};

async fn create_pending_request_with_metadata(
    node: &defra_agent::defra_node::EmbeddedNode,
    request_id: &str,
    session_id: &str,
    metadata: &str,
    execution_origin: &str,
) -> String {
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_metadata = escape_graphql_string(metadata);
    let escaped_execution_origin = escape_graphql_string(execution_origin);
    let created_at = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                agent_did: "{AGENT_DID}",
                behavior_id: "{AGENT_NAME}",
                session_id: "{escaped_session_id}",
                retry_parent_request: "",
                retry_root_request: "{escaped_request_id}",
                superseded_by_request: "",
                content: "queued",
                metadata: "{escaped_metadata}",
                status: "pending",
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "{escaped_execution_origin}",
                failure_reason: "",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: {max_retries}
            }}) {{ _docID }}
        }}"#,
        max_retries = defra_agent::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create pending request with metadata failed: {:?}",
        response.errors
    );

    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}) {{
                _docID
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    first_row::<DocIdRow>(&response, "AgentRequest").doc_id
}

fn automated_wakeup_metadata(session_id: &str, queued_after_request_id: &str) -> String {
    serde_json::json!({
        "queue": {
            "source": "subagent_completion",
            "policy": "coalesce",
            "key": format!("subagent_completion:{session_id}"),
            "queued_after_request_id": queued_after_request_id,
        }
    })
    .to_string()
}

fn user_queue_metadata() -> String {
    serde_json::json!({
        "queue": {
            "source": "user",
            "policy": "append",
            "key": null,
            "queued_after_request_id": null,
        }
    })
    .to_string()
}

#[tokio::test]
async fn interrupt_request_drains_automated_wakeups_but_preserves_user_queue() {
    let db = test_db("queue-drain-on-interrupt").await;
    let session_id = "session-queue-drain-on-interrupt";
    let created_at = chrono::Utc::now().to_rfc3339();
    let parent_doc_id = create_request(
        &db.node,
        "req-queue-drain-parent",
        session_id,
        "pending",
        &created_at,
    )
    .await;
    let auto_doc_id = create_pending_request_with_metadata(
        &db.node,
        "req-queue-drain-auto",
        session_id,
        &automated_wakeup_metadata(session_id, "req-queue-drain-parent"),
        "scheduled",
    )
    .await;
    let user_doc_id = create_pending_request_with_metadata(
        &db.node,
        "req-queue-drain-user",
        session_id,
        &user_queue_metadata(),
        "scheduled",
    )
    .await;
    let interactive_auto_doc_id = create_pending_request_with_metadata(
        &db.node,
        "req-queue-drain-interactive-auto",
        session_id,
        &automated_wakeup_metadata(session_id, "req-queue-drain-parent"),
        "interactive",
    )
    .await;
    let plain_doc_id = create_request(
        &db.node,
        "req-queue-drain-plain",
        session_id,
        "pending",
        &created_at,
    )
    .await;

    interrupt_request(&db.node, "req-queue-drain-parent")
        .await
        .unwrap();

    let parent = fetch_request_snapshot(&db.node, &parent_doc_id).await;
    assert_eq!(parent.lifecycle_state, "pending");

    let auto = fetch_request_snapshot(&db.node, &auto_doc_id).await;
    assert_eq!(auto.status, "interrupted");
    assert_eq!(auto.lifecycle_state, "interrupted");
    assert_eq!(
        auto.failure_reason,
        "automated wake-up drained because active request was interrupted"
    );

    let user = fetch_request_snapshot(&db.node, &user_doc_id).await;
    assert_eq!(user.status, "pending");
    assert_eq!(user.lifecycle_state, "pending");

    let interactive_auto = fetch_request_snapshot(&db.node, &interactive_auto_doc_id).await;
    assert_eq!(interactive_auto.status, "pending");
    assert_eq!(interactive_auto.lifecycle_state, "pending");

    let plain = fetch_request_snapshot(&db.node, &plain_doc_id).await;
    assert_eq!(plain.status, "pending");
    assert_eq!(plain.lifecycle_state, "pending");
}

#[tokio::test]
async fn already_interrupted_request_still_drains_automated_wakeups() {
    let db = test_db("queue-drain-on-already-interrupted").await;
    let session_id = "session-queue-drain-on-already-interrupted";
    let created_at = chrono::Utc::now().to_rfc3339();
    let parent_doc_id = create_request(
        &db.node,
        "req-queue-drain-latched-parent",
        session_id,
        "pending",
        &created_at,
    )
    .await;
    set_interrupt_requested_at(&db.node, &parent_doc_id, "2026-05-12T00:00:00Z").await;
    let auto_doc_id = create_pending_request_with_metadata(
        &db.node,
        "req-queue-drain-latched-auto",
        session_id,
        &automated_wakeup_metadata(session_id, "req-queue-drain-latched-parent"),
        "scheduled",
    )
    .await;

    interrupt_request(&db.node, "req-queue-drain-latched-parent")
        .await
        .unwrap();

    let auto = fetch_request_snapshot(&db.node, &auto_doc_id).await;
    assert_eq!(auto.status, "interrupted");
    assert_eq!(auto.lifecycle_state, "interrupted");
}
