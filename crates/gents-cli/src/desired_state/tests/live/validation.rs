use super::*;

#[tokio::test]
async fn live_validate_rejects_invalid_event_trigger_collection_identifier() -> Result<()> {
    use super::super::DesiredEventTrigger;

    let tempdir = tempfile::tempdir()?;
    let node = EmbeddedNode::builder()
        .data_path(tempdir.path().join("data"))
        .with_storage_backend(StorageBackend::Regolith)
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
        .with_storage_backend(StorageBackend::Regolith)
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
        .with_storage_backend(StorageBackend::Regolith)
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
