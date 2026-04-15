use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use defra_node::EmbeddedNode;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::Deserialize;

use crate::graphql::escape_graphql_string;
use crate::lifecycle::DEFAULT_REQUEST_MAX_RETRIES;

use super::shared::ToolError;
use super::DELEGATE_TOOL_NAME;

#[derive(Debug, Deserialize)]
pub(super) struct DelegateToAgentArgs {
    pub(super) target_did: String,
    pub(super) content: String,
    pub(super) wait: bool,
}

#[derive(Clone)]
pub(super) struct DelegateToAgentTool {
    node: Arc<EmbeddedNode>,
    allowed_target_dids: Arc<HashSet<String>>,
}

impl DelegateToAgentTool {
    pub(super) fn new(node: Arc<EmbeddedNode>, allowed_target_dids: Vec<String>) -> Self {
        Self {
            node,
            allowed_target_dids: Arc::new(
                allowed_target_dids
                    .into_iter()
                    .map(|did| did.trim().to_string())
                    .filter(|did| !did.is_empty())
                    .collect(),
            ),
        }
    }
}

impl Tool for DelegateToAgentTool {
    const NAME: &'static str = DELEGATE_TOOL_NAME;

    type Error = ToolError;
    type Args = DelegateToAgentArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Delegate a request to another defra-agent DID via DefraDB.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "target_did": { "type": "string" },
                    "content": { "type": "string" },
                    "wait": { "type": "boolean" }
                },
                "required": ["target_did", "content", "wait"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if !self.allowed_target_dids.contains(args.target_did.trim()) {
            return Err(anyhow!(
                "delegate_to_agent target DID {} is not in the allowed delegation set",
                args.target_did
            )
            .into());
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339();
        let escaped_target = escape_graphql_string(&args.target_did);
        let escaped_content = escape_graphql_string(&args.content);
        let escaped_request_id = escape_graphql_string(&request_id);
        let escaped_session_id = escape_graphql_string(&session_id);
        let escaped_created_at = escape_graphql_string(&created_at);

        let mutation = format!(
            r#"mutation {{
                create_AgentRequest(
                    input: {{
                        request_id: "{escaped_request_id}",
                        agent_did: "{escaped_target}",
                        session_id: "{escaped_session_id}",
                        retry_parent_request: "",
                        retry_root_request: "{escaped_request_id}",
                        superseded_by_request: "",
                        content: "{escaped_content}",
                        status: "pending",
                        lifecycle_state: "pending",
                        backend_id: "",
                        execution_origin: "interactive",
                        created_at: "{escaped_created_at}",
                        retry_count: 0,
                        max_retries: {max_retries}
                    }}
                ) {{ _docID }}
            }}"#,
            max_retries = DEFAULT_REQUEST_MAX_RETRIES,
        );

        let resp = self.node.execute(&mutation).await;
        if resp.has_errors() {
            return Err(anyhow!(
                "delegate_to_agent failed to create request: {:?}",
                resp.errors
            )
            .into());
        }

        let lineage_mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                    input: {{
                        retry_parent_request: "",
                        retry_root_request: "{escaped_request_id}",
                        superseded_by_request: ""
                    }}
                ) {{ _docID }}
            }}"#
        );
        let lineage_resp = self.node.execute(&lineage_mutation).await;
        if lineage_resp.has_errors() {
            return Err(anyhow!(
                "delegate_to_agent failed to persist request lineage: {:?}",
                lineage_resp.errors
            )
            .into());
        }

        if !args.wait {
            return Ok(request_id);
        }

        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(super::DELEGATE_WAIT_TIMEOUT_SECS);
        loop {
            let query = format!(
                r#"{{
                    AgentResponse(
                        filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                        order: {{ created_at: DESC }},
                        limit: 1
                    ) {{
                        content
                        status
                    }}
                }}"#
            );

            let resp = self.node.execute(&query).await;
            if resp.has_errors() {
                return Err(anyhow!(
                    "delegate_to_agent failed to query response: {:?}",
                    resp.errors
                )
                .into());
            }

            let rows: Vec<AgentResponseRow> = resp
                .data
                .as_ref()
                .and_then(|data| data.get("AgentResponse"))
                .and_then(|value| serde_json::from_value(value.clone()).ok())
                .unwrap_or_default();

            if let Some(row) = rows.into_iter().next() {
                match row.status.as_str() {
                    "complete" => return Ok(row.content),
                    "error" => {
                        return Err(
                            anyhow!("delegated agent returned error: {}", row.content).into()
                        )
                    }
                    _ => {}
                }
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(
                    anyhow!("timed out waiting for delegated response {request_id}").into(),
                );
            }

            tokio::time::sleep(Duration::from_millis(super::DELEGATE_POLL_INTERVAL_MS)).await;
        }
    }
}

#[derive(Debug, Deserialize)]
struct AgentResponseRow {
    #[serde(default)]
    content: String,
    #[serde(default)]
    status: String,
}
