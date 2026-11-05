use std::sync::Arc;

use crate::inference::llm::tool::Tool;
use crate::inference::llm::tool::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::super::*;
use crate::store::identity::KeyIdentity;

pub(super) async fn test_node(
    name: &str,
) -> (Arc<EmbeddedNode>, Arc<KeyIdentity>, Arc<KeyIdentity>) {
    let identity = Arc::new(test_identity(name));
    let owner = Arc::new(test_identity("agent-test-owner"));
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .with_node_acp_enabled()
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
    crate::test_support::bootstrap_test_node(&node, identity.as_ref(), owner.as_ref()).await;
    (node, identity, owner)
}

pub(super) async fn install_agent(
    node: &Arc<EmbeddedNode>,
    identity: &KeyIdentity,
    owner: &KeyIdentity,
    name: &str,
) -> String {
    crate::test_support::install_test_agent_as(node, identity, owner, name)
        .await
        .agent_did
}

pub(super) async fn install_default_agent(
    node: &Arc<EmbeddedNode>,
    identity: &KeyIdentity,
    owner: &KeyIdentity,
) -> String {
    let did = install_agent(node, identity, owner, "Default fixture agent").await;
    let access = crate::store::config_client::ConfigAccess::local_as(
        node.clone(),
        owner.did().parse().unwrap(),
    );
    crate::test_support::set_test_default_agent_as(&access, identity.did(), &did).await;
    did
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
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    owner: &KeyIdentity,
    backend_id: &str,
    endpoint: &str,
) {
    let value = serde_json::json!({
        "node_did": node_did,
        "backend_id": backend_id,
        "name": "Balanced Backend",
        "provider_kind": "OpenAiCompatible",
        "endpoint": endpoint,
        "auth": {"kind": "unauthenticated"},
        "max_concurrent": 2
    });
    let plan = crate::store::config_client::DesiredStateApplyPlan::new(vec![
        crate::store::config_client::DesiredStateApplyDocument {
            collection: crate::Collection::InferenceBackend,
            add: value.clone(),
            update: value,
        },
    ])
    .unwrap();
    crate::store::config_client::ConfigAccess::local_as(node.clone(), owner.did().parse().unwrap())
        .transact_write("test.insert_backend", |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::store::config_client::apply_desired_state_plan(txn, plan).await
            })
        })
        .await
        .unwrap();
    crate::inference::backends::registry::set_backend_probe_status(
        node, node_did, backend_id, "healthy",
    )
    .await
    .unwrap();
}
