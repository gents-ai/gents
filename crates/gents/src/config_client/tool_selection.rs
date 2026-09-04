use crate::graphql::escape_graphql_string;
use crate::ToolSelectionDocument;
use anyhow::{Context, Result};

use super::{mint_recreate_identity_timestamp, ConfigAccess, ConfigApplyTxn};
use gents_protocol::graphql::{
    graphql_rows_from_response, optional_bool_field, optional_string_field, string_list_field,
};

pub async fn write_tool_selection_document(
    access: &ConfigAccess,
    selection: &ToolSelectionDocument,
) -> Result<String> {
    write_tool_selection_document_with_clear_fields(access, selection, &[]).await
}

pub async fn write_tool_selection_document_with_clear_fields(
    access: &ConfigAccess,
    selection: &ToolSelectionDocument,
    clear_update_fields: &[&str],
) -> Result<String> {
    const CLEARABLE_FIELDS: &[&str] = &[
        "bash_mode",
        "command_execution_policy",
        "command_network_mode",
        "cross_deployment_spawn_timeout_seconds",
        "display_name",
        "file_tool_root",
        "file_tools_mode",
    ];
    for field in clear_update_fields {
        if !CLEARABLE_FIELDS.contains(field) {
            anyhow::bail!("unsupported ToolSelection clear field {field:?}");
        }
    }
    let txn = access.begin_apply_txn().await?;
    let result = write_tool_selection_in_txn(&txn, selection, clear_update_fields).await;
    match result {
        Ok(doc_id) => {
            txn.commit().await?;
            Ok(doc_id)
        }
        Err(error) => {
            let _ = txn.discard().await;
            Err(error)
        }
    }
}

async fn write_tool_selection_in_txn(
    txn: &ConfigApplyTxn<'_>,
    selection: &ToolSelectionDocument,
    clear_update_fields: &[&str],
) -> Result<String> {
    effective_tool_selection(txn, selection, &["write_tools"], clear_update_fields)
        .await?
        .validate()?;
    let add_fields = format!(
        "{},\n                    updated_at: \"{}\"",
        tool_selection_fields(selection, true),
        escape_graphql_string(&mint_recreate_identity_timestamp()),
    );
    let mut update_fields = tool_selection_fields(selection, false);
    if !clear_update_fields.is_empty() {
        if !update_fields.is_empty() {
            update_fields.push_str(",\n                    ");
        }
        update_fields.push_str(
            &clear_update_fields
                .iter()
                .map(|field| format!("{field}: null"))
                .collect::<Vec<_>>()
                .join(",\n                    "),
        );
    }
    let mutation = format!(
        r#"mutation {{
            upsert_ToolSelection(
                filter: {{ selection_id: {{ _eq: "{selection_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        selection_id = escape_graphql_string(&selection.selection_id),
        add_fields = add_fields,
        update_fields = update_fields,
    );
    let response = txn.execute(&mutation).await?;
    gents_protocol::graphql::extract_mutation_doc_id(&response, "ToolSelection")
}

pub(crate) async fn effective_tool_selection(
    txn: &ConfigApplyTxn<'_>,
    patch: &ToolSelectionDocument,
    ignored_fields: &[&str],
    clear_fields: &[&str],
) -> Result<ToolSelectionDocument> {
    let existing = load_tool_selection_in_txn(txn, &patch.selection_id).await?;
    super::common::merge_sparse_document(existing, patch, ignored_fields, clear_fields)
}

pub(crate) async fn load_tool_selection_in_txn(
    txn: &ConfigApplyTxn<'_>,
    selection_id: &str,
) -> Result<Option<ToolSelectionDocument>> {
    let selection_id = escape_graphql_string(selection_id);
    let query = format!(
        r#"{{
            ToolSelection(
                filter: {{ selection_id: {{ _eq: "{selection_id}" }} }},
                limit: 1
            ) {{
                selection_id agent_did display_name tool_policy_version enable_file_tools
                file_tools_mode file_tool_root enable_bash bash_mode command_execution_policy
                command_allowed_argv_prefixes command_forbidden_argv_prefixes
                read_only_command_allowlist command_network_mode cli_tool_names enable_meta_tools
                enable_goal_tools enable_goal_creation allowed_mcp_service_ids
                required_mcp_service_ids backgroundable_tool_names approval_required_tools
                enable_memory enable_session_history_tool enable_context_budget enable_defra_query
                defra_query_collections subagent_targets subagent_spawn_enabled
                subagent_steering_enabled subagent_background_enabled subagent_default_await_mode
                subagent_allow_cross_deployment cross_deployment_spawn_timeout_seconds
                write_tools datastore_tool_surface_ids eth_tool_ids enable_self_config
                self_config_categories self_config_no_lockout self_config_dry_run enable_lsp lsp_config
            }}
        }}"#
    );
    let response = txn.execute(&query).await?;
    graphql_rows_from_response(&response, "ToolSelection")
        .into_iter()
        .next()
        .map(serde_json::from_value)
        .transpose()
        .context("decoding existing ToolSelection")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ensure_runtime_schemas, load_tool_selection, WriteToolDecl};
    use anyhow::Result;
    use defra_node::{EmbeddedNode, StorageBackend};

    #[tokio::test]
    async fn create_validates_the_write_tools_the_writer_actually_persists() -> Result<()> {
        let node = std::sync::Arc::new(EmbeddedNode::builder().build().await?);
        ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::Local(node.clone());
        let selection = ToolSelectionDocument {
            selection_id: "imperative-create".to_string(),
            agent_did: "did:test:owner".to_string(),
            write_tools: Some(vec![WriteToolDecl {
                tool_name: "".to_string(),
                collection: "Invalid collection".to_string(),
                description: String::new(),
                fields: Vec::new(),
                output_obligation: None,
            }]),
            ..Default::default()
        };

        write_tool_selection_document(&access, &selection).await?;
        let stored = load_tool_selection(&node, &selection.selection_id)
            .await?
            .expect("created selection");
        assert!(stored.write_tools.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn clear_fields_reject_unknown_graphql_names_before_access() {
        let access = ConfigAccess::Graphql("not-an-endpoint".to_string());
        let selection = ToolSelectionDocument {
            selection_id: "selection".to_string(),
            agent_did: "did:test:owner".to_string(),
            ..Default::default()
        };
        let error = write_tool_selection_document_with_clear_fields(
            &access,
            &selection,
            &["display_name } mutation { delete_AgentRequest"],
        )
        .await
        .expect_err("unknown clear field must be rejected")
        .to_string();
        assert!(error.contains("unsupported ToolSelection clear field"));
    }

    /// Round-trip test: write a `ToolSelectionDocument` with subagent
    /// enablement fields set, then read it back and assert every field persisted.
    ///
    /// This test will FAIL before the fix because `tool_selection_fields()` does
    /// not emit the subagent fields.
    #[tokio::test]
    async fn write_tool_selection_persists_subagent_enablement_fields() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let data_dir = tempdir.path().join("data");
        let node = EmbeddedNode::builder()
            .data_path(&data_dir)
            .with_storage_backend(StorageBackend::Regolith)
            .build()
            .await?;
        ensure_runtime_schemas(&node).await?;

        // A valid `subagent_targets` entry is the JSON serialization of a named
        // SubagentTarget, not a bare behavior id.
        let target = crate::subagent_target_entry(
            "amy-research",
            "did:test:subagent-enablement",
            "amy-research",
            None,
        );
        let selection = ToolSelectionDocument {
            selection_id: "test-subagent-fields".to_string(),
            agent_did: "did:test:subagent-enablement".to_string(),
            subagent_spawn_enabled: Some(true),
            subagent_targets: Some(vec![target.clone()]),
            subagent_steering_enabled: Some(true),
            subagent_background_enabled: Some(true),
            subagent_default_await_mode: Some("background".to_string()),
            subagent_allow_cross_deployment: Some(true),
            cross_deployment_spawn_timeout_seconds: Some(90),
            ..Default::default()
        };

        let access = ConfigAccess::Local(std::sync::Arc::new(node));
        write_tool_selection_document(&access, &selection).await?;

        let node = match &access {
            ConfigAccess::Local(n) => n,
            ConfigAccess::Graphql(_) => unreachable!(),
        };

        let loaded = load_tool_selection(node, "test-subagent-fields")
            .await?
            .expect("ToolSelection should exist after write");

        assert_eq!(
            loaded.subagent_spawn_enabled,
            Some(true),
            "subagent_spawn_enabled must persist"
        );
        assert_eq!(
            loaded.subagent_targets,
            Some(vec![target.clone()]),
            "subagent_targets must persist"
        );
        assert_eq!(
            loaded.subagent_steering_enabled,
            Some(true),
            "subagent_steering_enabled must persist"
        );
        assert_eq!(
            loaded.subagent_background_enabled,
            Some(true),
            "subagent_background_enabled must persist"
        );
        assert_eq!(
            loaded.subagent_default_await_mode.as_deref(),
            Some("background"),
            "subagent_default_await_mode must persist"
        );
        assert_eq!(
            loaded.subagent_allow_cross_deployment,
            Some(true),
            "subagent_allow_cross_deployment must persist"
        );
        assert_eq!(
            loaded.cross_deployment_spawn_timeout_seconds,
            Some(90),
            "cross_deployment_spawn_timeout_seconds must persist"
        );

        Ok(())
    }

    #[tokio::test]
    async fn write_tool_selection_round_trips_nullable_goal_capabilities() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let node = EmbeddedNode::builder()
            .data_path(tempdir.path().join("data"))
            .with_storage_backend(StorageBackend::Regolith)
            .build()
            .await?;
        ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::Local(std::sync::Arc::new(node));

        let explicit = ToolSelectionDocument {
            selection_id: "test-goal-capabilities".to_string(),
            agent_did: "did:test:goal-capabilities".to_string(),
            enable_meta_tools: Some(false),
            enable_goal_tools: Some(true),
            enable_goal_creation: Some(false),
            ..Default::default()
        };
        write_tool_selection_document(&access, &explicit).await?;

        let node = match &access {
            ConfigAccess::Local(node) => node,
            ConfigAccess::Graphql(_) => unreachable!(),
        };
        let loaded = crate::document_config::load_tool_selection(node, "test-goal-capabilities")
            .await?
            .expect("stored selection");
        assert_eq!(loaded.enable_meta_tools, Some(false));
        assert_eq!(loaded.enable_goal_tools, Some(true));
        assert_eq!(loaded.enable_goal_creation, Some(false));

        let preserve = ToolSelectionDocument {
            selection_id: explicit.selection_id.clone(),
            agent_did: explicit.agent_did.clone(),
            display_name: Some("updated".to_string()),
            ..Default::default()
        };
        write_tool_selection_document(&access, &preserve).await?;
        let loaded = crate::document_config::load_tool_selection(node, "test-goal-capabilities")
            .await?
            .expect("updated selection");
        assert_eq!(loaded.enable_goal_tools, Some(true));
        assert_eq!(loaded.enable_goal_creation, Some(false));
        Ok(())
    }

    /// Regression for the `config tools set` clobbering bug: an update that
    /// leaves the subagent enablement fields `None` (as the imperative command
    /// does — it exposes no flags for them) MUST NOT overwrite an existing
    /// apply-managed subagent config. The writer omits `None` fields from the
    /// `update` clause, so DefraDB preserves the stored values.
    #[tokio::test]
    async fn update_with_none_subagent_fields_preserves_existing_config() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let data_dir = tempdir.path().join("data");
        let node = EmbeddedNode::builder()
            .data_path(&data_dir)
            .with_storage_backend(StorageBackend::Regolith)
            .build()
            .await?;
        ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::Local(std::sync::Arc::new(node));

        // A valid `subagent_targets` entry is the JSON serialization of a named
        // SubagentTarget, not a bare behavior id.
        let target =
            crate::subagent_target_entry("amy-research", "did:test:clobber", "amy-research", None);

        // Step 1: an apply-style write enables subagents.
        let applied = ToolSelectionDocument {
            selection_id: "test-clobber".to_string(),
            agent_did: "did:test:clobber".to_string(),
            display_name: Some("Original".to_string()),
            subagent_spawn_enabled: Some(true),
            subagent_targets: Some(vec![target.clone()]),
            subagent_background_enabled: Some(true),
            subagent_default_await_mode: Some("background".to_string()),
            subagent_allow_cross_deployment: Some(true),
            cross_deployment_spawn_timeout_seconds: Some(90),
            ..Default::default()
        };
        write_tool_selection_document(&access, &applied).await?;

        // Step 2: an imperative `tools set`-style update touches only its own
        // fields and leaves every subagent field `None`.
        let imperative = ToolSelectionDocument {
            selection_id: "test-clobber".to_string(),
            agent_did: "did:test:clobber".to_string(),
            display_name: Some("Updated".to_string()),
            subagent_targets: None,
            subagent_spawn_enabled: None,
            subagent_steering_enabled: None,
            subagent_background_enabled: None,
            subagent_default_await_mode: None,
            subagent_allow_cross_deployment: None,
            cross_deployment_spawn_timeout_seconds: None,
            ..Default::default()
        };
        write_tool_selection_document(&access, &imperative).await?;

        let node = match &access {
            ConfigAccess::Local(n) => n,
            ConfigAccess::Graphql(_) => unreachable!(),
        };
        let loaded = load_tool_selection(node, "test-clobber")
            .await?
            .expect("ToolSelection should exist after update");

        // The imperative field changed...
        assert_eq!(
            loaded.display_name.as_deref(),
            Some("Updated"),
            "imperative-owned field should update"
        );
        // ...but the apply-managed subagent config is preserved, not clobbered.
        assert_eq!(
            loaded.subagent_spawn_enabled,
            Some(true),
            "subagent_spawn_enabled must NOT be clobbered by a None update"
        );
        assert_eq!(
            loaded.subagent_targets,
            Some(vec![target.clone()]),
            "subagent_targets must NOT be clobbered by a None update"
        );
        assert_eq!(
            loaded.subagent_background_enabled,
            Some(true),
            "subagent_background_enabled must NOT be clobbered by a None update"
        );
        assert_eq!(
            loaded.subagent_default_await_mode.as_deref(),
            Some("background"),
            "subagent_default_await_mode must NOT be clobbered by a None update"
        );
        assert_eq!(
            loaded.subagent_allow_cross_deployment,
            Some(true),
            "subagent_allow_cross_deployment must NOT be clobbered by a None update"
        );
        assert_eq!(
            loaded.cross_deployment_spawn_timeout_seconds,
            Some(90),
            "cross_deployment_spawn_timeout_seconds must NOT be clobbered by a None update"
        );

        Ok(())
    }

    #[tokio::test]
    async fn update_with_clear_fields_nulls_nullable_config() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let data_dir = tempdir.path().join("data");
        let node = EmbeddedNode::builder()
            .data_path(&data_dir)
            .with_storage_backend(StorageBackend::Regolith)
            .build()
            .await?;
        ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::Local(std::sync::Arc::new(node));

        let applied = ToolSelectionDocument {
            selection_id: "test-clear-fields".to_string(),
            agent_did: "did:test:clear-fields".to_string(),
            display_name: Some("Original".to_string()),
            file_tool_root: Some("/tmp/workspace".to_string()),
            command_execution_policy: Some("read_only".to_string()),
            command_network_mode: Some("disabled".to_string()),
            allowed_mcp_service_ids: Some(vec!["observability".to_string()]),
            backgroundable_tool_names: Some(vec!["bash_unrestricted".to_string()]),
            cli_tool_names: Some(vec!["rg".to_string()]),
            defra_query_collections: Some(vec!["AgentRequest".to_string()]),
            cross_deployment_spawn_timeout_seconds: Some(90),
            ..Default::default()
        };
        write_tool_selection_document(&access, &applied).await?;

        let clear_update = ToolSelectionDocument {
            selection_id: "test-clear-fields".to_string(),
            agent_did: "did:test:clear-fields".to_string(),
            allowed_mcp_service_ids: Some(Vec::new()),
            backgroundable_tool_names: Some(Vec::new()),
            cli_tool_names: Some(Vec::new()),
            defra_query_collections: Some(Vec::new()),
            ..Default::default()
        };
        write_tool_selection_document_with_clear_fields(
            &access,
            &clear_update,
            &[
                "display_name",
                "file_tool_root",
                "command_execution_policy",
                "command_network_mode",
                "cross_deployment_spawn_timeout_seconds",
            ],
        )
        .await?;

        let node = match &access {
            ConfigAccess::Local(n) => n,
            ConfigAccess::Graphql(_) => unreachable!(),
        };
        let loaded = load_tool_selection(node, "test-clear-fields")
            .await?
            .expect("ToolSelection should exist after update");

        assert_eq!(loaded.display_name, None);
        assert_eq!(loaded.file_tool_root, None);
        assert_eq!(loaded.command_execution_policy, None);
        assert_eq!(loaded.command_network_mode, None);
        assert!(loaded
            .allowed_mcp_service_ids
            .unwrap_or_default()
            .is_empty());
        assert!(loaded
            .backgroundable_tool_names
            .unwrap_or_default()
            .is_empty());
        assert!(loaded.cli_tool_names.unwrap_or_default().is_empty());
        assert!(loaded
            .defra_query_collections
            .unwrap_or_default()
            .is_empty());
        assert_eq!(loaded.cross_deployment_spawn_timeout_seconds, None);

        Ok(())
    }

    #[tokio::test]
    async fn sparse_meta_tools_change_validates_preserved_required_services() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let node = EmbeddedNode::builder()
            .data_path(tempdir.path().join("data"))
            .with_storage_backend(StorageBackend::Regolith)
            .build()
            .await?;
        ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::Local(std::sync::Arc::new(node));

        let initial = ToolSelectionDocument {
            selection_id: "required-services".to_string(),
            agent_did: "did:test:required-services".to_string(),
            enable_meta_tools: Some(true),
            allowed_mcp_service_ids: Some(vec!["required".to_string()]),
            required_mcp_service_ids: Some(vec!["required".to_string()]),
            ..Default::default()
        };
        write_tool_selection_document(&access, &initial).await?;

        let patch = ToolSelectionDocument {
            selection_id: initial.selection_id.clone(),
            agent_did: initial.agent_did.clone(),
            enable_meta_tools: Some(false),
            ..Default::default()
        };
        let error = write_tool_selection_document(&access, &patch)
            .await
            .expect_err("preserved required services must keep meta tools enabled")
            .to_string();
        assert!(error.contains("needs enable_meta_tools=true"), "{error}");

        let node = match &access {
            ConfigAccess::Local(node) => node,
            ConfigAccess::Graphql(_) => unreachable!(),
        };
        let stored = load_tool_selection(node, &initial.selection_id)
            .await?
            .expect("stored selection");
        assert_eq!(stored.enable_meta_tools, Some(true));
        Ok(())
    }

    #[tokio::test]
    async fn document_upsert_validates_preserved_required_services() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let node = EmbeddedNode::builder()
            .data_path(tempdir.path().join("data"))
            .with_storage_backend(StorageBackend::Regolith)
            .build()
            .await?;
        ensure_runtime_schemas(&node).await?;
        let initial = ToolSelectionDocument {
            selection_id: "direct-required-services".to_string(),
            agent_did: "did:test:direct-required-services".to_string(),
            enable_meta_tools: Some(true),
            allowed_mcp_service_ids: Some(vec!["required".to_string()]),
            required_mcp_service_ids: Some(vec!["required".to_string()]),
            ..Default::default()
        };
        crate::upsert_tool_selection(&node, &initial).await?;

        let patch = ToolSelectionDocument {
            selection_id: initial.selection_id.clone(),
            agent_did: initial.agent_did.clone(),
            enable_meta_tools: Some(false),
            ..Default::default()
        };
        let error = crate::upsert_tool_selection(&node, &patch)
            .await
            .expect_err("direct upsert must validate preserved required services")
            .to_string();
        assert!(error.contains("needs enable_meta_tools=true"), "{error}");
        let stored = load_tool_selection(&node, &initial.selection_id)
            .await?
            .expect("stored selection");
        assert_eq!(stored.enable_meta_tools, Some(true));
        Ok(())
    }
}

fn tool_selection_fields(selection: &ToolSelectionDocument, include_id: bool) -> String {
    let mut fields = Vec::new();
    if include_id {
        fields.push(format!(
            r#"selection_id: "{}""#,
            escape_graphql_string(&selection.selection_id)
        ));
    }
    fields.push(format!(
        r#"agent_did: "{}""#,
        escape_graphql_string(&selection.agent_did)
    ));
    fields.extend(
        [
            optional_string_field("display_name", selection.display_name.as_deref()),
            // Emitted from the loaded/desired value only; the imperative builder
            // leaves it `None`, so `optional_string_field` omits it and the stored
            // version is preserved on update (the version is backfill-owned, never
            // set by an imperative flag).
            optional_string_field(
                "tool_policy_version",
                selection.tool_policy_version.as_deref(),
            ),
            optional_bool_field("enable_file_tools", selection.enable_file_tools),
            optional_string_field("file_tools_mode", selection.file_tools_mode.as_deref()),
            optional_string_field("file_tool_root", selection.file_tool_root.as_deref()),
            optional_bool_field("enable_bash", selection.enable_bash),
            optional_string_field("bash_mode", selection.bash_mode.as_deref()),
            optional_string_field(
                "command_execution_policy",
                selection.command_execution_policy.as_deref(),
            ),
            selection
                .command_allowed_argv_prefixes
                .as_ref()
                .and_then(|values| string_list_field("command_allowed_argv_prefixes", values)),
            selection
                .command_forbidden_argv_prefixes
                .as_ref()
                .and_then(|values| string_list_field("command_forbidden_argv_prefixes", values)),
            selection
                .read_only_command_allowlist
                .as_ref()
                .and_then(|values| string_list_field("read_only_command_allowlist", values)),
            optional_string_field(
                "command_network_mode",
                selection.command_network_mode.as_deref(),
            ),
            selection
                .cli_tool_names
                .as_ref()
                .and_then(|values| string_list_field("cli_tool_names", values)),
            optional_bool_field("enable_meta_tools", selection.enable_meta_tools),
            optional_bool_field("enable_goal_tools", selection.enable_goal_tools),
            optional_bool_field("enable_goal_creation", selection.enable_goal_creation),
            selection
                .allowed_mcp_service_ids
                .as_ref()
                .and_then(|values| string_list_field("allowed_mcp_service_ids", values)),
            selection
                .required_mcp_service_ids
                .as_ref()
                .and_then(|values| string_list_field("required_mcp_service_ids", values)),
            selection
                .backgroundable_tool_names
                .as_ref()
                .and_then(|values| string_list_field("backgroundable_tool_names", values)),
            selection
                .approval_required_tools
                .as_ref()
                .and_then(|values| string_list_field("approval_required_tools", values)),
            optional_bool_field("enable_memory", selection.enable_memory),
            optional_bool_field(
                "enable_session_history_tool",
                selection.enable_session_history_tool,
            ),
            optional_bool_field("enable_context_budget", selection.enable_context_budget),
            optional_bool_field("enable_defra_query", selection.enable_defra_query),
            selection
                .defra_query_collections
                .as_ref()
                .and_then(|values| string_list_field("defra_query_collections", values)),
            selection
                .subagent_targets
                .as_ref()
                .and_then(|values| string_list_field("subagent_targets", values)),
            optional_bool_field("subagent_spawn_enabled", selection.subagent_spawn_enabled),
            optional_bool_field(
                "subagent_steering_enabled",
                selection.subagent_steering_enabled,
            ),
            optional_bool_field(
                "subagent_background_enabled",
                selection.subagent_background_enabled,
            ),
            optional_string_field(
                "subagent_default_await_mode",
                selection.subagent_default_await_mode.as_deref(),
            ),
            optional_bool_field(
                "subagent_allow_cross_deployment",
                selection.subagent_allow_cross_deployment,
            ),
            selection
                .cross_deployment_spawn_timeout_seconds
                .map(|value| format!("cross_deployment_spawn_timeout_seconds: {value}")),
            optional_bool_field("enable_self_config", selection.enable_self_config),
            selection
                .self_config_categories
                .as_ref()
                .and_then(|values| string_list_field("self_config_categories", values)),
            optional_bool_field("self_config_no_lockout", selection.self_config_no_lockout),
            optional_bool_field("self_config_dry_run", selection.self_config_dry_run),
            // NOTE: `write_tools` is deliberately NOT encoded here. The
            // imperative path always sets `write_tools: None` (it is
            // apply-managed only), so there is nothing to render.
        ]
        .into_iter()
        .flatten(),
    );
    fields.join(",\n                    ")
}
