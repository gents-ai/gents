use super::*;

fn backend_entry(backend_id: &str) -> super::super::DesiredInferenceBackend {
    super::super::DesiredInferenceBackend {
        backend_id: backend_id.to_string(),
        name: backend_id.to_string(),
        provider_kind: Default::default(),
        openai_wire_api: None,
        endpoint: "http://127.0.0.1:9990/v1".to_string(),
        api_key: None,
        api_key_env_var: None,
        max_concurrent: 1,
        max_queue_depth: 8,
        enabled: true,
        models: Vec::new(),
    }
}

/// Regression test for #981: a live InferenceBackend absent from the
/// manifest (e.g. after a backend rename) must be reported live_only and
/// deleted by prune, even when no behavior references it.
#[tokio::test]
async fn diff_prune_detects_and_deletes_live_only_inference_backends() -> Result<()> {
    use crate::config_bundle::{build_desired_state_live_bundle, live_manifest_from_bundle};
    use crate::config_import::apply_desired_state_changes;
    use crate::desired_state::{diff_manifests, export_bundle_from_manifest};

    let tempdir = tempfile::tempdir()?;
    let node = EmbeddedNode::builder()
        .data_path(tempdir.path().join("data"))
        .with_storage_backend(StorageBackend::Regolith)
        .build()
        .await?;
    ensure_runtime_schemas(&node).await?;
    let access = ConfigAccess::Local(std::sync::Arc::new(node));

    let mut manifest = manifest_with_subagent_targets(Vec::new());
    manifest.tool_selections.clear();
    manifest.inference_backends = vec![
        backend_entry("openai-sol-high"),
        backend_entry("openai-terra"),
    ];

    let live_bundle = build_desired_state_live_bundle(&access, &manifest).await?;
    let (live_principal, live_manifest) = live_manifest_from_bundle(&manifest, &live_bundle)?;
    let planned = diff_manifests(
        std::path::Path::new("/backend-prune"),
        access.mode(),
        &manifest,
        live_principal.as_ref(),
        &live_manifest,
        false,
    );
    let bundle = export_bundle_from_manifest(&manifest, access.mode())?;
    let txn = access.begin_apply_txn().await?;
    apply_desired_state_changes(&txn, &bundle, &planned).await?;
    txn.commit().await?;

    // Rename openai-sol-high -> openai-sol in the manifest; the live
    // document for the old id is now referenced by nothing.
    manifest.inference_backends = vec![backend_entry("openai-sol"), backend_entry("openai-terra")];

    let live_bundle = build_desired_state_live_bundle(&access, &manifest).await?;
    let (live_principal, live_manifest) = live_manifest_from_bundle(&manifest, &live_bundle)?;
    let drift = diff_manifests(
        std::path::Path::new("/backend-prune"),
        access.mode(),
        &manifest,
        live_principal.as_ref(),
        &live_manifest,
        false,
    );
    assert_eq!(
        drift.collections.inference_backends.live_only,
        vec!["openai-sol-high".to_string()],
        "stale backend must be reported live_only; got: {:?}",
        drift.collections.inference_backends
    );

    let planned = diff_manifests(
        std::path::Path::new("/backend-prune"),
        access.mode(),
        &manifest,
        live_principal.as_ref(),
        &live_manifest,
        true,
    );
    assert_eq!(
        planned.collections.inference_backends.delete,
        vec!["openai-sol-high".to_string()],
        "prune must plan the stale backend for deletion; got: {:?}",
        planned.collections.inference_backends
    );
    let bundle = export_bundle_from_manifest(&manifest, access.mode())?;
    let txn = access.begin_apply_txn().await?;
    apply_desired_state_changes(&txn, &bundle, &planned).await?;
    txn.commit().await?;

    let rows = crate::graphql_rows(
        &access,
        "InferenceBackend",
        "{ InferenceBackend { backend_id } }",
    )
    .await?;
    let mut ids = rows
        .iter()
        .filter_map(|row| row.get("backend_id").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    ids.sort();
    assert_eq!(
        ids,
        vec!["openai-sol".to_string(), "openai-terra".to_string()]
    );
    Ok(())
}

/// InferenceBackend documents are node-global: a backend referenced by
/// another agent's behavior must never be treated as live_only (or
/// pruned) by this agent's manifest, while a backend referenced by no
/// one remains prunable.
#[tokio::test]
async fn prune_spares_backends_referenced_by_other_agents() -> Result<()> {
    use crate::config_bundle::{build_desired_state_live_bundle, live_manifest_from_bundle};
    use crate::config_import::apply_desired_state_changes;
    use crate::desired_state::{diff_manifests, export_bundle_from_manifest};

    let tempdir = tempfile::tempdir()?;
    let node = EmbeddedNode::builder()
        .data_path(tempdir.path().join("data"))
        .with_storage_backend(StorageBackend::Regolith)
        .build()
        .await?;
    ensure_runtime_schemas(&node).await?;
    let access = ConfigAccess::Local(std::sync::Arc::new(node));

    let mut manifest = manifest_with_subagent_targets(Vec::new());
    manifest.tool_selections.clear();
    manifest.inference_backends = vec![backend_entry("openai-sol")];

    let live_bundle = build_desired_state_live_bundle(&access, &manifest).await?;
    let (live_principal, live_manifest) = live_manifest_from_bundle(&manifest, &live_bundle)?;
    let planned = diff_manifests(
        std::path::Path::new("/backend-prune-foreign"),
        access.mode(),
        &manifest,
        live_principal.as_ref(),
        &live_manifest,
        false,
    );
    let bundle = export_bundle_from_manifest(&manifest, access.mode())?;
    let txn = access.begin_apply_txn().await?;
    apply_desired_state_changes(&txn, &bundle, &planned).await?;
    txn.commit().await?;

    access
        .execute(
            r#"mutation { create_InferenceBackend(input: {
                    backend_id: "other-agent-backend",
                    name: "other-agent-backend",
                    provider_kind: "OpenAiCompatible",
                    endpoint: "http://127.0.0.1:9991/v1",
                    max_concurrent: 1,
                    max_queue_depth: 8,
                    enabled: true
                }) { _docID } }"#,
        )
        .await?;
    access
        .execute(
            r#"mutation { create_AgentBehavior(input: {
                    behavior_id: "other-agent-behavior",
                    agent_did: "did:key:some-other-agent",
                    backend_id: "other-agent-backend",
                    enabled: true
                }) { _docID } }"#,
        )
        .await?;
    access
        .execute(
            r#"mutation { create_InferenceBackend(input: {
                    backend_id: "stale-backend",
                    name: "stale-backend",
                    provider_kind: "OpenAiCompatible",
                    endpoint: "http://127.0.0.1:9992/v1",
                    max_concurrent: 1,
                    max_queue_depth: 8,
                    enabled: true
                }) { _docID } }"#,
        )
        .await?;

    let live_bundle = build_desired_state_live_bundle(&access, &manifest).await?;
    let (live_principal, live_manifest) = live_manifest_from_bundle(&manifest, &live_bundle)?;
    let planned = diff_manifests(
        std::path::Path::new("/backend-prune-foreign"),
        access.mode(),
        &manifest,
        live_principal.as_ref(),
        &live_manifest,
        true,
    );
    assert_eq!(
        planned.collections.inference_backends.delete,
        vec!["stale-backend".to_string()],
        "only the unreferenced backend may be planned for deletion; got: {:?}",
        planned.collections.inference_backends
    );
    assert!(
        planned.collections.inference_backends.live_only.is_empty(),
        "the foreign-referenced backend must not appear live_only; got: {:?}",
        planned.collections.inference_backends
    );

    let bundle = export_bundle_from_manifest(&manifest, access.mode())?;
    let txn = access.begin_apply_txn().await?;
    apply_desired_state_changes(&txn, &bundle, &planned).await?;
    txn.commit().await?;

    let rows = crate::graphql_rows(
        &access,
        "InferenceBackend",
        "{ InferenceBackend { backend_id } }",
    )
    .await?;
    let mut ids = rows
        .iter()
        .filter_map(|row| row.get("backend_id").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    ids.sort();
    assert_eq!(
        ids,
        vec!["openai-sol".to_string(), "other-agent-backend".to_string()]
    );
    Ok(())
}
