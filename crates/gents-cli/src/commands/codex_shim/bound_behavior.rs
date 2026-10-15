pub(super) use crate::commands::inference_binding::load_bound_context_window;
use crate::commands::inference_binding::load_bound_profile;
use anyhow::Result;
use gents::defra_node::EmbeddedNode;

pub(super) const MODEL_SELECTION_SEPARATOR: &str = "::";

#[cfg(test)]
use crate::commands::inference_binding::explicit_agent_override as explicit_override;

pub(super) use crate::commands::inference_binding::resolve_bound_agent_id;

pub(super) async fn load_bound_inference_profile_id(
    node: &EmbeddedNode,
    node_did: &str,
    agent_id: &str,
) -> Result<String> {
    Ok(load_bound_profile(node, node_did, agent_id)
        .await?
        .profile_id)
}

pub(super) async fn load_bound_model_selection_id(
    node: &EmbeddedNode,
    node_did: &str,
    agent_id: &str,
) -> Result<String> {
    let profile = load_bound_profile(node, node_did, agent_id).await?;
    Ok(model_selection_id(&profile.backend_id, &profile.model_name))
}

pub(super) async fn load_bound_model_selection_id_for_state(
    node: &EmbeddedNode,
    node_did: &str,
    agent_id: &str,
) -> Result<String> {
    load_bound_model_selection_id(node, node_did, agent_id).await
}

pub(super) fn model_selection_id(backend_id: &str, model_name: &str) -> String {
    format!("{backend_id}{MODEL_SELECTION_SEPARATOR}{model_name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn same_labels_resolve_only_the_selected_node_binding() {
        use gents::config_client::{
            apply_desired_state_plan, ConfigAccess, DesiredStateApplyDocument,
            DesiredStateApplyPlan,
        };
        use gents::{Collection, NodeIdentity};
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let node = EmbeddedNode::builder()
            .data_path(dir.path().join("db"))
            .build()
            .await
            .unwrap();
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let first =
            gents::KeyIdentity::load_or_create(&dir.path().join("first.key"), None).unwrap();
        let second =
            gents::KeyIdentity::load_or_create(&dir.path().join("second.key"), None).unwrap();
        for (identity, model, window) in [
            (&first, "first-model", 32000),
            (&second, "second-model", 64000),
        ] {
            let owner = identity.did();
            gents::ensure_node(&node, owner).await.unwrap();
            let plan = DesiredStateApplyPlan::new([
                (Collection::Agent, json!({"node_did":owner,"agent_id":"same","inference_profile_id":"same-profile"})),
                (Collection::InferenceProfile,json!({"node_did":owner,"profile_id":"same-profile","backend_id":"same-backend","model_name":model,"context_window":window})),
                (Collection::InferenceBackend,json!({"node_did":owner,"backend_id":"same-backend","name":"Test","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:1/v1","auth":{"kind":"unauthenticated"}})),
            ].into_iter().map(|(collection,value)|DesiredStateApplyDocument {collection,add:value.clone(),update:value}).collect()).unwrap();
            ConfigAccess::transact_local(&node, None, "codex.binding.fixture", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await })
            })
            .await
            .unwrap();
        }
        assert_eq!(
            load_bound_model_selection_id(&node, first.did(), "same")
                .await
                .unwrap(),
            "same-backend::first-model"
        );
        assert_eq!(
            load_bound_model_selection_id(&node, second.did(), "same")
                .await
                .unwrap(),
            "same-backend::second-model"
        );
        assert_eq!(
            load_bound_context_window(&node, first.did(), "same")
                .await
                .unwrap(),
            32000
        );
        assert_eq!(
            load_bound_context_window(&node, second.did(), "same")
                .await
                .unwrap(),
            64000
        );
        assert!(load_bound_model_selection_id(&node, "did:missing", "same")
            .await
            .is_err());
        assert!(load_bound_model_selection_id(&node, first.did(), "missing")
            .await
            .is_err());
        node.shutdown().await;
    }

    #[test]
    fn explicit_override_uses_value() {
        assert_eq!(
            explicit_override(Some("custom-agent")),
            Some("custom-agent".to_string())
        );
    }

    #[test]
    fn explicit_override_trims_whitespace() {
        assert_eq!(
            explicit_override(Some("  spaced  ")),
            Some("spaced".to_string())
        );
    }

    #[test]
    fn explicit_override_treats_empty_as_unset() {
        assert_eq!(explicit_override(Some("")), None);
        assert_eq!(explicit_override(Some("   ")), None);
        assert_eq!(explicit_override(None), None);
    }
}
