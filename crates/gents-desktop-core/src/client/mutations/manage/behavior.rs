use anyhow::{Context, Result};
#[cfg(test)]
use defra_node::EmbeddedNode;
use gents::collection::Collection;
#[cfg(test)]
use gents::config_client::{apply_desired_state_plan, read_desired_state_record_in_txn};
use gents::config_client::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents::AgentBehaviorDocument;

pub async fn upsert_agent_behavior_on(
    access: &ConfigAccess,
    document: &AgentBehaviorDocument,
) -> Result<()> {
    access
        .transact("desktop.behavior.save", |txn| {
            Box::pin(async move {
                let (_, value) = gents::config_client::read_desired_state_record_in_txn(
                    txn,
                    Collection::AgentBehavior,
                    &document.agent_did,
                    &document.behavior_id,
                )
                .await?
                .with_context(|| {
                    format!(
                        "AgentBehavior {:?} does not exist for principal {:?}; create it through the behavior scaffold or persona materializer",
                        document.behavior_id, document.agent_did
                    )
                })?;
                let mut retained: AgentBehaviorDocument = serde_json::from_value(value)?;
                anyhow::ensure!(
                    retained.behavior_id
                        != gents::behavior_scope::SETUP_CONFIGURATOR_BEHAVIOR_ID
                        && !retained.tags.iter().any(|tag| {
                            tag == gents::agent::persona_ops::SETUP_STEWARD_BEHAVIOR_TAG
                        }),
                    "protected configurator behavior cannot be modified"
                );
                anyhow::ensure!(
                    document.context_id == retained.context_id
                        && document.inference_profile_id == retained.inference_profile_id,
                    "behavior save cannot change context_id or inference_profile_id; use the behavior materializer"
                );
                retained.display_name = document.display_name.clone();
                retained.description = document.description.clone();
                retained.enabled = document.enabled;
                let system_tags = retained
                    .tags
                    .iter()
                    .filter(|tag| gents::config_client::is_behavior_system_tag(tag))
                    .cloned()
                    .collect::<Vec<_>>();
                retained.tags = document.tags.clone();
                for tag in system_tags {
                    if !retained.tags.contains(&tag) {
                        retained.tags.push(tag);
                    }
                }
                let value = serde_json::to_value(&retained)?;
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: Collection::AgentBehavior,
                    add: value.clone(),
                    update: value,
                }])?;
                gents::config_client::apply_desired_state_plan(txn, &plan).await?;
                Ok(())
            })
        })
        .await
}

pub async fn create_disabled_behavior_scaffold_on(
    access: &ConfigAccess,
    agent_did: &str,
    source_behavior_id: &str,
    display_name: &str,
    request_id: &str,
) -> Result<String> {
    access
        .transact("desktop.behavior.create_scaffold", |txn| {
            Box::pin(async move {
                gents::config_client::materialize_disabled_behavior_scaffold_in_txn(
                    txn,
                    agent_did,
                    source_behavior_id,
                    display_name,
                    request_id,
                )
                .await
            })
        })
        .await
}

#[cfg(test)]
pub async fn upsert_agent_behavior(
    node: &EmbeddedNode,
    document: &AgentBehaviorDocument,
) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::AgentBehavior,
        add: value.clone(),
        update: value,
    }])?;
    ConfigAccess::transact_local(node, None, "desktop.behavior.save", |txn| {
        let plan = &plan;
        Box::pin(async move {
            apply_desired_state_plan(txn, plan).await?;
            Ok(())
        })
    })
    .await
}

#[cfg(test)]
pub async fn delete_agent_behavior(
    node: &EmbeddedNode,
    agent_did: &str,
    id: &str,
) -> Result<usize> {
    ConfigAccess::transact_local(node, None, "desktop.behavior.delete", |txn| {
        Box::pin(async move {
            gents::config_client::delete_behavior_closure_in_txn(txn, agent_did, id).await
        })
    })
    .await
}

pub async fn delete_agent_behavior_on(
    access: &ConfigAccess,
    agent_did: &str,
    id: &str,
) -> Result<usize> {
    access
        .transact("desktop.behavior.delete", |txn| {
            Box::pin(async move {
                gents::config_client::delete_behavior_closure_in_txn(txn, agent_did, id).await
            })
        })
        .await
}

#[cfg(test)]
pub async fn delete_agent_context(node: &EmbeddedNode, agent_did: &str, id: &str) -> Result<usize> {
    super::delete_scoped_document_local(
        node,
        "desktop.context.delete",
        Collection::AgentContext,
        agent_did,
        id,
    )
    .await
}

pub async fn delete_agent_context_on(
    access: &ConfigAccess,
    agent_did: &str,
    id: &str,
) -> Result<usize> {
    super::delete_scoped_document(
        access,
        "desktop.context.delete",
        Collection::AgentContext,
        agent_did,
        id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn behavior_save_updates_metadata_but_rejects_create_and_rebinding() -> Result<()> {
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        gents::ensure_runtime_schemas(&node).await?;
        let owner = "did:test:behavior-save";
        gents::ensure_agent_principal(&node, owner).await?;
        let access = ConfigAccess::Local(node.clone());
        let mut behavior: AgentBehaviorDocument = serde_json::from_value(json!({
            "agent_did":owner,
            "behavior_id":"local:review",
            "inference_profile_id":"local:review:inference",
            "display_name":"Review"
        }))?;

        let error = upsert_agent_behavior_on(&access, &behavior)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("does not exist"));

        let documents = [
            (Collection::InferenceBackend, json!({"agent_did":owner,"backend_id":"backend","name":"Backend","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:1/v1","auth":{"kind":"unauthenticated"}})),
            (Collection::InferenceProfile, json!({"agent_did":owner,"profile_id":"local:review:inference","scope_behavior_id":"local:review","backend_id":"backend","model_name":"test"})),
            (Collection::AgentBehavior, serde_json::to_value(&behavior)?),
        ]
        .into_iter()
        .map(|(collection, value)| DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        })
        .collect();
        let plan = DesiredStateApplyPlan::new(documents)?;
        access
            .transact("test.behavior.save.seed", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    Ok(())
                })
            })
            .await?;

        behavior.display_name = Some("Renamed review".into());
        behavior.description = Some("Updated metadata".into());
        behavior.tags = vec!["review".into()];
        behavior.enabled = false;
        upsert_agent_behavior_on(&access, &behavior).await?;
        let stored = gents::load_agent_behavior(&node, owner, "local:review")
            .await?
            .unwrap();
        assert_eq!(stored.display_name.as_deref(), Some("Renamed review"));
        assert!(!stored.enabled);
        assert_eq!(stored.inference_profile_id, "local:review:inference");

        behavior.inference_profile_id = "foreign-profile".into();
        let error = upsert_agent_behavior_on(&access, &behavior)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("cannot change"));
        let stored = gents::load_agent_behavior(&node, owner, "local:review")
            .await?
            .unwrap();
        assert_eq!(stored.inference_profile_id, "local:review:inference");

        let mut protected = stored;
        protected.behavior_id = gents::behavior_scope::SETUP_CONFIGURATOR_BEHAVIOR_ID.into();
        protected.inference_profile_id = "gents:base:configurator:inference".into();
        protected.tags.clear();
        let protected_profile = json!({
            "agent_did":owner,
            "profile_id":"gents:base:configurator:inference",
            "scope_behavior_id":gents::behavior_scope::SETUP_CONFIGURATOR_BEHAVIOR_ID,
            "backend_id":"backend",
            "model_name":"test"
        });
        let protected_behavior = serde_json::to_value(&protected)?;
        let plan = DesiredStateApplyPlan::new(vec![
            DesiredStateApplyDocument {
                collection: Collection::InferenceProfile,
                add: protected_profile.clone(),
                update: protected_profile,
            },
            DesiredStateApplyDocument {
                collection: Collection::AgentBehavior,
                add: protected_behavior.clone(),
                update: protected_behavior,
            },
        ])?;
        access
            .transact("test.behavior.save.protected", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    Ok(())
                })
            })
            .await?;
        protected.enabled = false;
        let error = upsert_agent_behavior_on(&access, &protected)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("protected configurator"));
        let error = delete_agent_behavior(
            &node,
            owner,
            gents::behavior_scope::SETUP_CONFIGURATOR_BEHAVIOR_ID,
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("protected configurator"));
        Ok(())
    }

    #[tokio::test]
    async fn context_delete_removes_the_scoped_document() -> Result<()> {
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        gents::ensure_runtime_schemas(&node).await?;
        let owner = "did:test:context-delete";
        gents::ensure_agent_principal(&node, owner).await?;
        let context = json!({
            "agent_did": owner,
            "context_id": "temporary",
            "display_name": "Temporary"
        });
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: Collection::AgentContext,
            add: context.clone(),
            update: context,
        }])?;
        ConfigAccess::Local(node.clone())
            .transact("test.context.create", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    Ok(())
                })
            })
            .await?;

        assert_eq!(delete_agent_context(&node, owner, "temporary").await?, 1);
        assert_eq!(delete_agent_context(&node, owner, "temporary").await?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn behavior_and_profile_deletion_obey_canonical_owned_references() -> Result<()> {
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        gents::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::Local(node.clone());
        for owner in ["did:test:alpha", "did:test:beta"] {
            gents::ensure_agent_principal(&node, owner).await?;
            let backend = serde_json::from_value(json!({
                "agent_did":owner, "backend_id":"backend", "name":"Backend",
                "provider_kind":"OpenAiCompatible", "endpoint":"http://localhost:8000/v1", "auth":{"kind":"unauthenticated"}
            }))?;
            gents::config_client::write_inference_backend_document(&access, &backend).await?;
            let profile = serde_json::from_value(json!({
                "agent_did":owner,"profile_id":"profile","backend_id":"backend","model_name":"model","reasoning_effort":"high"
            }))?;
            super::super::profile::upsert_inference_profile(&node, &profile).await?;
            let behavior = serde_json::from_value(json!({
                "agent_did":owner,"behavior_id":"review","inference_profile_id":"profile"
            }))?;
            upsert_agent_behavior(&node, &behavior).await?;
        }
        let invalid = serde_json::from_value(json!({
            "agent_did":"did:test:alpha","behavior_id":"review","inference_profile_id":"profile","context_id":"missing"
        }))?;
        assert!(upsert_agent_behavior(&node, &invalid).await.is_err());
        assert!(super::super::profile::delete_inference_profile(
            &node,
            "did:test:alpha",
            "profile"
        )
        .await
        .is_err());
        // The shared closure owns local target references; foreign destinations
        // remain governed by delegation admission, not global label lookup.
        let target = json!({
            "agent_did":"did:test:alpha","target_id":"target","target_agent_did":"did:test:alpha","behavior_id":"review","name":"reviewer"
        });
        let target_plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: Collection::SubagentTarget,
            add: target.clone(),
            update: target,
        }])?;
        access
            .transact("test.target", |txn| {
                let plan = &target_plan;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    Ok(())
                })
            })
            .await?;
        assert!(delete_agent_behavior(&node, "did:test:alpha", "review")
            .await
            .is_err());
        assert_eq!(
            delete_agent_behavior(&node, "did:test:beta", "review").await?,
            1
        );
        assert_eq!(
            super::super::profile::delete_inference_profile(&node, "did:test:beta", "profile")
                .await?,
            1
        );
        assert!(delete_agent_behavior(&node, "did:test:alpha", "review")
            .await
            .is_err());
        let remove_target = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
            Collection::SubagentTarget,
            "did:test:alpha".into(),
            "target".into(),
        )])?;
        access
            .transact("test.target.remove", |txn| {
                let plan = &remove_target;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    Ok(())
                })
            })
            .await?;
        for default in [Some("review"), None] {
            access
                .transact("test.principal.default", |txn| {
                    Box::pin(async move {
                        let (_, mut value) = read_desired_state_record_in_txn(
                            txn,
                            Collection::AgentPrincipal,
                            "did:test:alpha",
                            "did:test:alpha",
                        )
                        .await?
                        .unwrap();
                        value["default_behavior_id"] = serde_json::to_value(default)?;
                        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                            collection: Collection::AgentPrincipal,
                            add: value.clone(),
                            update: value,
                        }])?;
                        apply_desired_state_plan(txn, &plan).await?;
                        Ok(())
                    })
                })
                .await?;
            if default.is_some() {
                assert!(delete_agent_behavior(&node, "did:test:alpha", "review")
                    .await
                    .is_err());
            }
        }
        assert_eq!(
            delete_agent_behavior(&node, "did:test:alpha", "review").await?,
            1
        );
        assert_eq!(
            super::super::profile::delete_inference_profile(&node, "did:test:alpha", "profile")
                .await?,
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn scaffold_replays_strips_reserved_tags_and_delete_removes_scoped_closure() -> Result<()>
    {
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        gents::ensure_runtime_schemas(&node).await?;
        let owner = "did:test:scaffold";
        gents::ensure_agent_principal(&node, owner).await?;
        let access = ConfigAccess::Local(node.clone());
        let backend = serde_json::from_value(json!({
            "agent_did":owner,"backend_id":"backend","name":"Backend",
            "provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1",
            "auth":{"kind":"unauthenticated"}
        }))?;
        gents::config_client::write_inference_backend_document(&access, &backend).await?;
        let profile = serde_json::from_value(json!({
            "agent_did":owner,"profile_id":"source-profile","backend_id":"backend",
            "model_name":"model","tags":["gents:pack:test","user:profile"]
        }))?;
        super::super::profile::upsert_inference_profile(&node, &profile).await?;
        let behavior = serde_json::from_value(json!({
            "agent_did":owner,"behavior_id":"source","inference_profile_id":"source-profile",
            "tags":["gents:setup-steward","gents:pack:test","user:behavior"]
        }))?;
        upsert_agent_behavior(&node, &behavior).await?;

        let first = create_disabled_behavior_scaffold_on(
            &access,
            owner,
            "source",
            "New behaviour",
            "request-1",
        )
        .await?;
        let mut first_document = gents::load_agent_behavior(&node, owner, &first)
            .await?
            .unwrap();
        first_document.tags = vec!["user:edited".into()];
        upsert_agent_behavior_on(&access, &first_document).await?;
        let replay = create_disabled_behavior_scaffold_on(
            &access,
            owner,
            "source",
            "New behaviour",
            "request-1",
        )
        .await?;
        assert_eq!(first, replay);
        let behaviors = gents::list_agent_behaviors(&node, owner).await?;
        assert_eq!(
            behaviors
                .iter()
                .filter(|row| row.behavior_id == first)
                .count(),
            1
        );
        let cloned = behaviors
            .into_iter()
            .find(|row| row.behavior_id == first)
            .unwrap();
        let reuse_error = create_disabled_behavior_scaffold_on(
            &access,
            owner,
            "source",
            "Different request intent",
            "request-1",
        )
        .await
        .unwrap_err();
        assert!(format!("{reuse_error:#}").contains("different source behavior or display name"));
        assert!(!cloned.enabled);
        assert!(!cloned
            .tags
            .iter()
            .any(|tag| tag == "gents:setup-steward" || tag.starts_with("gents:pack:")));
        assert!(cloned.tags.contains(&"user:edited".into()));
        assert_eq!(
            cloned
                .tags
                .iter()
                .filter(|tag| tag.starts_with("gents:desktop-scaffold"))
                .count(),
            3
        );
        let cloned_profile =
            gents::load_inference_profile(&node, owner, &cloned.inference_profile_id)
                .await?
                .unwrap();
        assert_eq!(
            cloned_profile.scope_behavior_id.as_deref(),
            Some(first.as_str())
        );
        assert_eq!(cloned_profile.backend_id, "backend");
        assert!(cloned_profile.tags.contains(&"user:profile".into()));

        let second = create_disabled_behavior_scaffold_on(
            &access,
            owner,
            "source",
            "Other behaviour",
            "request-2",
        )
        .await?;
        assert_eq!(delete_agent_behavior(&node, owner, &second).await?, 1);
        assert!(
            gents::load_inference_profile(&node, owner, &format!("{second}:inference"))
                .await?
                .is_none()
        );

        let escaped_owner = gents::graphql::escape_graphql_string(owner);
        let escaped_behavior = gents::graphql::escape_graphql_string(&first);
        let response = node.execute(&format!(r#"mutation {{ create_AgentSession(input: {{session_id:"session",agent_did:"{escaped_owner}",requester_did:"{escaped_owner}",behavior_id:"{escaped_behavior}",created_at:"2026-01-01T00:00:00Z"}}) {{_docID}} }}"#)).await;
        gents::graphql::ensure_no_errors(&response, "create retained session")?;
        assert!(delete_agent_behavior(&node, owner, &first)
            .await
            .unwrap_err()
            .to_string()
            .contains("AgentSession"));
        Ok(())
    }
}
