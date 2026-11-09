use anyhow::Result;
mod editor;
pub use editor::*;
use gents::documents::apply_desired_state_plan;
use gents::documents::Node;
use gents::store::collection::Collection;
use gents::store::config_client::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};

pub async fn upsert_node_on(access: &ConfigAccess, document: &Node) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Node,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_plan(access, "desktop.node.save", plan).await
}

/// Publication rejects a disabled default, so enabling the new default and
/// replacing a stored default that is disabled must land in one apply.
pub async fn set_default_agent_on(
    access: &ConfigAccess,
    node_did: &str,
    agent_did: &str,
) -> Result<()> {
    access
        .transact_write("desktop.node.default_agent", |txn| {
            Box::pin(async move { set_default_agent_in_txn(txn, node_did, agent_did).await })
        })
        .await
}

async fn set_default_agent_in_txn(
    txn: &gents::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    agent_did: &str,
) -> Result<()> {
    use anyhow::Context;
    use gents::store::config_client::read_desired_state_record_in_txn;
    let (_, mut node) = read_desired_state_record_in_txn(txn, Collection::Node, node_did, node_did)
        .await?
        .context("default agent requires an existing node")?;
    let (_, mut agent) =
        read_desired_state_record_in_txn(txn, Collection::Agent, node_did, agent_did)
            .await?
            .with_context(|| format!("Agent {agent_did:?} does not exist"))?;
    node["default_agent_did"] = agent_did.into();
    agent["enabled"] = true.into();
    let plan = DesiredStateApplyPlan::new(
        [(Collection::Agent, agent), (Collection::Node, node)]
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
    )?;
    apply_desired_state_plan(txn, &plan).await?;
    Ok(())
}

#[cfg(test)]
pub async fn apply_config_components(
    access: &ConfigAccess,
    document: &gents::documents::PackConfig,
) -> Result<()> {
    let (owner, plan) = config_components_plan(document)?;
    access
        .transact_write("desktop.config.components", |txn| {
            let plan = &plan;
            let owner = &owner;
            Box::pin(async move { apply_config_components_in_txn(txn, owner, plan).await })
        })
        .await
}

pub(crate) async fn apply_config_components_with_publication_on(
    access: &ConfigAccess,
    document: &gents::documents::PackConfig,
    observer: std::sync::Arc<dyn gents::store::config_client::AgentPublicationObserver>,
) -> Result<()> {
    let (owner, plan) = config_components_plan(document)?;
    access
        .transact_write_with_agent_publication("desktop.config.components", observer, |txn| {
            let plan = &plan;
            let owner = &owner;
            Box::pin(async move { apply_config_components_in_txn(txn, owner, plan).await })
        })
        .await
}

async fn apply_config_components_in_txn(
    txn: &gents::store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    plan: &DesiredStateApplyPlan,
) -> Result<()> {
    use anyhow::Context;
    gents::store::config_client::read_desired_state_record_in_txn(
        txn,
        Collection::Node,
        owner,
        owner,
    )
    .await?
    .context("component apply requires an existing node")?;
    apply_desired_state_plan(txn, plan).await?;
    Ok(())
}

fn config_components_plan(
    document: &gents::documents::PackConfig,
) -> Result<(String, DesiredStateApplyPlan)> {
    let owner = document.node.node_did.clone();
    let scope: Node = serde_json::from_value(serde_json::json!({"node_did":owner}))?;
    anyhow::ensure!(
        serde_json::to_value(&document.node)? == serde_json::to_value(&scope)?,
        "component apply requires scope-only node; use node save for node settings"
    );
    let plan = DesiredStateApplyPlan::from_pack_config(document)?;
    anyhow::ensure!(
        plan.documents().iter().all(|entry| entry
            .add
            .get("node_did")
            .and_then(serde_json::Value::as_str)
            == Some(owner.as_str())),
        "component owner does not match the selected node"
    );
    let plan = DesiredStateApplyPlan::new(
        plan.documents()
            .iter()
            .filter(|entry| entry.collection != Collection::Node)
            .cloned()
            .collect(),
    )?;
    Ok((owner, plan))
}

/// Patch existing scoped components atomically; omitted fields (including
/// hidden credentials) are retained. Never creates or removes documents.
pub async fn patch_config_components_on(
    access: &ConfigAccess,
    node_did: &str,
    patches: &[(
        gents::store::config_client::patch::SelfConfigTarget,
        String,
        gents::store::config_client::patch::SelfConfigPatch,
    )],
) -> Result<()> {
    patch_config_components_with(access, node_did, patches).await
}

async fn patch_config_components_with(
    access: &ConfigAccess,
    node_did: &str,
    patches: &[(
        gents::store::config_client::patch::SelfConfigTarget,
        String,
        gents::store::config_client::patch::SelfConfigPatch,
    )],
) -> Result<()> {
    access
        .transact_write("desktop.config.components.patch", |txn| {
            Box::pin(async move { patch_config_components_in_txn(txn, node_did, patches).await })
        })
        .await
}

async fn patch_config_components_in_txn(
    txn: &gents::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    patches: &[(
        gents::store::config_client::patch::SelfConfigTarget,
        String,
        gents::store::config_client::patch::SelfConfigPatch,
    )],
) -> Result<()> {
    use anyhow::Context;
    use gents::store::config_client::{
        patch::{apply_patch, ensure_admissible},
        read_desired_state_record_in_txn,
    };
    anyhow::ensure!(
        !node_did.trim().is_empty(),
        "component patch requires an owner"
    );
    let mut identities = rapidhash::RapidHashSet::default();
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
    read_desired_state_record_in_txn(txn, Collection::Node, node_did, node_did)
        .await?
        .context("component patch requires an existing node")?;
    let mut documents = Vec::with_capacity(patches.len());
    for (target, id, patch) in patches {
        let (_, retained) =
            read_desired_state_record_in_txn(txn, target.collection(), node_did, id)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::ServedRuntimeHome;
    use gents::store::config_client::read_desired_state_record_in_txn;
    use serde_json::json;

    #[tokio::test]
    async fn node_replacement_keeps_authored_metadata_and_scoped_default_binding() -> Result<()> {
        let runtime = ServedRuntimeHome::start().await;
        let access = runtime.user_access();
        let other = runtime.node_did();
        let default = runtime.provision_agent().await;
        let target = runtime.fresh_did().await;
        let config: gents::documents::PackConfig = serde_json::from_value(json!({
            "node":{"node_did":other},
            "inference_backends":[{"node_did":other,"backend_id":"backend","name":"Backend","provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1","auth":{"kind":"unauthenticated"}}],
            "inference_profiles":[{"node_did":other,"profile_id":"profile","backend_id":"backend","model_name":"model"}],
            "agents":[{"node_did":other,"agent_did":default.agent_did,"creation_nonce":default.creation_nonce,"inference_profile_id":"profile"}]
        }))?;
        let plan = DesiredStateApplyPlan::from_pack_config(&config)?;
        access
            .transact_write("desktop.node.seed", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        let target = target.as_str();
        let mut document: Node = serde_json::from_value(
            json!({"node_did":target,"display_name":"Agent","avatar":"12","created_at":"2026-01-01T00:00:00Z","created_by":"did:test:creator","tags":["team"]}),
        )?;
        upsert_node_on(&access, &document).await?;
        document.default_agent_did = Some(default.agent_did.clone());
        assert!(upsert_node_on(&access, &document).await.is_err());
        access
            .transact_readonly("desktop.node.verify", |txn| {
                Box::pin(async move {
                    let (_, value) =
                        read_desired_state_record_in_txn(txn, Collection::Node, target, target)
                            .await?
                            .unwrap();
                    let saved: Node = serde_json::from_value(value)?;
                    assert_eq!(saved.created_at.as_deref(), Some("2026-01-01T00:00:00Z"));
                    assert_eq!(saved.created_by.as_deref(), Some("did:test:creator"));
                    assert_eq!(saved.tags, vec!["team"]);
                    assert_eq!(saved.avatar.as_deref(), Some("12"));
                    assert!(saved.default_agent_did.is_none());
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        runtime.shutdown().await;
        Ok(())
    }
    #[tokio::test]
    async fn setting_the_default_enables_it_and_recovers_a_stored_disabled_default() -> Result<()> {
        let runtime = ServedRuntimeHome::start().await;
        let access = runtime.user_access();
        let owner = runtime.node_did();
        let first = runtime.provision_agent().await;
        let second = runtime.provision_agent().await;
        let seed: gents::documents::PackConfig = serde_json::from_value(json!({
            "node":{"node_did":owner,"default_agent_did":first.agent_did,"avatar":"12","tags":["keep"]},
            "inference_backends":[{"node_did":owner,"backend_id":"backend","name":"Backend","provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1","auth":{"kind":"unauthenticated"}}],
            "inference_profiles":[{"node_did":owner,"profile_id":"profile","backend_id":"backend","model_name":"model"}],
            "agents":[
                {"node_did":owner,"agent_did":first.agent_did,"creation_nonce":first.creation_nonce,"inference_profile_id":"profile"},
                {"node_did":owner,"agent_did":second.agent_did,"creation_nonce":second.creation_nonce,"inference_profile_id":"profile","enabled":false}
            ]
        }))?;
        let plan = DesiredStateApplyPlan::from_pack_config(&seed)?;
        access
            .transact_write("desktop.default.seed", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        // The native admin disables the default with a raw GraphQL mutation,
        // outside desired-state publication, which would refuse it.
        runtime
            .user_graphql_access()
            .write(
                "test.default.stale",
                &format!(
                    r#"mutation {{ update_Agent(
                        filter: {{ node_did: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }} }},
                        input: {{ enabled: false }}
                    ) {{ _docID }} }}"#,
                    gents::store::graphql::escape_graphql_string(owner),
                    gents::store::graphql::escape_graphql_string(&first.agent_did),
                ),
            )
            .await?;

        assert!(set_default_agent_on(&access, owner, "missing")
            .await
            .is_err());
        set_default_agent_on(&access, owner, &second.agent_did).await?;
        let second_did = second.agent_did.as_str();
        access
            .transact_readonly("desktop.default.verify", |txn| {
                Box::pin(async move {
                    let (_, node) =
                        read_desired_state_record_in_txn(txn, Collection::Node, owner, owner)
                            .await?
                            .unwrap();
                    assert_eq!(node["default_agent_did"], second_did);
                    assert_eq!(node["tags"], json!(["keep"]));
                    assert_eq!(node["avatar"], "12");
                    let (_, second) =
                        read_desired_state_record_in_txn(txn, Collection::Agent, owner, second_did)
                            .await?
                            .unwrap();
                    assert_eq!(second["enabled"], true);
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        runtime.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn component_apply_is_partial_atomic_scoped_and_preserves_node() -> Result<()> {
        let runtime = ServedRuntimeHome::start_unpublished().await;
        let access = runtime.user_access();
        let owner = runtime.node_did();
        runtime
            .publish_node(json!({"display_name":"Keep name","avatar":"12","created_at":"2026-01-01T00:00:00Z","created_by":"did:test:creator","tags":["keep"]}))
            .await;
        let agent = runtime.provision_agent().await;
        let missing = runtime.fresh_did().await;
        let missing = missing.as_str();
        let seed: gents::documents::PackConfig = serde_json::from_value(json!({
            "node":{"node_did":owner,"display_name":"Keep name","avatar":"12","default_agent_did":agent.agent_did,"created_at":"2026-01-01T00:00:00Z","created_by":"did:test:creator","tags":["keep"]},
            "inference_backends":[{"node_did":owner,"backend_id":"backend","name":"Backend","provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1","auth":{"kind":"unauthenticated"}}],
            "inference_profiles":[{"node_did":owner,"profile_id":"profile","backend_id":"backend","model_name":"model"}],
            "agents":[{"node_did":owner,"agent_did":agent.agent_did,"creation_nonce":agent.creation_nonce,"context_id":"context","inference_profile_id":"profile"}],
            "contexts":[{"node_did":owner,"context_id":"context","system_prompt":"initial"}]
        }))?;
        let plan = DesiredStateApplyPlan::from_pack_config(&seed)?;
        access
            .transact_write("desktop.component.seed", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        let input = json!({"node":{"node_did":owner},"contexts":[{"node_did":owner,"context_id":"context","system_prompt":"literal ${NOT_AN_ENV} {{ not_a_template }}"}]});
        let document = serde_json::from_value(input.clone())?;
        apply_config_components(&access, &document).await?;
        let mut invalid = input.clone();
        invalid["contexts"][0]["system_prompt"] = json!("must roll back");
        invalid["inference_profiles"] = json!([{"node_did":owner,"profile_id":"bad","backend_id":"missing","model_name":"model"}]);
        assert!(
            apply_config_components(&access, &serde_json::from_value(invalid)?)
                .await
                .is_err()
        );
        let mut foreign = input.clone();
        foreign["contexts"][0]["node_did"] = json!("did:test:foreign");
        assert!(
            apply_config_components(&access, &serde_json::from_value(foreign)?)
                .await
                .is_err()
        );
        for (key, value) in [
            ("display_name", json!("discarded")),
            ("default_agent_did", json!("different")),
            ("created_at", json!("2026-02-01T00:00:00Z")),
            ("created_by", json!("other")),
            ("enabled", json!(false)),
            ("tags", json!(["other"])),
        ] {
            let mut changed = input.clone();
            changed["node"][key] = value;
            assert!(
                apply_config_components(&access, &serde_json::from_value(changed)?)
                    .await
                    .is_err(),
                "must reject meaningful node setting {key}"
            );
        }
        let absent = serde_json::from_value(json!({"node":{"node_did":missing}}))?;
        assert!(apply_config_components(&access, &absent).await.is_err());
        let agent_did = agent.agent_did.as_str();
        access
            .transact_readonly("desktop.component.verify", |txn| {
                Box::pin(async move {
                    let (_, node) =
                        read_desired_state_record_in_txn(txn, Collection::Node, owner, owner)
                            .await?
                            .unwrap();
                    assert_eq!(node["display_name"], "Keep name");
                    assert_eq!(node["default_agent_did"], agent_did);
                    assert_eq!(node["created_by"], "did:test:creator");
                    assert_eq!(node["tags"], json!(["keep"]));
                    assert_eq!(node["avatar"], "12");
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
                        Collection::Node,
                        missing,
                        missing
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
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        runtime.shutdown().await;
        Ok(())
    }
    #[tokio::test]
    async fn component_patch_preserves_hidden_auth_and_rolls_back_invalid_companion() -> Result<()>
    {
        use gents::store::config_client::patch::SelfConfigTarget;
        let runtime = ServedRuntimeHome::start().await;
        let access = runtime.user_access();
        let owner = runtime.node_did();
        let other_patch = runtime.fresh_did().await;
        let other_patch = other_patch.as_str();
        let absent = runtime.fresh_did().await;
        for scope in [owner, other_patch] {
            let config: gents::documents::PackConfig = serde_json::from_value(json!({
                "node":{"node_did":scope},
                "inference_backends":[{"node_did":scope,"backend_id":" backend ","name":"Backend","provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1","auth":{"kind":"api_key","key":"secret-retained"},"connect_timeout_secs":7,"discovery_timeout_secs":8,"max_queue_depth":0,"tags":["retained"]}],
                "inference_profiles":[{"node_did":scope,"profile_id":"profile","backend_id":" backend ","model_name":"initial"}]
            }))?;
            let plan = DesiredStateApplyPlan::from_pack_config(&config)?;
            access
                .transact_write("test.patch.seed", |txn| {
                    let plan = &plan;
                    Box::pin(async move {
                        apply_desired_state_plan(txn, plan).await?;
                        Ok::<_, anyhow::Error>(())
                    })
                })
                .await?;
        }
        let mut seeded = Vec::new();
        for scope in [owner, other_patch] {
            let backend: Option<(String, serde_json::Value)> = access
                .transact_readonly("test.patch.seeded", |txn| {
                    Box::pin(async move {
                        read_desired_state_record_in_txn(
                            txn,
                            Collection::InferenceBackend,
                            scope,
                            " backend ",
                        )
                        .await
                    })
                })
                .await?;
            seeded.push(backend.unwrap().1["auth"].clone());
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
        patch_config_components_on(
            &access,
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
            access.transact_readonly("test.patch.read", |txn| {
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
                    Ok::<_, anyhow::Error>((backend, profile))
                })
            })
        };
        let before = read().await?;
        // The runtime moves the seeded API key into a node credential; the
        // patch must retain that hidden reference unchanged.
        assert_eq!(seeded[0]["kind"], "credential");
        assert!(seeded[0].get("key").is_none());
        assert_eq!(before.0["auth"], seeded[0]);
        assert_eq!(before.0["max_queue_depth"], 0);
        assert_eq!(before.0["connect_timeout_secs"], 7);
        assert_eq!(before.0["discovery_timeout_secs"], 8);
        assert_eq!(before.0["tags"], json!(["retained"]));
        assert_eq!(before.1["model_name"], "selected");
        assert!(patch_config_components_on(&access, owner, &[
            patch(SelfConfigTarget::InferenceBackend," backend ",json!({"endpoint":"http://changed:9000/v1","auth":{"kind":"api_key","key":"replace"}})),
            patch(SelfConfigTarget::InferenceProfile,"profile",json!({"model_name":"replacement", "context_window":0})),
        ]).await.is_err());
        assert_eq!(read().await?, before);
        for changes in [
            json!({"node_did":other_patch}),
            json!({"backend_id":"retarget"}),
            json!({"models":["invented"]}),
        ] {
            assert!(patch_config_components_on(
                &access,
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
        assert!(patch_config_components_on(
            &access,
            owner,
            &[patch(
                SelfConfigTarget::InferenceBackend,
                "backend",
                json!({"name":"no trim"})
            )]
        )
        .await
        .is_err());
        assert!(patch_config_components_on(&access, &absent, &[])
            .await
            .is_err());
        let duplicate = patch(
            SelfConfigTarget::InferenceBackend,
            " backend ",
            json!({"name":"duplicate"}),
        );
        assert!(
            patch_config_components_on(&access, owner, &[duplicate.clone(), duplicate])
                .await
                .is_err()
        );
        patch_config_components_on(
            &access,
            owner,
            &[patch(
                SelfConfigTarget::InferenceBackend,
                " backend ",
                json!({"auth":{"kind":"unauthenticated"},"max_queue_depth":0}),
            )],
        )
        .await?;
        assert_eq!(read().await?.0["auth"], json!({"kind":"unauthenticated"}));
        let other_seeded = &seeded[1];
        access
            .transact_readonly("test.patch.foreign", |txn| {
                Box::pin(async move {
                    let (_, other) = read_desired_state_record_in_txn(
                        txn,
                        Collection::InferenceBackend,
                        other_patch,
                        " backend ",
                    )
                    .await?
                    .unwrap();
                    assert_eq!(other["auth"], *other_seeded);
                    assert_eq!(other["name"], "Backend");
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        runtime.shutdown().await;
        Ok(())
    }
}
