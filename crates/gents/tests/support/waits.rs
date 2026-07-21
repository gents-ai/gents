use std::time::Duration;

use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use serde_json::Value;

pub async fn wait_for_runtime_process_state(
    node: &EmbeddedNode,
    agent_did: &str,
    expected_process_state: &str,
) {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let query = format!(
            r#"{{
                AgentRuntime(filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }}, limit: 1) {{
                    process_state
                }}
            }}"#
        );
        let response = node.execute(&query).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let process_state = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRuntime"))
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("process_state"))
            .and_then(Value::as_str);
        if process_state == Some(expected_process_state) {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
