use super::*;

#[tokio::test]
async fn all_subagent_fields_persist_and_apply_is_idempotent() -> Result<()> {
    use std::path::PathBuf;

    use crate::config_bundle::{build_desired_state_live_bundle, live_manifest_from_bundle};
    use crate::config_import::{apply_desired_state_changes, diff_has_pending_apply};
    use crate::desired_state::{diff_manifests, export_bundle_from_manifest};

    let tempdir = tempfile::tempdir()?;
    let data_dir = tempdir.path().join("data");
    let node = EmbeddedNode::builder()
        .data_path(&data_dir)
        .with_storage_backend(StorageBackend::Lark)
        .build()
        .await?;
    ensure_runtime_schemas(&node).await?;

    let access = ConfigAccess::Local(std::sync::Arc::new(node));

    {
        use gents::graphql::escape_graphql_string;
        let did = escape_graphql_string("did:key:test-subagent-idempotency");
        access
                .execute(&format!(
                    r#"mutation {{ create_AgentPrincipal(input: {{ agent_did: "{did}", enabled: true }}) {{ _docID }} }}"#
                ))
                .await?;
    }

    let desired_manifest = {
        use super::super::{DesiredAgentPrincipal, DesiredStateManifest, DesiredToolSelection};
        DesiredStateManifest {
            agent_principal: DesiredAgentPrincipal {
                agent_did: "did:key:test-subagent-idempotency".to_string(),
                display_name: None,
                default_behavior_id: None,
                enabled: true,
            },
            agent_behaviors: Vec::new(),
            skills: Vec::new(),
            datastore_tool_surfaces: Vec::new(),
            chain_key_bindings: Vec::new(),
            eth_tools: Vec::new(),
            tool_selections: vec![DesiredToolSelection {
                selection_id: "subagent-idempotency-sel".to_string(),
                agent_did: "did:key:test-subagent-idempotency".to_string(),
                display_name: None,
                tool_policy_version: None,
                enable_file_tools: false,
                file_tools_mode: "ReadOnly".to_string(),
                file_tool_root: None,
                enable_bash: false,
                bash_mode: "ReadOnly".to_string(),
                command_execution_policy: None,
                command_allowed_argv_prefixes: Vec::new(),
                command_forbidden_argv_prefixes: Vec::new(),
                read_only_command_allowlist: Vec::new(),
                command_network_mode: None,
                cli_tool_names: Vec::new(),
                enable_meta_tools: false,
                allowed_mcp_service_ids: Vec::new(),
                required_mcp_service_ids: Vec::new(),
                delegate_to: Vec::new(),
                backgroundable_tool_names: Vec::new(),
                enable_memory: false,
                enable_session_history_tool: false,
                enable_context_budget: true,
                enable_defra_query: true,
                defra_query_collections: Vec::new(),
                subagent_targets: vec![SubagentTarget {
                    name: "researcher".to_string(),
                    agent_did: "did:key:test-subagent-idempotency".to_string(),
                    behavior_id: "amy-research".to_string(),
                    description: None,
                }
                .to_entry()],
                subagent_spawn_enabled: true,
                subagent_steering_enabled: true,
                subagent_background_enabled: true,
                subagent_default_await_mode: Some("background".to_string()),
                subagent_allow_cross_deployment: true,
                cross_deployment_spawn_timeout_seconds: Some(90),
                write_tools: Vec::new(),
                datastore_tool_surface_ids: Vec::new(),
                eth_tool_ids: Vec::new(),
                enable_self_config: false,
                self_config_categories: Vec::new(),
                self_config_no_lockout: false,
                self_config_dry_run: false,
                enable_lsp: false,
                lsp_config: None,
            }],
            inference_backends: Vec::new(),
            inference_profiles: Vec::new(),
            tool_service_registries: Vec::new(),
            projection_acp_bindings: Vec::new(),
            tasks: Vec::new(),
            schedules: Vec::new(),
            event_triggers: Vec::new(),
            callback_bindings: Vec::new(),
            repository_placements: Vec::new(),
        }
    };

    let root = PathBuf::from(".");
    let desired_bundle = export_bundle_from_manifest(&desired_manifest, "local")?;

    let live_bundle = build_desired_state_live_bundle(&access, &desired_manifest).await?;
    let (live_principal, live_manifest) =
        live_manifest_from_bundle(&desired_manifest, &live_bundle)?;
    let planned = diff_manifests(
        &root,
        "local",
        &desired_manifest,
        live_principal.as_ref(),
        &live_manifest,
        false,
    );

    let txn = access.begin_apply_txn().await?;
    apply_desired_state_changes(&txn, &desired_bundle, &planned).await?;
    txn.commit().await?;

    let remaining_bundle = build_desired_state_live_bundle(&access, &desired_manifest).await?;
    let (remaining_principal, remaining_manifest) =
        live_manifest_from_bundle(&desired_manifest, &remaining_bundle)?;

    let live_sel = remaining_manifest
        .tool_selections
        .iter()
        .find(|s| s.selection_id == "subagent-idempotency-sel")
        .expect("ToolSelection should exist after apply");

    assert_eq!(
        live_sel.subagent_targets,
        vec![SubagentTarget {
            name: "researcher".to_string(),
            agent_did: "did:key:test-subagent-idempotency".to_string(),
            behavior_id: "amy-research".to_string(),
            description: None,
        }
        .to_entry()],
        "subagent_targets must persist through apply"
    );
    assert_eq!(
        live_sel.subagent_spawn_enabled, true,
        "subagent_spawn_enabled must persist through apply"
    );
    assert_eq!(
        live_sel.subagent_steering_enabled, true,
        "subagent_steering_enabled must persist through apply"
    );
    assert_eq!(
        live_sel.subagent_background_enabled, true,
        "subagent_background_enabled must persist through apply"
    );
    assert_eq!(
        live_sel.subagent_default_await_mode.as_deref(),
        Some("background"),
        "subagent_default_await_mode must persist through apply"
    );
    assert_eq!(
        live_sel.subagent_allow_cross_deployment, true,
        "subagent_allow_cross_deployment must persist through apply"
    );
    assert_eq!(
        live_sel.cross_deployment_spawn_timeout_seconds,
        Some(90),
        "cross_deployment_spawn_timeout_seconds must persist through apply"
    );

    let second_diff = diff_manifests(
        &root,
        "local",
        &desired_manifest,
        remaining_principal.as_ref(),
        &remaining_manifest,
        false,
    );

    assert!(
        !diff_has_pending_apply(&second_diff.counts),
        "second diff must have no pending apply (idempotent); got: {:?}",
        second_diff.counts
    );
    assert!(
        second_diff
            .collections
            .tool_selections
            .unchanged
            .contains(&"subagent-idempotency-sel".to_string()),
        "tool selection must be in the 'unchanged' set after re-apply; got: {:?}",
        second_diff.collections.tool_selections
    );

    Ok(())
}

#[tokio::test]
async fn behavior_description_and_summary_persist_and_apply_is_idempotent() -> Result<()> {
    use std::path::PathBuf;

    use crate::config_bundle::{build_desired_state_live_bundle, live_manifest_from_bundle};
    use crate::config_import::{apply_desired_state_changes, diff_has_pending_apply};
    use crate::desired_state::{diff_manifests, export_bundle_from_manifest};

    let tempdir = tempfile::tempdir()?;
    let data_dir = tempdir.path().join("data");
    let node = EmbeddedNode::builder()
        .data_path(&data_dir)
        .with_storage_backend(StorageBackend::Lark)
        .build()
        .await?;
    ensure_runtime_schemas(&node).await?;

    let access = ConfigAccess::Local(std::sync::Arc::new(node));

    {
        use gents::graphql::escape_graphql_string;
        let did = escape_graphql_string("did:key:test-behavior-desc-idempotency");
        access
                .execute(&format!(
                    r#"mutation {{ create_AgentPrincipal(input: {{ agent_did: "{did}", enabled: true }}) {{ _docID }} }}"#
                ))
                .await?;
    }

    let desired_manifest = {
        use super::super::{DesiredAgentBehavior, DesiredAgentPrincipal, DesiredStateManifest};
        DesiredStateManifest {
            agent_principal: DesiredAgentPrincipal {
                agent_did: "did:key:test-behavior-desc-idempotency".to_string(),
                display_name: None,
                default_behavior_id: None,
                enabled: true,
            },
            agent_behaviors: vec![DesiredAgentBehavior {
                behavior_id: "desc-idempotency-behavior".to_string(),
                agent_did: "did:key:test-behavior-desc-idempotency".to_string(),
                display_name: Some("Research Assistant".to_string()),
                description: Some(
                    "A general-purpose assistant for research and writing tasks.".to_string(),
                ),
                summary: Some("Research assistant".to_string()),
                system_prompt: None,
                request_context_template: None,
                backend_id: None,
                model_name: None,
                tool_selection_id: None,
                inference_profile_id: None,
                compaction_strategy: None,
                compaction_threshold: None,
                enabled: true,
                skill_refs: Vec::new(),
                skill_excludes: Vec::new(),
            }],
            skills: Vec::new(),
            datastore_tool_surfaces: Vec::new(),
            chain_key_bindings: Vec::new(),
            eth_tools: Vec::new(),
            tool_selections: Vec::new(),
            inference_backends: Vec::new(),
            inference_profiles: Vec::new(),
            tool_service_registries: Vec::new(),
            projection_acp_bindings: Vec::new(),
            tasks: Vec::new(),
            schedules: Vec::new(),
            event_triggers: Vec::new(),
            callback_bindings: Vec::new(),
            repository_placements: Vec::new(),
        }
    };

    let root = PathBuf::from(".");
    let desired_bundle = export_bundle_from_manifest(&desired_manifest, "local")?;

    let live_bundle = build_desired_state_live_bundle(&access, &desired_manifest).await?;
    let (live_principal, live_manifest) =
        live_manifest_from_bundle(&desired_manifest, &live_bundle)?;
    let planned = diff_manifests(
        &root,
        "local",
        &desired_manifest,
        live_principal.as_ref(),
        &live_manifest,
        false,
    );

    let txn = access.begin_apply_txn().await?;
    apply_desired_state_changes(&txn, &desired_bundle, &planned).await?;
    txn.commit().await?;

    let remaining_bundle = build_desired_state_live_bundle(&access, &desired_manifest).await?;
    let (remaining_principal, remaining_manifest) =
        live_manifest_from_bundle(&desired_manifest, &remaining_bundle)?;

    let live_behavior = remaining_manifest
        .agent_behaviors
        .iter()
        .find(|b| b.behavior_id == "desc-idempotency-behavior")
        .expect("AgentBehavior should exist after apply");

    assert_eq!(
        live_behavior.description,
        Some("A general-purpose assistant for research and writing tasks.".to_string()),
        "description must persist through apply"
    );
    assert_eq!(
        live_behavior.summary,
        Some("Research assistant".to_string()),
        "summary must persist through apply"
    );

    let second_diff = diff_manifests(
        &root,
        "local",
        &desired_manifest,
        remaining_principal.as_ref(),
        &remaining_manifest,
        false,
    );

    assert!(
        !diff_has_pending_apply(&second_diff.counts),
        "second diff must have no pending apply (idempotent); got: {:?}",
        second_diff.counts
    );
    assert!(
        second_diff
            .collections
            .agent_behaviors
            .unchanged
            .contains(&"desc-idempotency-behavior".to_string()),
        "behavior must be in the 'unchanged' set after re-apply; got: {:?}",
        second_diff.collections.agent_behaviors
    );

    Ok(())
}
