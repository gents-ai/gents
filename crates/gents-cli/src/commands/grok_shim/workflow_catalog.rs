//! `x.ai/workflows/list`: the workflow catalogs of the bound agent's plugins.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;
use gents::document_config::{load_agent_tools_in_txn, PluginToolRef};
use gents::plugin::data::DataCaller;
use gents::plugin::executor::PluginExecutor;
use gents_protocol::job::{CatalogEntry, CatalogSource};
use serde::Serialize;
use serde_json::{json, Value};

/// One row of the pager's Workflows tab. The key casing and the `source`
/// spellings are unverified until the pager probe confirms them.
#[derive(Serialize)]
struct WorkflowInfo {
    name: String,
    description: String,
    when_to_use: String,
    source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
}

fn workflow_info(entry: CatalogEntry) -> WorkflowInfo {
    WorkflowInfo {
        name: entry.name,
        description: entry.description,
        when_to_use: entry.when_to_use,
        source: match entry.source {
            CatalogSource::Pack => "bundled",
            CatalogSource::Project => "project",
            CatalogSource::Kept => "user",
        },
        path: entry.path,
    }
}

/// The `{"workflows": [...]}` result: the catalog of every plugin in the
/// agent's Tools whose manifest declares one. The shim runs beside the node,
/// so the catalog's local-only requirement holds. A plugin that is not
/// installed, is pinned to another artifact or declares no catalog is
/// skipped; a catalog call that fails fails the list.
pub(super) async fn list(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
    plugin_home: &Path,
) -> Result<Value> {
    let tools =
        gents::ConfigAccess::transact_local_readonly(node, None, "grok.workflows_list", |txn| {
            Box::pin(async move { load_agent_tools_in_txn(txn, node_did, agent_id).await })
        })
        .await?;
    let plugins: Vec<PluginToolRef> = tools
        .and_then(|tools| tools.integrations)
        .and_then(|integrations| integrations.plugins)
        .unwrap_or_default();
    let executor = PluginExecutor::new(Some(plugin_home.to_owned()));
    let caller = DataCaller::new(node.clone(), node_did)?;
    let mut workflows = Vec::new();
    for plugin_ref in plugins {
        let declares_catalog = executor
            .resolve(&plugin_ref.plugin, plugin_ref.digest.as_deref())
            .is_ok_and(|record| record.declaration.catalog.is_some());
        if declares_catalog {
            let entries = gents::plugin::catalog(&executor, &plugin_ref, &caller).await?;
            workflows.extend(entries.into_iter().map(workflow_info));
        }
    }
    Ok(json!({ "workflows": workflows }))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use defra_node::EmbeddedNode;
    use gents::document_config::{AgentContext, IntegrationTools, PluginToolRef, Tools};
    use gents_protocol::job::{CatalogEntry, CatalogSource};
    use serde_json::{json, Value};

    use super::*;

    #[test]
    fn catalog_entry_maps_to_workflow_info() {
        for source in [
            CatalogSource::Pack,
            CatalogSource::Project,
            CatalogSource::Kept,
        ] {
            let spelling = match source {
                CatalogSource::Pack => "bundled",
                CatalogSource::Project => "project",
                CatalogSource::Kept => "user",
            };
            let info = workflow_info(CatalogEntry {
                name: "triage".into(),
                description: "Sort issues".into(),
                when_to_use: "new issues".into(),
                source,
                path: Some("workflows/triage.rhai".into()),
            });
            assert_eq!(
                serde_json::to_value(info).unwrap(),
                json!({
                    "name": "triage",
                    "description": "Sort issues",
                    "when_to_use": "new issues",
                    "source": spelling,
                    "path": "workflows/triage.rhai",
                })
            );
        }
    }

    fn install(home: &Path, name: &str, catalog: bool) {
        let mut declaration = json!({
            "name": name, "description": "fixture", "language": "rust",
            "artifact": "plugins/fixture.afb", "input_schema": {"type": "object"}
        });
        if catalog {
            declaration["catalog"] = json!({"argv": ["catalog"]});
        }
        let record = serde_json::from_value(json!({
            "namespace": "team", "name": name, "version": "0.1.0",
            "digest": format!("sha256:{}", "0".repeat(64)),
            "language": "rust", "declaration": declaration,
        }))
        .unwrap();
        gents::plugin::store::write_record(home, &record).unwrap();
    }

    /// A node with one agent; `plugins` gives that agent a Tools document
    /// offering those plugin coordinates, and `None` gives it no Tools.
    async fn node_with_agent(
        plugins: Option<&[&str]>,
    ) -> (tempfile::TempDir, Arc<EmbeddedNode>, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let identity = gents::KeyIdentity::load_or_create(dir.path().join("actor.key"), None)
            .expect("test signing identity");
        let node_did = gents::NodeIdentity::did(&identity).to_owned();
        let agent_id = "catalog-agent".to_owned();
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(&node_did)
                .data_path(dir.path().join("node"))
                .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
                .build()
                .await
                .expect("embedded node"),
        );
        gents::schema::ensure_runtime_schemas(node.as_ref())
            .await
            .expect("runtime schemas");
        super::super::seed_test_agent_configuration(
            &node,
            &node_did,
            &agent_id,
            &agent_id,
            "catalog-model",
            true,
        )
        .await;
        if let Some(plugins) = plugins {
            let refs: Vec<PluginToolRef> = plugins
                .iter()
                .map(|plugin| serde_json::from_value(json!({"plugin": plugin})).unwrap())
                .collect();
            let (did, agent) = (node_did.clone(), agent_id.clone());
            gents::ConfigAccess::transact_local_write(&node, None, "catalog.fixture", |txn| {
                let (did, agent, refs) = (did.clone(), agent.clone(), refs.clone());
                Box::pin(async move {
                    use gents::config_client::{
                        apply_desired_state_plan, read_desired_state_record_in_txn as read,
                        DesiredStateApplyDocument, DesiredStateApplyPlan,
                    };
                    use gents::Collection;
                    let (_, mut row) = read(txn, Collection::Agent, &did, &agent)
                        .await?
                        .expect("fixture agent");
                    row["context_id"] = "catalog-context".into();
                    let tools = Tools {
                        tools_id: "catalog-tools".into(),
                        node_did: did.clone(),
                        integrations: Some(IntegrationTools {
                            plugins: Some(refs),
                            ..Default::default()
                        }),
                        ..Default::default()
                    };
                    let context = AgentContext {
                        context_id: "catalog-context".into(),
                        node_did: did.clone(),
                        display_name: None,
                        description: None,
                        system_prompt: None,
                        tools_id: Some("catalog-tools".into()),
                        compaction_id: None,
                        skill_ids: Vec::new(),
                        tags: Vec::new(),
                    };
                    let docs = [
                        (Collection::Tools, serde_json::to_value(tools)?),
                        (Collection::AgentContext, serde_json::to_value(context)?),
                        (Collection::Agent, row),
                    ];
                    let plan = DesiredStateApplyPlan::new(
                        docs.into_iter()
                            .map(|(collection, value)| DesiredStateApplyDocument {
                                collection,
                                add: value.clone(),
                                update: value,
                            })
                            .collect(),
                    )?;
                    apply_desired_state_plan(txn, &plan).await.map(|_| ())
                })
            })
            .await
            .unwrap();
        }
        (dir, node, node_did, agent_id)
    }

    #[tokio::test]
    async fn no_catalog_plugin_returns_empty_list() {
        let empty = json!({"workflows": Vec::<Value>::new()});

        let home = tempfile::tempdir().unwrap();
        install(home.path(), "plain", false);
        let (_dir, node, did, agent) =
            node_with_agent(Some(&["team/plain", "team/not-installed"])).await;
        assert_eq!(list(&node, &did, &agent, home.path()).await.unwrap(), empty);

        let (_dir, node, did, agent) = node_with_agent(None).await;
        assert_eq!(list(&node, &did, &agent, home.path()).await.unwrap(), empty);
    }

    #[tokio::test]
    async fn catalog_failure_returns_shaped_internal_error() {
        let home = tempfile::tempdir().unwrap();
        install(home.path(), "broken", true);
        let (_dir, node, did, agent) = node_with_agent(Some(&["team/broken"])).await;
        let error = list(&node, &did, &agent, home.path()).await.unwrap_err();
        assert!(
            format!("{error:#}").contains("reading the installed plugin bytes"),
            "{error:#}"
        );
    }
}
