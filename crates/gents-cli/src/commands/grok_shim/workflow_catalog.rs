//! `x.ai/workflows/list`: the installed job plugins of the bound agent's Tools.
//! The plugin is the workflow, so its installed record is its catalog entry.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;
use gents::documents::{load_agent_tools_in_txn, PluginToolRef};
use gents::plugin::executor::PluginExecutor;
use gents::plugin::store::InstalledPlugin;
use serde::Serialize;
use serde_json::{json, Value};

/// One row of the pager's Workflows tab: an installed job plugin's name, its
/// manifest description, and the first paragraph of its TOOL.md (or the
/// description again) as `when_to_use`. The pager reads the reply at
/// `/result/workflows` and decodes five snake_case fields per row.
#[derive(Serialize)]
struct WorkflowInfo {
    name: String,
    description: String,
    when_to_use: String,
    source: &'static str,
    path: Option<String>,
}

fn workflow_info(record: &InstalledPlugin) -> WorkflowInfo {
    let description = record.declaration.description.clone();
    let when_to_use = record
        .instructions
        .as_deref()
        .and_then(first_paragraph)
        .unwrap_or_else(|| description.clone());
    WorkflowInfo {
        name: record.name.clone(),
        description,
        when_to_use,
        source: "bundled",
        path: None,
    }
}

/// The first paragraph of `text` that is not a heading, its lines joined by
/// one space.
fn first_paragraph(text: &str) -> Option<String> {
    text.split("\n\n")
        .map(str::trim)
        .find(|paragraph| !paragraph.is_empty() && !paragraph.starts_with('#'))
        .map(|paragraph| paragraph.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The `{"result": {"workflows": [...]}}` reply: one row per plugin in the agent's Tools
/// whose installed record declares `job: true`. A plugin that is not
/// installed, is pinned to another artifact or runs no jobs is skipped.
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
    let workflows: Vec<WorkflowInfo> = plugins
        .iter()
        .filter_map(|plugin_ref| {
            executor
                .resolve(&plugin_ref.plugin, plugin_ref.digest.as_deref())
                .ok()
        })
        .filter(|record| record.declaration.job.0)
        .map(|record| workflow_info(&record))
        .collect();
    Ok(json!({ "result": { "workflows": workflows } }))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use defra_node::EmbeddedNode;
    use gents::documents::{AgentContext, IntegrationTools, PluginToolRef, Tools};
    use serde_json::{json, Value};

    use super::*;

    fn install(home: &Path, name: &str, job: bool, instructions: Option<&str>) {
        let mut record = json!({
            "namespace": "team", "name": name, "version": "0.1.0",
            "digest": format!("sha256:{}", "0".repeat(64)),
            "language": "rust", "declaration": {
                "name": name, "description": format!("{name} description"), "language": "rust",
                "artifact": "plugins/fixture.afb", "input_schema": {"type": "object"}, "job": job,
            },
        });
        if let Some(instructions) = instructions {
            record["instructions"] = instructions.into();
        }
        gents::plugin::store::write_record(home, &serde_json::from_value(record).unwrap()).unwrap();
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
        gents::store::schema::ensure_runtime_schemas(node.as_ref())
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
                    use gents::store::config_client::{
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

    #[test]
    fn first_paragraph_skips_headings_and_joins_lines() {
        assert_eq!(
            first_paragraph("# review\n\nReview a diff\nthrough four lenses.\n\nMore."),
            Some("Review a diff through four lenses.".into())
        );
        assert_eq!(first_paragraph("# only a heading\n\n"), None);
        assert_eq!(first_paragraph(""), None);
    }

    #[tokio::test]
    async fn no_job_plugin_returns_empty_list() {
        let empty = json!({"result": {"workflows": Vec::<Value>::new()}});

        let home = tempfile::tempdir().unwrap();
        install(
            home.path(),
            "plain",
            false,
            Some("# plain\n\nNot a workflow."),
        );
        let (_dir, node, did, agent) =
            node_with_agent(Some(&["team/plain", "team/not-installed"])).await;
        assert_eq!(list(&node, &did, &agent, home.path()).await.unwrap(), empty);

        let (_dir, node, did, agent) = node_with_agent(None).await;
        assert_eq!(list(&node, &did, &agent, home.path()).await.unwrap(), empty);
    }

    #[tokio::test]
    async fn job_plugins_list_from_their_installed_records() {
        let home = tempfile::tempdir().unwrap();
        install(
            home.path(),
            "review",
            true,
            Some("# review\n\nReview a diff\nthrough four lenses.\n\n## Input\n\n- args"),
        );
        install(home.path(), "terse", true, None);
        let (_dir, node, did, agent) =
            node_with_agent(Some(&["team/review", "team/terse", "team/not-installed"])).await;
        assert_eq!(
            list(&node, &did, &agent, home.path()).await.unwrap(),
            json!({"result": {"workflows": [
                {
                    "name": "review",
                    "description": "review description",
                    "when_to_use": "Review a diff through four lenses.",
                    "source": "bundled",
                    "path": null,
                },
                {
                    "name": "terse",
                    "description": "terse description",
                    "when_to_use": "terse description",
                    "source": "bundled",
                    "path": null,
                },
            ]}})
        );
    }
}
