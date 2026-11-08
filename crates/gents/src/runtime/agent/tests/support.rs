use std::sync::Arc;

use crate::inference::llm::tool::Tool;
use crate::inference::llm::tool::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::super::*;
use crate::store::identity::KeyIdentity;

pub(super) async fn test_node(
    name: &str,
) -> (crate::support::TestDb, KeyIdentity, crate::ConfigAccess) {
    let db = crate::support::test_db(name).await;
    crate::test_support::signed_test_node_without_agent_from_db(db, name).await
}

pub(super) async fn install_agent(
    node: &Arc<EmbeddedNode>,
    identity: &dyn NodeIdentity,
    owner: &crate::ConfigAccess,
    name: &str,
) -> String {
    let owner_did = owner.actor_did().expect("signed fixture owner");
    let binding = crate::store::identity::provision_agent_identity_authorized(
        node,
        identity,
        &identity::Did::new(owner_did).unwrap(),
    )
    .await
    .unwrap();
    crate::test_support::install_test_agent_binding_with_access(
        identity.did(),
        name,
        &binding,
        owner,
    )
    .await;
    binding.agent_did
}

pub(super) async fn install_default_agent(
    node: &Arc<EmbeddedNode>,
    identity: &dyn NodeIdentity,
    owner: &crate::ConfigAccess,
) -> String {
    let did = install_agent(node, identity, owner, "Default fixture agent").await;
    crate::test_support::set_test_default_agent_as(owner, identity.did(), &did).await;
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
    node_did: &str,
    owner: &crate::ConfigAccess,
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
    owner
        .transact_write("test.insert_backend", |txn| {
            let plan = &plan;
            Box::pin(async move { crate::documents::apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap();
    crate::inference::backends::registry::set_backend_probe_status_as(
        owner, node_did, backend_id, "healthy",
    )
    .await
    .unwrap();
}
