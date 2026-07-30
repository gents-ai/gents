use gents::tool_call_lifecycle::ToolCallLifecycle;
use serde::Deserialize;

use crate::support::{create_request, first_row, test_db, AGENT_DID};

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    lifecycle_state: Option<String>,
    cancel_cause: Option<String>,
    result: String,
}

#[derive(Debug, Deserialize)]
struct MessageRow {
    content: String,
}

#[derive(Debug, Deserialize)]
struct WakeRequestRow {
    metadata: Option<String>,
}

async fn load_tool_call(node: &gents::defra_node::EmbeddedNode, tool_call_id: &str) -> ToolCallRow {
    let tool_call_id = gents::graphql::escape_graphql_string(tool_call_id);
    let query = format!(
        r#"{{
            AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{tool_call_id}" }} }}, limit: 1) {{
                lifecycle_state
                cancel_cause
                result
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentToolCall")
}

async fn load_messages(
    node: &gents::defra_node::EmbeddedNode,
    session_id: &str,
) -> Vec<MessageRow> {
    let session_id = gents::graphql::escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{ content }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "load recovery messages failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

async fn load_wakes(
    node: &gents::defra_node::EmbeddedNode,
    session_id: &str,
) -> Vec<WakeRequestRow> {
    let session_id = gents::graphql::escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    execution_origin: {{ _eq: "scheduled" }}
                }}
            ) {{ metadata }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "load recovery wakes failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

#[tokio::test]
async fn recover_all_interrupts_backgrounded_running_tool_with_live_parent() {
    let db = test_db("r6-background-recovery-live-parent").await;
    create_request(
        &db.node,
        "r6-recovery-parent",
        "r6-recovery-session",
        "processing",
        "2026-05-14T00:00:00Z",
    )
    .await;

    let mut lifecycle = ToolCallLifecycle::new_background_tool(
        db.node.clone(),
        "r6-recovery-parent".to_string(),
        "r6-recovery-session".to_string(),
        "did:test:test".to_string(),
        "r6-recovery-tool".to_string(),
        1,
        "bash".to_string(),
        "{}".to_string(),
        chrono::Utc::now() + chrono::Duration::minutes(5),
    );
    lifecycle.start_running().await.unwrap();

    let report = ToolCallLifecycle::recover_all(&db.node, AGENT_DID)
        .await
        .unwrap();
    assert_eq!(report.tool_calls_recovered, 1);

    let row = load_tool_call(db.node.as_ref(), "r6-recovery-tool").await;
    assert_eq!(row.lifecycle_state.as_deref(), Some("cancelled"));
    assert_eq!(row.cancel_cause.as_deref(), Some("interrupted"));
    assert!(row.result.contains("interrupted on restart"));

    let messages = load_messages(db.node.as_ref(), "r6-recovery-session").await;
    assert_eq!(messages.len(), 1);
    assert!(messages[0].content.contains(r#"<tool-completion"#));
    assert!(messages[0].content.contains(r#"status="cancelled""#));
    assert!(messages[0]
        .content
        .contains("<reason>interrupted_on_restart</reason>"));

    let wakes = load_wakes(db.node.as_ref(), "r6-recovery-session").await;
    assert_eq!(wakes.len(), 1);
    let metadata: serde_json::Value =
        serde_json::from_str(wakes[0].metadata.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["queue"]["source"], "background_completion");
    assert_eq!(
        metadata["queue"]["key"],
        "background_completion:r6-recovery-session"
    );
}
