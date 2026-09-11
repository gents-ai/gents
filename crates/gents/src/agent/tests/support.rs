use std::sync::Arc;

use crate::llm::tool::Tool;
use crate::llm::tool::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::super::*;
use crate::identity::KeyIdentity;

pub(super) async fn test_node() -> Arc<EmbeddedNode> {
    let identity = test_identity("agent-test-node");
    Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    )
}

pub(super) fn test_identity(name: &str) -> KeyIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    KeyIdentity::load_or_create(path, None).unwrap()
}

#[derive(Debug, Deserialize)]
pub(super) struct EchoArgs {
    pub(super) value: String,
}

#[derive(Debug, thiserror::Error)]
#[error("echo tool error")]
pub(super) struct EchoToolError;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct EchoTool;

impl Tool for EchoTool {
    const NAME: &'static str = "echo_value";

    type Error = EchoToolError;
    type Args = EchoArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Echo a value back".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "value": {
                        "type": "string",
                        "description": "Value to echo"
                    }
                },
                "required": ["value"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        Ok(args.value)
    }
}

pub(super) async fn insert_backend(
    node: &EmbeddedNode,
    agent_did: &str,
    backend_id: &str,
    endpoint: &str,
) {
    crate::ensure_agent_principal(node, agent_did)
        .await
        .unwrap();
    let value = serde_json::json!({
        "agent_did": agent_did,
        "backend_id": backend_id,
        "name": "Balanced Backend",
        "provider_kind": "OpenAiCompatible",
        "endpoint": endpoint,
        "auth": {"kind": "unauthenticated"},
        "max_concurrent": 2
    });
    let plan = crate::config_client::DesiredStateApplyPlan::new(vec![
        crate::config_client::DesiredStateApplyDocument {
            collection: crate::Collection::InferenceBackend,
            add: value.clone(),
            update: value,
        },
    ])
    .unwrap();
    crate::config_client::ConfigAccess::transact_local(node, None, "test.insert_backend", |txn| {
        let plan = &plan;
        Box::pin(async move { crate::config_client::apply_desired_state_plan(txn, plan).await })
    })
    .await
    .unwrap();
    crate::backend_registry::set_backend_probe_status(node, agent_did, backend_id, "healthy")
        .await
        .unwrap();
}
