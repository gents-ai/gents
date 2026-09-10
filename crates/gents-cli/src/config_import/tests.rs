use super::*;
use serde_json::json;
use std::sync::Arc;

fn config(owner: &str, tools: &str) -> desired_state::DesiredStateManifest {
    serde_json::from_value(json!({
        "agent_principal":{"agent_did":owner},
        "contexts":[{"agent_did":owner,"context_id":"context","tools_id":tools}],
        "tools":[{"agent_did":owner,"tools_id":tools,"host":{"bash":{"allowed_argv_prefixes":[]}}}]
    }))
    .unwrap()
}

async fn apply_config(
    access: &ConfigAccess,
    desired: &desired_state::DesiredStateManifest,
    live: Option<&desired_state::DesiredStateManifest>,
    prune: bool,
) -> Result<ConfigApplyCounts> {
    let empty = serde_json::from_value(json!({"agent_principal":desired.agent_principal}))?;
    let mut report = desired_state::diff_manifests(
        std::path::Path::new("."),
        "test",
        desired,
        live.map(|live| &live.agent_principal),
        live.unwrap_or(&empty),
        false,
    );
    if prune {
        if let Some(live) = live {
            let removals = live
                .tools
                .iter()
                .filter(|tool| {
                    !desired
                        .tools
                        .iter()
                        .any(|next| next.tools_id == tool.tools_id)
                })
                .map(|tool| gents::apply_model::DocRef {
                    collection: Collection::Tools,
                    id: tool.tools_id.clone(),
                })
                .collect::<Vec<_>>();
            report.collections.record_prune_deletes(&removals);
        }
    }
    let bundle = desired_state::export_bundle_from_manifest(desired, "test")?;
    access
        .transact("test.config_import", |txn| {
            let bundle = &bundle;
            let report = &report;
            Box::pin(async move { apply_desired_state_changes(txn, bundle, report).await })
        })
        .await
}

async fn tools_id(access: &ConfigAccess, owner: &str, id: &str) -> Result<Option<String>> {
    access
        .transact("test.config_import.read", |txn| {
            Box::pin(async move {
                Ok(
                    read_desired_state_record_in_txn(txn, Collection::Tools, owner, id)
                        .await?
                        .map(|(id, _)| id),
                )
            })
        })
        .await
}

#[tokio::test]
async fn imports_complete_candidate_and_recreates_only_the_scoped_tombstone() -> Result<()> {
    let node = Arc::new(gents::defra_node::EmbeddedNode::builder().build().await?);
    gents::ensure_runtime_schemas(&node).await?;
    let access = ConfigAccess::Local(node.clone());
    let first = config("owner-a", "tools");
    let foreign = config("owner-b", "tools");
    let counts = apply_config(&access, &first, None, false).await?;
    assert_eq!(counts.get(Collection::AgentContext), 1);
    assert_eq!(counts.get(Collection::Tools), 1);
    apply_config(&access, &foreign, None, false).await?;
    let original = tools_id(&access, "owner-a", "tools").await?.unwrap();
    let foreign_id = tools_id(&access, "owner-b", "tools").await?.unwrap();

    // The replacement context and removal must be validated together.
    let replacement = config("owner-a", "replacement");
    let counts = apply_config(&access, &replacement, Some(&first), true).await?;
    assert_eq!(counts.get(Collection::Tools), 2);
    assert!(tools_id(&access, "owner-a", "tools").await?.is_none());
    assert_eq!(
        tools_id(&access, "owner-b", "tools").await?,
        Some(foreign_id.clone())
    );
    apply_config(&access, &first, Some(&replacement), true).await?;
    assert_ne!(
        tools_id(&access, "owner-a", "tools").await?.unwrap(),
        original
    );
    assert_eq!(
        tools_id(&access, "owner-b", "tools").await?,
        Some(foreign_id)
    );

    // Retained inbound references prohibit an independently requested deletion.
    let rejected = access
        .transact("test.config_import.dangling_delete", |txn| {
            Box::pin(async move {
                apply_delete_collection(txn, Collection::Tools, "owner-a", &["tools".into()]).await
            })
        })
        .await;
    assert!(rejected.is_err());
    assert!(tools_id(&access, "owner-a", "tools").await?.is_some());
    node.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn invalid_candidate_rolls_back_other_selected_writes() -> Result<()> {
    let node = Arc::new(gents::defra_node::EmbeddedNode::builder().build().await?);
    gents::ensure_runtime_schemas(&node).await?;
    let access = ConfigAccess::Local(node.clone());
    let valid = config("owner", "tools");
    apply_config(&access, &valid, None, false).await?;
    let mut invalid = valid.clone();
    invalid.contexts[0].tools_id = Some("missing".into());
    invalid.tools[0].display_name = Some("must roll back".into());
    assert!(apply_config(&access, &invalid, Some(&valid), false)
        .await
        .is_err());
    let exported = crate::config_bundle::build_config_export_bundle(&access, "owner").await?;
    assert_eq!(exported.config.tools[0].display_name, None);
    assert_eq!(
        exported.config.contexts[0].tools_id.as_deref(),
        Some("tools")
    );
    node.shutdown().await;
    Ok(())
}
