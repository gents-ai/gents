use crate::client_runtime::ClientRuntime;
use anyhow::Result;

use crate::types::DesktopBackgroundToolRequest;

pub async fn background_tool(
    core: &ClientRuntime,
    request: DesktopBackgroundToolRequest,
) -> Result<bool> {
    gents_toolcalls::background::status::request_foreground_backgrounding(
        &core.operator_access(&request.node_did)?,
        &request.node_did,
        &request.session_id,
        &request.request_id,
        &request.tool_call_id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn background_command_uses_the_runtime_when_the_replica_has_no_tool_row() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v0/graphql"))
            .and(body_string_contains("command-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "AgentToolCall": [{
                    "_docID": "tool-doc", "request_doc_id": "request-doc",
                    "tool_name": "bash_unrestricted", "lifecycle_state": "running",
                    "await_mode": "foreground", "background_requested_at": "2026-10-22T00:00:00Z"
                }] }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (core, temp) = crate::tests::support::boot_core().await;
        let runtime = crate::client_test_support::runtime_key("background-runtime").await;
        let node = runtime.did.as_str();
        crate::client_test_support::runtime_metadata(&server, &runtime).await;
        core.connect_runtime(gents_protocol::runtime_connection::RuntimeConnection {
            node_did: (node).to_string(),
            label: ("Served runtime").to_string(),
            graphql: format!("{}/api/v0/graphql", server.uri()),
            local_node_home: Some((temp.path().join("runtime").to_str().unwrap()).to_string()),
            access_request: None,
            access_state: None,
        })
        .await
        .unwrap();
        assert!(background_tool(
            core.as_ref(),
            DesktopBackgroundToolRequest {
                node_did: node.into(),
                session_id: "session-1".into(),
                request_id: "request-1".into(),
                tool_call_id: "command-1".into(),
            },
        )
        .await
        .unwrap());
        core.shutdown().await.unwrap();
    }
}
