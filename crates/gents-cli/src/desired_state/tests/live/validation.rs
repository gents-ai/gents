use anyhow::Result;
use gents::defra_node::{EmbeddedNode, StorageBackend};
use gents::{config_client::ConfigAccess, document_config::PackConfig, ensure_runtime_schemas};
use serde_json::json;
use crate::desired_state::validate::validate_manifest_against_live;

async fn access() -> Result<(tempfile::TempDir, ConfigAccess)> {
    let dir = tempfile::tempdir()?;
    let node = EmbeddedNode::builder().data_path(dir.path().join("data"))
        .with_storage_backend(StorageBackend::Regolith).build().await?;
    ensure_runtime_schemas(&node).await?;
    Ok((dir, ConfigAccess::Local(std::sync::Arc::new(node))))
}

#[tokio::test]
async fn live_validate_rejects_invalid_event_source_collection_identifier() -> Result<()> {
    let (_dir, access) = access().await?;
    let manifest: PackConfig = serde_json::from_value(json!({
        "agent_principal": {"agent_did": "owner"},
        "event_sources": [{"agent_did": "owner", "event_source_id": "malformed-source",
            "source_collection": "AgentMessage) { _docID } mutation {", "filter": "{}"}]
    }))?;
    let errors = validate_manifest_against_live(&manifest, &access).await?;
    assert!(errors.iter().any(|error| error.contains("malformed-source")
        && error.contains("invalid source_collection") && error.contains("invalid identifier")), "{errors:?}");
    Ok(())
}

#[tokio::test]
async fn schema_validation_leaves_delegation_resolution_to_its_owner() -> Result<()> {
    let (_dir, access) = access().await?;
    for destination in ["owner", "remote"] {
        let manifest: PackConfig = serde_json::from_value(json!({
            "agent_principal": {"agent_did": "owner"},
            "subagent_targets": [{"agent_did": "owner", "target_id": "research",
                "target_agent_did": destination, "behavior_id": "research", "name": "research"}]
        }))?;
        assert!(validate_manifest_against_live(&manifest, &access).await?.is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn event_source_validation_checks_group_fields_and_each_bound_task() -> Result<()> {
    let (_dir, access) = access().await?;
    let manifest: PackConfig = serde_json::from_value(json!({
        "agent_principal": {"agent_did": "owner"},
        "event_sources": [{"agent_did": "owner", "event_source_id": "source",
            "source_collection": "AgentMessage", "correlation_field": "absent_correlation",
            "group": {"expected_count": {"source_field": "absent_count"}}}],
        "triggers": [{"agent_did": "owner", "trigger_id": "trigger", "task_id": "task",
            "source": {"kind": "event", "event_source_id": "source"}}],
        "tasks": [{"agent_did": "owner", "task_id": "task", "behavior_id": "behavior",
            "prompt_template": "{{ doc.absent_template }}"}]
    }))?;
    let errors = validate_manifest_against_live(&manifest, &access).await?;
    for field in ["absent_correlation", "absent_count", "absent_template"] {
        assert!(errors.iter().any(|error| error.contains(field)), "missing {field}: {errors:?}");
    }
    Ok(())
}
