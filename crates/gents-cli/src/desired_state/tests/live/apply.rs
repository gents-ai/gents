use super::*;

#[tokio::test]
async fn preboot_pairing_apply_is_idempotent_and_restart_loader_consumes_seed() -> Result<()> {
    use std::sync::Arc;

    use crate::config_bundle::{build_desired_state_live_bundle, live_manifest_from_bundle};
    use crate::config_import::apply_desired_state_changes;
    use crate::desired_state::{diff_manifests, export_bundle_from_manifest};
    use gents::agent::p2p_reconcile::{
        reconcile_peer_tick, GraphqlPairingStateStore, PairingFilters, PairingStateStore,
        RemoteP2pAdmin, RemoteP2pAdminResult, RemoteReplicator,
    };
    use gents::KeyIdentity;

    let tempdir = tempfile::tempdir()?;
    let data_path = tempdir.path().join("data");
    let node = EmbeddedNode::builder()
        .data_path(&data_path)
        .with_storage_backend(StorageBackend::Lark)
        .build()
        .await?;
    ensure_runtime_schemas(&node).await?;
    let access = ConfigAccess::Local(std::sync::Arc::new(node));
    let peer_id = "bb".repeat(32);
    let address = format!("{peer_id}@127.0.0.1:4100");
    let mut manifest = manifest_with_subagent_targets(Vec::new());
    manifest.tool_selections.clear();
    manifest
        .peer_pairings
        .push(super::super::DesiredPeerPairing {
            peer_did: "did:key:remote".to_string(),
            addresses: vec![address.clone()],
            template: "conversation".to_string(),
            enabled: true,
            peer_id: peer_id.clone(),
        });

    let live_bundle = build_desired_state_live_bundle(&access, &manifest).await?;
    let (live_principal, live_manifest) = live_manifest_from_bundle(&manifest, &live_bundle)?;
    let planned = diff_manifests(
        std::path::Path::new("/preboot"),
        access.mode(),
        &manifest,
        live_principal.as_ref(),
        &live_manifest,
        false,
    );
    assert_eq!(
        planned.collections.peer_pairings.create,
        vec![peer_id.clone()]
    );
    let bundle = export_bundle_from_manifest(&manifest, access.mode())?;
    let txn = access.begin_apply_txn().await?;
    let counts = apply_desired_state_changes(&txn, &bundle, &planned).await?;
    txn.commit().await?;
    assert_eq!(counts.peer_pairings, 1);

    let live_bundle = build_desired_state_live_bundle(&access, &manifest).await?;
    let (live_principal, live_manifest) = live_manifest_from_bundle(&manifest, &live_bundle)?;
    let noop = diff_manifests(
        std::path::Path::new("/preboot"),
        access.mode(),
        &manifest,
        live_principal.as_ref(),
        &live_manifest,
        false,
    );
    assert_eq!(
        noop.collections.peer_pairings.unchanged,
        vec![peer_id.clone()]
    );
    assert!(!noop.counts.has_pending_apply());
    let txn = access.begin_apply_txn().await?;
    let repeated = apply_desired_state_changes(&txn, &bundle, &noop).await?;
    txn.commit().await?;
    assert_eq!(repeated.peer_pairings, 0);
    drop(access);

    let identity = Arc::new(KeyIdentity::load_or_create(
        tempdir.path().join("restart-identity.key"),
        None,
    )?);
    let restarted_node = Arc::new(
        EmbeddedNode::builder()
            .data_path(&data_path)
            .with_storage_backend(StorageBackend::Lark)
            .build()
            .await?,
    );
    let restarted_store = GraphqlPairingStateStore::new(restarted_node.clone(), identity.clone());
    let loaded = restarted_store
        .load_desired(&peer_id)
        .await?
        .expect("seeded pairing is visible to restarted reconciler");
    assert!(loaded.replicator_addresses.contains(&address));

    #[derive(Default)]
    struct RestartAdmin {
        added_replicators: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl RemoteP2pAdmin for RestartAdmin {
        async fn peer_info(&self) -> RemoteP2pAdminResult<Vec<String>> {
            Ok(Vec::new())
        }
        async fn active_peers(&self) -> RemoteP2pAdminResult<Vec<String>> {
            Ok(Vec::new())
        }
        async fn connect(&self, _addresses: &[String]) -> RemoteP2pAdminResult<()> {
            Ok(())
        }
        async fn list_replicators(&self) -> RemoteP2pAdminResult<Vec<RemoteReplicator>> {
            Ok(self
                .added_replicators
                .lock()
                .unwrap()
                .iter()
                .map(|address| RemoteReplicator {
                    id: Some(address.clone()),
                    collections: Vec::new(),
                    address: Some(address.clone()),
                    filters: Some(Default::default()),
                })
                .collect())
        }
        async fn add_replicator(
            &self,
            addresses: &[String],
            _collections: &[String],
            _filters: &PairingFilters,
        ) -> RemoteP2pAdminResult<()> {
            self.added_replicators
                .lock()
                .unwrap()
                .extend_from_slice(addresses);
            Ok(())
        }
        async fn delete_replicator(
            &self,
            id: &str,
            _collections: &[String],
        ) -> RemoteP2pAdminResult<()> {
            self.added_replicators
                .lock()
                .unwrap()
                .retain(|address| address != id);
            Ok(())
        }
        async fn list_p2p_collections(&self) -> RemoteP2pAdminResult<Vec<String>> {
            Ok(Vec::new())
        }
        async fn resolve_collection_id(&self, name: &str) -> RemoteP2pAdminResult<Option<String>> {
            Ok(Some(name.to_string()))
        }
        async fn resolve_collection_name(&self, id: &str) -> RemoteP2pAdminResult<Option<String>> {
            Ok(Some(id.to_string()))
        }
        async fn add_p2p_collections(&self, _collections: &[String]) -> RemoteP2pAdminResult<()> {
            Ok(())
        }
        async fn delete_p2p_collections(
            &self,
            _collections: &[String],
        ) -> RemoteP2pAdminResult<()> {
            Ok(())
        }
        async fn list_p2p_documents(&self) -> RemoteP2pAdminResult<Vec<String>> {
            Ok(Vec::new())
        }
        async fn add_p2p_documents(&self, _doc_ids: &[String]) -> RemoteP2pAdminResult<()> {
            Ok(())
        }
        async fn delete_p2p_documents(&self, _doc_ids: &[String]) -> RemoteP2pAdminResult<()> {
            Ok(())
        }
        async fn sync_documents(
            &self,
            _collection_name: &str,
            _doc_ids: &[String],
            _timeout: Option<std::time::Duration>,
        ) -> RemoteP2pAdminResult<()> {
            Ok(())
        }
        async fn sync_collection_versions(
            &self,
            _version_ids: &[String],
            _timeout: Option<std::time::Duration>,
        ) -> RemoteP2pAdminResult<()> {
            Ok(())
        }
        async fn sync_branchable_collection(
            &self,
            _collection_id: &str,
            _timeout: Option<std::time::Duration>,
        ) -> RemoteP2pAdminResult<()> {
            Ok(())
        }
    }

    let admin = RestartAdmin::default();
    let outcome = reconcile_peer_tick(&admin, &restarted_store, &peer_id).await?;
    assert!(!outcome.ops_applied.is_empty());
    assert_eq!(
        admin.added_replicators.lock().unwrap().as_slice(),
        &[address.clone()]
    );
    drop(restarted_store);
    drop(restarted_node);

    manifest.peer_pairings[0].enabled = false;
    let node = EmbeddedNode::builder()
        .data_path(&data_path)
        .with_storage_backend(StorageBackend::Lark)
        .build()
        .await?;
    let access = ConfigAccess::Local(std::sync::Arc::new(node));
    let live_bundle = build_desired_state_live_bundle(&access, &manifest).await?;
    let (live_principal, live_manifest) = live_manifest_from_bundle(&manifest, &live_bundle)?;
    let removal = diff_manifests(
        std::path::Path::new("/preboot"),
        access.mode(),
        &manifest,
        live_principal.as_ref(),
        &live_manifest,
        false,
    );
    assert_eq!(
        removal.collections.peer_pairings.delete,
        vec![peer_id.clone()]
    );
    let bundle = export_bundle_from_manifest(&manifest, access.mode())?;
    let txn = access.begin_apply_txn().await?;
    apply_desired_state_changes(&txn, &bundle, &removal).await?;
    txn.commit().await?;
    let rows = crate::graphql_rows(
        &access,
        "PeerPairingDesired",
        "{ PeerPairingDesired { peer_id } }",
    )
    .await?;
    assert!(rows.is_empty());
    drop(access);
    let removal_node = Arc::new(
        EmbeddedNode::builder()
            .data_path(&data_path)
            .with_storage_backend(StorageBackend::Lark)
            .build()
            .await?,
    );
    let removal_store = GraphqlPairingStateStore::new(removal_node, identity);
    let outcome = reconcile_peer_tick(&admin, &removal_store, &peer_id).await?;
    assert!(!outcome.ops_applied.is_empty());
    assert!(admin.added_replicators.lock().unwrap().is_empty());
    Ok(())
}

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
            peer_pairings: Vec::new(),
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
            tool_selections: Vec::new(),
            inference_backends: Vec::new(),
            inference_profiles: Vec::new(),
            tool_service_registries: Vec::new(),
            projection_acp_bindings: Vec::new(),
            peer_pairings: Vec::new(),
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
