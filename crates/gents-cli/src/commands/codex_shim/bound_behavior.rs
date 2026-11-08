pub(super) use crate::commands::inference_binding::load_bound_context_window;
use crate::commands::inference_binding::load_bound_profile;
use anyhow::Result;
use gents::defra_node::Node as EmbeddedNode;
use std::sync::Arc;

pub(super) const MODEL_SELECTION_SEPARATOR: &str = "::";

pub(super) use crate::commands::inference_binding::resolve_bound_agent_did;

pub(super) async fn load_bound_inference_profile_id(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_did: &str,
) -> Result<String> {
    Ok(load_bound_profile(node, node_did, agent_did)
        .await?
        .profile_id)
}

pub(super) use crate::commands::inference_binding::load_bound_agent_signer;

pub(super) async fn load_bound_model_selection_id(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_did: &str,
) -> Result<String> {
    let profile = load_bound_profile(node, node_did, agent_did).await?;
    Ok(model_selection_id(&profile.backend_id, &profile.model_name))
}

pub(super) async fn load_bound_model_selection_id_for_state(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_did: &str,
) -> Result<String> {
    load_bound_model_selection_id(node, node_did, agent_did).await
}

pub(super) fn model_selection_id(backend_id: &str, model_name: &str) -> String {
    format!("{backend_id}{MODEL_SELECTION_SEPARATOR}{model_name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn same_labels_resolve_only_the_selected_node_binding() {
        use gents::documents::apply_desired_state_plan;
        use gents::store::config_client::{DesiredStateApplyDocument, DesiredStateApplyPlan};
        use gents::{Collection, NodeIdentity};
        use serde_json::json;
        // Two Nodes share the profile and backend labels; the second is a peer
        // whose rows the User also published on this node.
        let client = crate::shared::test_support::ClientHome::new().await;
        let node = client.node().clone();
        let dir = tempfile::tempdir().unwrap();
        let peer = gents::KeyIdentity::load_or_create(dir.path().join("peer.key"), None).unwrap();
        let mut agents = Vec::new();
        for (runtime, model, window) in [
            (client.home.identity.as_ref(), "first-model", 32000),
            (&peer as &dyn NodeIdentity, "second-model", 64000),
        ] {
            let owner = runtime.did();
            let binding = gents::store::identity::provision_agent_identity(runtime).unwrap();
            let mut documents = vec![
                (Collection::InferenceBackend,json!({"node_did":owner,"backend_id":"same-backend","name":"Test","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:1/v1","auth":{"kind":"unauthenticated"}})),
                (Collection::InferenceProfile,json!({"node_did":owner,"profile_id":"same-profile","backend_id":"same-backend","model_name":model,"context_window":window})),
                (Collection::Agent, json!({"node_did":owner,"agent_did":binding.agent_did,"creation_nonce":binding.creation_nonce,"inference_profile_id":"same-profile"})),
            ];
            if owner != client.node_did() {
                documents.insert(0, (Collection::Node, json!({"node_did":owner})));
            }
            let plan = DesiredStateApplyPlan::new(documents.into_iter().map(|(collection,value)|DesiredStateApplyDocument {collection,add:value.clone(),update:value}).collect()).unwrap();
            client
                .access
                .transact_write("codex.binding.fixture", |txn| {
                    let plan = &plan;
                    Box::pin(async move { apply_desired_state_plan(txn, plan).await })
                })
                .await
                .unwrap();
            agents.push((owner.to_owned(), binding.agent_did));
        }
        let [(first, first_agent), (second, second_agent)] = <[_; 2]>::try_from(agents).unwrap();
        assert_eq!(
            load_bound_model_selection_id(&node, &first, &first_agent)
                .await
                .unwrap(),
            "same-backend::first-model"
        );
        // The peer's rows answer to the peer's actors; this runtime binds only
        // its own Node and never resolves the shared labels across owners.
        assert!(load_bound_model_selection_id(&node, &second, &second_agent)
            .await
            .is_err());
        assert!(load_bound_model_selection_id(&node, &first, &second_agent)
            .await
            .is_err());
        assert_eq!(
            load_bound_context_window(&node, &first, &first_agent)
                .await
                .unwrap(),
            32000
        );
        assert!(load_bound_model_selection_id(&node, "did:missing", &first_agent)
            .await
            .is_err());
        assert!(load_bound_model_selection_id(&node, &first, "missing")
            .await
            .is_err());
    }
}
