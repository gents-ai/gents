use super::*;

#[tokio::test]
async fn live_validate_rejects_invalid_event_trigger_collection_identifier() -> Result<()> {
    use super::super::DesiredEventTrigger;

    let tempdir = tempfile::tempdir()?;
    let node = EmbeddedNode::builder()
        .data_path(tempdir.path().join("data"))
        .with_storage_backend(StorageBackend::Lark)
        .build()
        .await?;
    ensure_runtime_schemas(&node).await?;
    let access = ConfigAccess::Local(std::sync::Arc::new(node));

    let mut manifest = manifest_with_subagent_targets(Vec::new());
    manifest.event_triggers.push(DesiredEventTrigger {
        trigger_id: "malformed-source".to_string(),
        task_id: "unused-task".to_string(),
        source_collection: "AgentMessage) { _docID } mutation {".to_string(),
        event_kind: "created".to_string(),
        filter: Some("{}".to_string()),
        correlation_field: None,
        fire_mode: None,
        expected_count: None,
        expected_count_field: None,
        group_timeout_secs: None,
        group_min_count: None,
        workspace_authority: None,
        enabled: true,
        concurrency: "serial".to_string(),
    });

    let errors = validate_manifest_against_live(&manifest, &access).await?;
    assert!(
        errors.iter().any(|error| {
            error.contains("malformed-source")
                && error.contains("invalid source_collection")
                && error.contains("invalid identifier")
        }),
        "expected direct live-validation identifier rejection, got {errors:?}"
    );
    Ok(())
}

#[tokio::test]
async fn live_validate_does_not_resolve_remote_subagent_target() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let data_dir = tempdir.path().join("data");
    let node = EmbeddedNode::builder()
        .data_path(&data_dir)
        .with_storage_backend(StorageBackend::Lark)
        .build()
        .await?;
    ensure_runtime_schemas(&node).await?;

    let access = ConfigAccess::Local(std::sync::Arc::new(node));

    let manifest = manifest_with_subagent_targets(vec![SubagentTarget {
        name: "remote-researcher".to_string(),
        agent_did: "did:key:zRemotePeer".to_string(),
        behavior_id: "does-not-exist-locally".to_string(),
        description: None,
    }]);
    let errors = validate_manifest_against_live(&manifest, &access).await?;

    assert!(
        !errors
            .iter()
            .any(|msg| msg.contains("does-not-exist-locally") || msg.contains("live-test-sel")),
        "remote subagent target must not trigger live resolution errors, got {errors:?}"
    );
    Ok(())
}

#[tokio::test]
async fn live_validate_passes_for_known_subagent_target() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let data_dir = tempdir.path().join("data");
    let node = EmbeddedNode::builder()
        .data_path(&data_dir)
        .with_storage_backend(StorageBackend::Lark)
        .build()
        .await?;
    ensure_runtime_schemas(&node).await?;

    let access = ConfigAccess::Local(std::sync::Arc::new(node));

    let manifest = manifest_with_subagent_targets(vec![SubagentTarget {
        name: "researcher".to_string(),
        agent_did: "did:key:test-live-validate".to_string(),
        behavior_id: "amy-research".to_string(),
        description: None,
    }]);
    let errors = validate_manifest_against_live(&manifest, &access).await?;

    assert!(
        !errors
            .iter()
            .any(|msg| msg.contains("amy-research") || msg.contains("live-test-sel")),
        "expected no subagent errors for known target, got {errors:?}"
    );
    Ok(())
}

#[tokio::test]
async fn live_validate_rejects_non_manifest_pairing_collision_and_diff_reports_it() -> Result<()> {
    use super::super::DesiredPeerPairing;
    use crate::commands::config::binding::{
        BoundDesiredManifest, ManifestBindMode, ManifestBindingContext,
    };
    use crate::config_bundle::{build_desired_state_live_bundle, live_manifest_from_bundle};
    use crate::config_import::apply_desired_state_changes;
    use crate::desired_state::{diff_manifests, export_bundle_from_manifest};
    use gents::graphql::escape_graphql_string;

    let tempdir = tempfile::tempdir()?;
    let node = EmbeddedNode::builder()
        .data_path(tempdir.path().join("data"))
        .with_storage_backend(StorageBackend::Lark)
        .build()
        .await?;
    ensure_runtime_schemas(&node).await?;
    let access = ConfigAccess::Local(std::sync::Arc::new(node));
    let peer_id = "aa".repeat(32);
    let peer_did = "did:key:remote";
    let address = format!("{peer_id}@127.0.0.1:4100");
    access
        .execute(&format!(
            r#"mutation {{ create_PeerPairingDesired(input: {{
                    peer_id: "{}",
                    agent_did: "{}",
                    collections: ["AgentRequest"],
                    replicator_addresses: ["{}"],
                    template: "conversation",
                    source: "operator"
                }}) {{ _docID }} }}"#,
            escape_graphql_string(&peer_id),
            escape_graphql_string(peer_did),
            escape_graphql_string(&address),
        ))
        .await?;

    let mut manifest = manifest_with_subagent_targets(Vec::new());
    manifest.peer_pairings.push(DesiredPeerPairing {
        peer_did: peer_did.to_string(),
        addresses: vec![address],
        template: "conversation".to_string(),
        enabled: false,
        peer_id,
    });
    let errors = validate_manifest_against_live(&manifest, &access).await?;
    assert!(errors.iter().any(|error| {
        error.contains("source \"operator\"") && error.contains("refusing to overwrite or delete")
    }));

    let owner_did = manifest.agent_principal.agent_did.clone();
    let bound = BoundDesiredManifest {
        context: ManifestBindingContext {
            bind_mode: ManifestBindMode::Manifest,
            target_agent_did: owner_did.clone(),
            source_manifest_dids: std::collections::BTreeSet::from([owner_did]),
        },
        manifest: manifest.clone(),
    };
    let report = crate::commands::config::diff::diff_bound_desired_manifest(
        std::path::Path::new("/ownership-collision"),
        &access,
        &bound,
    )
    .await?;
    assert_eq!(report.status, "diffed");
    assert!(!report.ok);
    assert!(report.live_validation_errors.iter().any(|error| {
        error.contains("source \"operator\"") && error.contains("refusing to overwrite or delete")
    }));

    manifest.peer_pairings.clear();
    manifest.tool_selections.clear();
    let live_bundle = build_desired_state_live_bundle(&access, &manifest).await?;
    let (live_principal, live_manifest) = live_manifest_from_bundle(&manifest, &live_bundle)?;
    let planned = diff_manifests(
        std::path::Path::new("/ownership-safe"),
        access.mode(),
        &manifest,
        live_principal.as_ref(),
        &live_manifest,
        false,
    );
    assert!(planned.collections.peer_pairings.delete.is_empty());
    let bundle = export_bundle_from_manifest(&manifest, access.mode())?;
    let txn = access.begin_apply_txn().await?;
    apply_desired_state_changes(&txn, &bundle, &planned).await?;
    txn.commit().await?;
    let rows = crate::graphql_rows(
        &access,
        "PeerPairingDesired",
        "{ PeerPairingDesired { peer_id source } }",
    )
    .await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["source"], "operator");
    Ok(())
}
