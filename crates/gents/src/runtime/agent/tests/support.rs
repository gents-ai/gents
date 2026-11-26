use std::sync::Arc;

use super::super::*;
use gents_store::identity::KeyIdentity;

pub(super) async fn test_node(
    name: &str,
) -> (
    gents_store::test_support::TestStore,
    KeyIdentity,
    gents_store::config_client::ConfigAccess,
) {
    let db = crate::support::trial_store(name).await;
    gents_documents::test_support::SignedHome::enroll(db, name)
        .await
        .into_parts()
}

pub(super) async fn install_agent(
    node: &Arc<EmbeddedNode>,
    identity: &dyn NodeIdentity,
    owner: &gents_store::config_client::ConfigAccess,
    name: &str,
) -> String {
    let owner_did = owner.actor_did().expect("signed fixture owner");
    let binding = gents_store::identity::provision_agent_identity_authorized(
        node,
        identity,
        &identity::Did::new(owner_did).unwrap(),
    )
    .await
    .unwrap();
    gents_inference::test_support::install_test_agent_binding_with_access(
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
    owner: &gents_store::config_client::ConfigAccess,
) -> String {
    let did = install_agent(node, identity, owner, "Default fixture agent").await;
    gents_documents::test_support::set_test_default_agent_as(owner, identity.did(), &did).await;
    did
}

pub(super) async fn insert_backend(
    node_did: &str,
    owner: &gents_store::config_client::ConfigAccess,
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
    let plan = gents_documents::DesiredStateApplyPlan::new(vec![
        gents_documents::DesiredStateApplyDocument {
            collection: gents_store::collection::Collection::InferenceBackend,
            add: value.clone(),
            update: value,
        },
    ])
    .unwrap();
    owner
        .transact_write("test.insert_backend", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
    gents_inference::backends::registry::set_backend_probe_status_as(
        owner, node_did, backend_id, "healthy",
    )
    .await
    .unwrap();
}
