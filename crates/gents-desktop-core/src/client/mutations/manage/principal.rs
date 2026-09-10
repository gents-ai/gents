//! Canonical authored configuration through the shared retained-candidate owner.
use anyhow::Result;
use defra_node::EmbeddedNode;
use gents::collection::Collection;
use gents::config_client::{
    apply_desired_state_plan, ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::document_config::AgentPrincipal;

pub async fn upsert_agent_principal(node: &EmbeddedNode, document: &AgentPrincipal) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::AgentPrincipal,
        add: value.clone(),
        update: value,
    }])?;
    ConfigAccess::transact_local(node, None, "desktop.agent_principal.save", |txn| {
        let plan = &plan;
        Box::pin(async move {
            apply_desired_state_plan(txn, plan).await?;
            Ok(())
        })
    })
    .await
}

/// Apply supplied canonical components under an existing principal. The required
/// PackConfig principal is scope-only: every field except agent_did must have its
/// canonical default. Principal settings use upsert_agent_principal instead.
/// Omitted component documents remain unchanged. No deletes, interpolation,
/// principal creation, or graph compilation are implied by this operation.
pub async fn apply_config_components(
    node: &EmbeddedNode,
    document: &gents::document_config::PackConfig,
) -> Result<()> {
    use anyhow::Context;
    let owner = &document.agent_principal.agent_did;
    let scope: AgentPrincipal = serde_json::from_value(serde_json::json!({"agent_did":owner}))?;
    anyhow::ensure!(
        serde_json::to_value(&document.agent_principal)? == serde_json::to_value(scope)?,
        "component apply requires scope-only agent_principal; use principal save for principal settings"
    );
    anyhow::ensure!(
        document.graph_intents.is_empty() && document.graph_capabilities.is_empty(),
        "component apply does not compile graph inputs; use the graph installer"
    );
    let plan = DesiredStateApplyPlan::from_pack_config(document)?;
    anyhow::ensure!(
        plan.documents().iter().all(|entry| entry
            .add
            .get("agent_did")
            .and_then(serde_json::Value::as_str)
            == Some(owner.as_str())),
        "component owner does not match the selected principal"
    );
    let plan = DesiredStateApplyPlan::new(
        plan.documents()
            .iter()
            .filter(|entry| entry.collection != Collection::AgentPrincipal)
            .cloned()
            .collect(),
    )?;
    ConfigAccess::transact_local(node, None, "desktop.config.components", |txn| {
        let plan = &plan;
        Box::pin(async move {
            gents::config_client::read_desired_state_record_in_txn(
                txn,
                Collection::AgentPrincipal,
                owner,
                owner,
            )
            .await?
            .context("component apply requires an existing principal")?;
            apply_desired_state_plan(txn, plan).await?;
            Ok(())
        })
    })
    .await
}

/// Patch existing scoped components atomically through the canonical patch and
/// candidate owners. Omitted fields (including hidden credentials) are retained.
/// Explicit null is a value validated by the canonical document deserializer.
/// This operation never creates or removes a document.
pub async fn patch_config_components(
    node: &EmbeddedNode,
    agent_did: &str,
    patches: &[(
        gents::config_client::patch::SelfConfigTarget,
        String,
        gents::config_client::patch::SelfConfigPatch,
    )],
) -> Result<()> {
    use anyhow::Context;
    use gents::config_client::{
        patch::{apply_patch, ensure_admissible},
        read_desired_state_record_in_txn,
    };
    anyhow::ensure!(
        !agent_did.trim().is_empty(),
        "component patch requires an owner"
    );
    let mut identities = std::collections::HashSet::new();
    for (target, id, patch) in patches {
        anyhow::ensure!(
            !id.trim().is_empty(),
            "component patch requires a document ID"
        );
        anyhow::ensure!(
            identities.insert((*target, id)),
            "duplicate component patch identity"
        );
        ensure_admissible(*target, patch)?;
    }
    ConfigAccess::transact_local(node, None, "desktop.config.components.patch", |txn| {
        Box::pin(async move {
            read_desired_state_record_in_txn(txn, Collection::AgentPrincipal, agent_did, agent_did)
                .await?
                .context("component patch requires an existing principal")?;
            let mut documents = Vec::with_capacity(patches.len());
            for (target, id, patch) in patches {
                let (_, retained) =
                    read_desired_state_record_in_txn(txn, target.collection(), agent_did, id)
                        .await?
                        .with_context(|| {
                            format!(
                                "component patch requires existing {} {}",
                                target.collection_name(),
                                id
                            )
                        })?;
                let retained = retained
                    .as_object()
                    .context("canonical config document must be an object")?;
                let value = serde_json::Value::Object(apply_patch(*target, retained, patch));
                documents.push(DesiredStateApplyDocument {
                    collection: target.collection(),
                    add: value.clone(),
                    update: value,
                });
            }
            let plan = DesiredStateApplyPlan::new(documents)?;
            apply_desired_state_plan(txn, &plan).await?;
            Ok(())
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::config_client::read_desired_state_record_in_txn;
    use serde_json::json;

    #[tokio::test]
    async fn principal_replacement_keeps_authored_metadata_and_scoped_default_binding() -> Result<()>
    {
        let node = EmbeddedNode::builder().build().await?;
        gents::ensure_runtime_schemas(&node).await?;
        let config: gents::document_config::PackConfig = serde_json::from_value(json!({
            "agent_principal":{"agent_did":"did:test:other"},
            "inference_backends":[{"agent_did":"did:test:other","backend_id":"backend","name":"Backend","provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1","auth":{"kind":"unauthenticated"}}],
            "inference_profiles":[{"agent_did":"did:test:other","profile_id":"profile","backend_id":"backend","model_name":"model"}],
            "agent_behaviors":[{"agent_did":"did:test:other","behavior_id":"default","inference_profile_id":"profile"}]
        }))?;
        let plan = DesiredStateApplyPlan::from_pack_config(&config)?;
        ConfigAccess::transact_local(&node, None, "desktop.principal.seed", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(txn, plan).await?;
                Ok(())
            })
        })
        .await?;
        let mut document: AgentPrincipal = serde_json::from_value(
            json!({"agent_did":"did:test:principal","display_name":"Agent","created_at":"2026-01-01T00:00:00Z","created_by":"did:test:creator","tags":["team"]}),
        )?;
        upsert_agent_principal(&node, &document).await?;
        document.default_behavior_id = Some("default".into());
        assert!(upsert_agent_principal(&node, &document).await.is_err());
        ConfigAccess::transact_local(&node, None, "desktop.principal.verify", |txn| {
            Box::pin(async move {
                let (_, value) = read_desired_state_record_in_txn(
                    txn,
                    Collection::AgentPrincipal,
                    "did:test:principal",
                    "did:test:principal",
                )
                .await?
                .unwrap();
                let saved: AgentPrincipal = serde_json::from_value(value)?;
                assert_eq!(saved.created_at.as_deref(), Some("2026-01-01T00:00:00Z"));
                assert_eq!(saved.created_by.as_deref(), Some("did:test:creator"));
                assert_eq!(saved.tags, vec!["team"]);
                assert!(saved.default_behavior_id.is_none());
                Ok(())
            })
        })
        .await?;
        Ok(())
    }
    #[tokio::test]
    async fn component_apply_is_partial_atomic_scoped_and_preserves_principal() -> Result<()> {
        let node = EmbeddedNode::builder().build().await?;
        gents::ensure_runtime_schemas(&node).await?;
        let owner = "did:test:components";
        let seed: gents::document_config::PackConfig = serde_json::from_value(json!({
            "agent_principal":{"agent_did":owner,"display_name":"Keep name","default_behavior_id":"behavior","created_at":"2026-01-01T00:00:00Z","created_by":"did:test:creator","tags":["keep"]},
            "inference_backends":[{"agent_did":owner,"backend_id":"backend","name":"Backend","provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1","auth":{"kind":"unauthenticated"}}],
            "inference_profiles":[{"agent_did":owner,"profile_id":"profile","backend_id":"backend","model_name":"model"}],
            "agent_behaviors":[{"agent_did":owner,"behavior_id":"behavior","context_id":"context","inference_profile_id":"profile"}],
            "contexts":[{"agent_did":owner,"context_id":"context","system_prompt":"initial"}]
        }))?;
        let plan = DesiredStateApplyPlan::from_pack_config(&seed)?;
        ConfigAccess::transact_local(&node, None, "desktop.component.seed", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(txn, plan).await?;
                Ok(())
            })
        })
        .await?;
        let input = json!({"agent_principal":{"agent_did":owner},"contexts":[{"agent_did":owner,"context_id":"context","system_prompt":"literal ${NOT_AN_ENV} {{ not_a_template }}"}]});
        let document = serde_json::from_value(input.clone())?;
        apply_config_components(&node, &document).await?;
        let mut invalid = input.clone();
        invalid["contexts"][0]["system_prompt"] = json!("must roll back");
        invalid["inference_profiles"] = json!([{"agent_did":owner,"profile_id":"bad","backend_id":"missing","model_name":"model"}]);
        assert!(
            apply_config_components(&node, &serde_json::from_value(invalid)?)
                .await
                .is_err()
        );
        let mut foreign = input.clone();
        foreign["contexts"][0]["agent_did"] = json!("did:test:foreign");
        assert!(
            apply_config_components(&node, &serde_json::from_value(foreign)?)
                .await
                .is_err()
        );
        for (key, value) in [
            ("display_name", json!("discarded")),
            ("default_behavior_id", json!("different")),
            ("created_at", json!("2026-02-01T00:00:00Z")),
            ("created_by", json!("other")),
            ("enabled", json!(false)),
            ("tags", json!(["other"])),
        ] {
            let mut changed = input.clone();
            changed["agent_principal"][key] = value;
            assert!(
                apply_config_components(&node, &serde_json::from_value(changed)?)
                    .await
                    .is_err(),
                "must reject meaningful principal setting {key}"
            );
        }
        let mut graph = input.clone();
        graph["graph_capabilities"] =
            json!([{"agent_did":owner,"capability_id":"cap","revision":"r","task_id":"task"}]);
        assert!(
            apply_config_components(&node, &serde_json::from_value(graph)?)
                .await
                .is_err()
        );
        let absent =
            serde_json::from_value(json!({"agent_principal":{"agent_did":"did:test:missing"}}))?;
        assert!(apply_config_components(&node, &absent).await.is_err());
        ConfigAccess::transact_local(&node, None, "desktop.component.verify", |txn| {
            Box::pin(async move {
                let (_, principal) =
                    read_desired_state_record_in_txn(txn, Collection::AgentPrincipal, owner, owner)
                        .await?
                        .unwrap();
                assert_eq!(principal["display_name"], "Keep name");
                assert_eq!(principal["default_behavior_id"], "behavior");
                assert_eq!(principal["created_by"], "did:test:creator");
                assert_eq!(principal["tags"], json!(["keep"]));
                assert!(read_desired_state_record_in_txn(
                    txn,
                    Collection::InferenceProfile,
                    owner,
                    "profile"
                )
                .await?
                .is_some());
                assert!(read_desired_state_record_in_txn(
                    txn,
                    Collection::InferenceProfile,
                    owner,
                    "bad"
                )
                .await?
                .is_none());
                assert!(read_desired_state_record_in_txn(
                    txn,
                    Collection::AgentPrincipal,
                    "did:test:missing",
                    "did:test:missing"
                )
                .await?
                .is_none());
                let (_, context) = read_desired_state_record_in_txn(
                    txn,
                    Collection::AgentContext,
                    owner,
                    "context",
                )
                .await?
                .unwrap();
                assert_eq!(
                    context["system_prompt"],
                    "literal ${NOT_AN_ENV} {{ not_a_template }}"
                );
                Ok(())
            })
        })
        .await?;
        Ok(())
    }
    #[tokio::test]
    async fn component_patch_preserves_hidden_auth_and_rolls_back_invalid_companion() -> Result<()>
    {
        use gents::config_client::patch::SelfConfigTarget;
        let node = EmbeddedNode::builder().build().await?;
        gents::ensure_runtime_schemas(&node).await?;
        let owner = "did:test:patch";
        for scope in [owner, "did:test:other-patch"] {
            let config: gents::document_config::PackConfig = serde_json::from_value(json!({
                "agent_principal":{"agent_did":scope},
                "inference_backends":[{"agent_did":scope,"backend_id":" backend ","name":"Backend","provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1","auth":{"kind":"api_key","key":"secret-retained"},"connect_timeout_secs":7,"discovery_timeout_secs":8,"max_queue_depth":0,"tags":["retained"]}],
                "inference_profiles":[{"agent_did":scope,"profile_id":"profile","backend_id":" backend ","model_name":"initial"}]
            }))?;
            let plan = DesiredStateApplyPlan::from_pack_config(&config)?;
            ConfigAccess::transact_local(&node, None, "test.patch.seed", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    Ok(())
                })
            })
            .await?;
        }
        let patch = |target, id: &str, fields: serde_json::Value| {
            (
                target,
                id.to_owned(),
                fields
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.clone(), Some(v.clone())))
                    .collect(),
            )
        };
        patch_config_components(
            &node,
            owner,
            &[
                patch(
                    SelfConfigTarget::InferenceBackend,
                    " backend ",
                    json!({"name":"Renamed"}),
                ),
                patch(
                    SelfConfigTarget::InferenceProfile,
                    "profile",
                    json!({"model_name":"selected"}),
                ),
            ],
        )
        .await?;
        let read = || {
            ConfigAccess::transact_local(&node, None, "test.patch.read", |txn| {
                Box::pin(async move {
                    let (_, backend) = read_desired_state_record_in_txn(
                        txn,
                        Collection::InferenceBackend,
                        owner,
                        " backend ",
                    )
                    .await?
                    .unwrap();
                    let (_, profile) = read_desired_state_record_in_txn(
                        txn,
                        Collection::InferenceProfile,
                        owner,
                        "profile",
                    )
                    .await?
                    .unwrap();
                    Ok((backend, profile))
                })
            })
        };
        let before = read().await?;
        assert_eq!(before.0["auth"]["key"], "secret-retained");
        assert_eq!(before.0["max_queue_depth"], 0);
        assert_eq!(before.0["connect_timeout_secs"], 7);
        assert_eq!(before.0["discovery_timeout_secs"], 8);
        assert_eq!(before.0["tags"], json!(["retained"]));
        assert_eq!(before.1["model_name"], "selected");
        assert!(patch_config_components(&node, owner, &[
            patch(SelfConfigTarget::InferenceBackend," backend ",json!({"endpoint":"http://changed:9000/v1","auth":{"kind":"api_key","key":"replace"}})),
            patch(SelfConfigTarget::InferenceProfile,"profile",json!({"model_name":"replacement", "context_window":0})),
        ]).await.is_err());
        assert_eq!(read().await?, before);
        for changes in [
            json!({"agent_did":"did:test:other-patch"}),
            json!({"backend_id":"retarget"}),
            json!({"models":["invented"]}),
        ] {
            assert!(patch_config_components(
                &node,
                owner,
                &[patch(
                    SelfConfigTarget::InferenceBackend,
                    " backend ",
                    changes
                )]
            )
            .await
            .is_err());
        }
        assert!(patch_config_components(
            &node,
            owner,
            &[patch(
                SelfConfigTarget::InferenceBackend,
                "backend",
                json!({"name":"no trim"})
            )]
        )
        .await
        .is_err());
        assert!(patch_config_components(&node, "did:test:absent", &[])
            .await
            .is_err());
        let duplicate = patch(
            SelfConfigTarget::InferenceBackend,
            " backend ",
            json!({"name":"duplicate"}),
        );
        assert!(
            patch_config_components(&node, owner, &[duplicate.clone(), duplicate])
                .await
                .is_err()
        );
        patch_config_components(
            &node,
            owner,
            &[patch(
                SelfConfigTarget::InferenceBackend,
                " backend ",
                json!({"auth":{"kind":"unauthenticated"},"max_queue_depth":0}),
            )],
        )
        .await?;
        assert_eq!(read().await?.0["auth"], json!({"kind":"unauthenticated"}));
        ConfigAccess::transact_local(&node, None, "test.patch.foreign", |txn| {
            Box::pin(async move {
                let (_, other) = read_desired_state_record_in_txn(
                    txn,
                    Collection::InferenceBackend,
                    "did:test:other-patch",
                    " backend ",
                )
                .await?
                .unwrap();
                assert_eq!(other["auth"]["key"], "secret-retained");
                assert_eq!(other["name"], "Backend");
                Ok(())
            })
        })
        .await?;
        Ok(())
    }
}
