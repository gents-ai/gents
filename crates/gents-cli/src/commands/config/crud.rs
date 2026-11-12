use anyhow::{Context, Result};
use gents_documents::operations::delete;
use gents_protocol::operations::document::{DocumentDeleteError, OperationActor};
use gents_protocol::operations::inference::InferenceDocument;
use gents_store::collection::Collection;
use serde_json::{json, Value};

use super::inference_failure::InferenceFailure;
use crate::cli::output_format::OutputFormat;
use crate::cli::{ConfigListArgs, ConfigShowArgs};
use crate::config_writes::ConfigAccess;
use crate::request_helpers::resolve_dual_id;
use crate::{print_json, resolve_config_access};

#[derive(Clone, Copy)]
pub(crate) struct ConfigDocumentSpec {
    pub(super) noun: &'static str,
    pub(crate) collection: Collection,
}

pub(super) const BACKEND_SPEC: ConfigDocumentSpec = ConfigDocumentSpec {
    noun: "backend",
    collection: Collection::InferenceBackend,
};

pub(super) const AGENT_SPEC: ConfigDocumentSpec = ConfigDocumentSpec {
    noun: "agent",
    collection: Collection::Agent,
};

pub(crate) const TOOLS_SPEC: ConfigDocumentSpec = ConfigDocumentSpec {
    noun: "tools",
    collection: Collection::Tools,
};

pub(super) const PROFILE_SPEC: ConfigDocumentSpec = ConfigDocumentSpec {
    noun: "profile",
    collection: Collection::InferenceProfile,
};

pub(crate) const TRIGGER_SPEC: ConfigDocumentSpec = ConfigDocumentSpec {
    noun: "trigger",
    collection: Collection::Trigger,
};

pub(crate) const SCHEDULE_SPEC: ConfigDocumentSpec = ConfigDocumentSpec {
    noun: "schedule",
    collection: Collection::Schedule,
};

pub(crate) const MCP_SPEC: ConfigDocumentSpec = ConfigDocumentSpec {
    noun: "mcp",
    collection: Collection::ToolServiceRegistry,
};

pub(crate) const SKILL_SPEC: ConfigDocumentSpec = ConfigDocumentSpec {
    noun: "skill",
    collection: Collection::Skill,
};

pub(crate) const EVENT_SOURCE_SPEC: ConfigDocumentSpec = ConfigDocumentSpec {
    noun: "event source",
    collection: Collection::EventSource,
};

pub(crate) const CONTEXT_SPEC: ConfigDocumentSpec = ConfigDocumentSpec {
    noun: "context",
    collection: Collection::AgentContext,
};

pub(crate) const NODE_SPEC: ConfigDocumentSpec = ConfigDocumentSpec {
    noun: "node",
    collection: Collection::Node,
};

const DOCUMENT_SPECS: [ConfigDocumentSpec; 10] = [
    BACKEND_SPEC,
    AGENT_SPEC,
    TOOLS_SPEC,
    PROFILE_SPEC,
    TRIGGER_SPEC,
    SCHEDULE_SPEC,
    MCP_SPEC,
    SKILL_SPEC,
    EVENT_SOURCE_SPEC,
    CONTEXT_SPEC,
];

pub(super) async fn config_list(spec: ConfigDocumentSpec, args: ConfigListArgs) -> Result<()> {
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref())
        .await
        .with_context(|| format!("resolving access for config {} list", spec.noun))?;
    let node_did = super::binding::resolve_target_node_did(
        None,
        None,
        args.home.as_deref(),
        args.graphql.as_deref(),
        Some(&access),
    )
    .await?;
    let rows = query_collection_sorted(&access, spec, &node_did).await?;

    match args.output.ensure_supported(
        &format!("config {} list", spec.noun),
        &[OutputFormat::Table, OutputFormat::Json],
    )? {
        OutputFormat::Json => print_json(&json!({
            "collection": spec.collection.graphql_type(),
            "count": rows.len(),
            "items": rows,
        })),
        OutputFormat::Table => {
            print_list_table(spec, &rows);
            Ok(())
        }
        _ => unreachable!("ensure_supported restricts config list output formats"),
    }
}

pub(super) async fn config_show(spec: ConfigDocumentSpec, args: ConfigShowArgs) -> Result<()> {
    let id = resolve_config_id(spec, &args)?;
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref())
        .await
        .with_context(|| format!("resolving access for config {} show", spec.noun))?;
    let node_did = super::binding::resolve_target_node_did(
        None,
        None,
        args.home.as_deref(),
        args.graphql.as_deref(),
        Some(&access),
    )
    .await?;
    let row = load_one(&access, spec, &node_did, &id).await?;
    if spec.collection == Collection::InferenceProfile {
        if let Some(warning) =
            super::profile::profile_effort_warning(&access, &node_did, &row).await
        {
            eprintln!("warning: {warning}");
        }
    }

    match args
        .output
        .ensure_supported(&format!("config {} show", spec.noun), &[OutputFormat::Json])?
    {
        OutputFormat::Json => print_json(&row),
        _ => unreachable!("ensure_supported restricts config show output formats"),
    }
}

pub(super) async fn config_rm(spec: ConfigDocumentSpec, args: ConfigShowArgs) -> Result<()> {
    let id = resolve_config_id(spec, &args)?;
    let output = args
        .output
        .ensure_supported(&format!("config {} rm", spec.noun), &[OutputFormat::Json])?;
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref())
        .await
        .with_context(|| format!("resolving access for config {} rm", spec.noun))?;
    let node_did = super::binding::resolve_target_node_did(
        None,
        None,
        args.home.as_deref(),
        args.graphql.as_deref(),
        Some(&access),
    )
    .await?;
    let receipt = delete::delete(
        &access,
        "cli.config.delete",
        &node_did,
        &[(spec.collection, id.clone())],
        OperationActor::User,
        None,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
    .map_err(|error| delete_failure(spec, error))?;
    match output {
        OutputFormat::Json => print_json(&json!({
            "collection": spec.collection.graphql_type(),
            "id": id,
            "deleted": receipt.targets.len(),
        })),
        _ => unreachable!("ensure_supported restricts config rm output formats"),
    }
}

pub(super) async fn inference_rm(document: InferenceDocument, args: ConfigShowArgs) -> Result<()> {
    let noun = document.resource();
    let id = resolve_dual_id(noun, "--id", args.id.as_deref(), args.id_flag.as_deref())?;
    let output = args
        .output
        .ensure_supported(&format!("config {noun} rm"), &[OutputFormat::Json])?;
    let (access, node_did) =
        super::profile::target(args.home.as_deref(), args.graphql.as_deref()).await?;
    let receipt = gents_inference::operations::remove(&access, &node_did, document, &id)
        .await
        .map_err(|error| InferenceFailure::new(error, document, "rm"))?;
    anyhow::ensure!(
        receipt.removed,
        "not found: {} {node_did}/{id}",
        receipt.collection
    );
    match output {
        OutputFormat::Json => print_json(&json!({
            "collection": receipt.collection,
            "id": id,
            "deleted": 1,
        })),
        _ => unreachable!("ensure_supported restricts config rm output formats"),
    }
}

fn delete_failure(spec: ConfigDocumentSpec, error: DocumentDeleteError) -> anyhow::Error {
    let next = match &error {
        DocumentDeleteError::Passthrough(_) => None,
        DocumentDeleteError::StillReferenced { collection, id, .. } => Some(
            DOCUMENT_SPECS
                .iter()
                .find(|referrer| referrer.collection.graphql_type() == *collection)
                .map_or_else(
                    || format!("gents utils data query find --collection {collection}"),
                    |referrer| {
                        format!("gents config {} show {id}", referrer.noun.replace(' ', "-"))
                    },
                ),
        ),
        DocumentDeleteError::TargetMissing { .. } => {
            Some(format!("gents config {} list", spec.noun.replace(' ', "-")))
        }
        _ => Some(format!(
            "gents config {} rm --help",
            spec.noun.replace(' ', "-")
        )),
    };
    match next {
        Some(next) => anyhow::anyhow!(
            "{}\nNext: {next}",
            gents_protocol::error_chain::ErrorChain(&error)
        ),
        None => match error {
            DocumentDeleteError::Passthrough(error) => anyhow::anyhow!(error),
            error => error.into(),
        },
    }
}

fn resolve_config_id(spec: ConfigDocumentSpec, args: &ConfigShowArgs) -> Result<String> {
    resolve_dual_id(
        spec.noun,
        "--id",
        args.id.as_deref(),
        args.id_flag.as_deref(),
    )
}

pub(crate) async fn query_collection(
    access: &ConfigAccess,
    spec: ConfigDocumentSpec,
    node_did: &str,
    id: Option<&str>,
) -> Result<Vec<Value>> {
    if spec.collection == Collection::Agent {
        let mut view = gents_documents::views::agents::AgentList::new(node_did);
        if let Some(id) = id {
            view = view.agent_did(id);
        }
        return access
            .read_view(&view)
            .await?
            .into_iter()
            .map(|row| {
                let enabled = row.agent.enabled;
                let mut value = serde_json::to_value(row)?;
                value["enabled"] = Value::Bool(enabled);
                Ok(value)
            })
            .collect();
    }
    let rows = if spec.collection == Collection::InferenceBackend {
        let mut view = gents_inference::views::backends::BackendList::new(node_did);
        if let Some(id) = id {
            view = view.backend_id(id);
        }
        access
            .read_view(&view)
            .await?
            .into_iter()
            .map(|row| {
                let doc_id = row
                    .metadata
                    .physical_doc_id
                    .clone()
                    .context("backend has no physical ID")?;
                let mut value = serde_json::to_value(row.metadata.into_backend()?)?;
                value
                    .as_object_mut()
                    .context("backend must be an object")?
                    .insert("_docID".into(), Value::String(doc_id));
                Ok(value)
            })
            .collect::<Result<Vec<_>>>()?
    } else if spec.collection == Collection::InferenceProfile {
        let mut view = gents_inference::views::profiles::ProfileList::new(node_did);
        if let Some(id) = id {
            view = view.profile_id(id);
        }
        access
            .read_view(&view)
            .await?
            .into_iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<Vec<_>, _>>()?
    } else if spec.collection == Collection::InferenceSampling {
        let mut view = gents_inference::views::settings::SamplingList::new(node_did);
        if let Some(id) = id {
            view = view.id(id);
        }
        access
            .read_view(&view)
            .await?
            .into_iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<Vec<_>, _>>()?
    } else if spec.collection == Collection::InferenceExecution {
        let mut view = gents_inference::views::settings::ExecutionList::new(node_did);
        if let Some(id) = id {
            view = view.id(id);
        }
        access
            .read_view(&view)
            .await?
            .into_iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<Vec<_>, _>>()?
    } else if spec.collection == Collection::InferenceRetryPolicy {
        let mut view = gents_inference::views::settings::RetryPolicyList::new(node_did);
        if let Some(id) = id {
            view = view.id(id);
        }
        access
            .read_view(&view)
            .await?
            .into_iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<Vec<_>, _>>()?
    } else {
        use gents_documents::views::{config, tasks};
        macro_rules! read {
            ($view:expr) => {{
                access
                    .read_view(&$view)
                    .await?
                    .into_iter()
                    .map(serde_json::to_value)
                    .collect::<std::result::Result<Vec<_>, _>>()?
            }};
        }
        macro_rules! config {
            ($view:ty) => {{
                let mut view = <$view>::new(node_did);
                if let Some(id) = id {
                    view = view.id(id);
                }
                read!(view)
            }};
        }
        match spec.collection {
            Collection::Node => config!(config::NodeList),
            Collection::DefaultAgentSelection => config!(config::DefaultAgentSelectionList),
            Collection::AgentContext => config!(config::AgentContextList),
            Collection::Compaction => config!(config::CompactionConfigList),
            Collection::Tools => config!(config::ToolsList),
            Collection::AgentTarget => config!(config::AgentTargetList),
            Collection::DatastoreToolSurface => config!(config::DatastoreToolSurfaceList),
            Collection::Skill => config!(config::SkillList),
            Collection::Page => config!(config::PageList),
            Collection::PluginSource => config!(config::PluginSourceList),
            Collection::ToolServiceRegistry => read!(gents_tools::mcp::views::ServiceInventory {
                node_did,
                service_id: id,
                enabled_only: false,
            }),
            Collection::Task => {
                let mut view = tasks::TaskList::new(node_did);
                if let Some(id) = id {
                    view = view.task_id(id);
                }
                read!(view)
            }
            Collection::Trigger => {
                let mut view = tasks::TriggerList::new(node_did);
                if let Some(id) = id {
                    view = view.trigger_ids(vec![id.to_owned()]);
                }
                read!(view)
            }
            Collection::Schedule => {
                let mut view = tasks::ScheduleList::new(node_did);
                if let Some(id) = id {
                    view = view.schedule_ids(vec![id.to_owned()]);
                }
                read!(view)
            }
            Collection::EventSource => {
                let mut view = tasks::EventSourceList::new(node_did);
                if let Some(id) = id {
                    view = view.event_source_ids(vec![id.to_owned()]);
                }
                read!(view)
            }
            collection => anyhow::bail!("no configuration view for {}", collection.graphql_type()),
        }
    };
    let mut ids = std::collections::BTreeSet::new();
    for row in &rows {
        anyhow::ensure!(
            row.get("node_did").and_then(Value::as_str) == Some(node_did),
            "configuration query returned a foreign owner"
        );
        let row_id = row
            .get(spec.collection.unique_field())
            .and_then(Value::as_str)
            .context("configuration row has no logical ID")?;
        anyhow::ensure!(
            ids.insert(row_id),
            "ambiguous scoped configuration {} {row_id:?}",
            spec.collection.graphql_type()
        );
        if let Some(id) = id {
            anyhow::ensure!(
                row_id == id,
                "configuration query returned a different logical ID"
            );
        }
    }
    Ok(rows)
}

/// The node's `spec` documents in id order.
pub(crate) async fn query_collection_sorted(
    access: &ConfigAccess,
    spec: ConfigDocumentSpec,
    node_did: &str,
) -> Result<Vec<Value>> {
    query_collection(access, spec, node_did, None).await
}

pub(crate) async fn load_one(
    access: &ConfigAccess,
    spec: ConfigDocumentSpec,
    node_did: &str,
    id: &str,
) -> Result<Value> {
    query_collection(access, spec, node_did, Some(id))
        .await?
        .into_iter()
        .next()
        .with_context(|| {
            format!(
                "not found: {} {node_did:?}/{id:?}",
                spec.collection.graphql_type()
            )
        })
}

fn print_list_table(spec: ConfigDocumentSpec, rows: &[Value]) {
    print_document_table(spec.collection.unique_field(), rows);
}

pub(super) fn print_document_table(id_field: &str, rows: &[Value]) {
    let headers = ["ID", "ENABLED", "NAME"];
    let rendered = rows
        .iter()
        .map(|row| {
            [
                string_cell(row, id_field),
                bool_cell(row, "enabled"),
                string_cell(row, "display_name").or_else(|| string_cell(row, "name")),
            ]
        })
        .collect::<Vec<_>>();
    let widths = column_widths(&headers, &rendered);
    print_table_row(&headers, &widths);
    let separators = widths.map(|width| "-".repeat(width));
    let separator_cells = [
        separators[0].as_str(),
        separators[1].as_str(),
        separators[2].as_str(),
    ];
    print_table_row(&separator_cells, &widths);
    for row in rendered {
        let cells = [
            row[0].as_deref().unwrap_or(""),
            row[1].as_deref().unwrap_or(""),
            row[2].as_deref().unwrap_or(""),
        ];
        print_table_row(&cells, &widths);
    }
}

fn string_cell(row: &Value, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn bool_cell(row: &Value, field: &str) -> Option<String> {
    row.get(field).and_then(Value::as_bool).map(|value| {
        if value {
            "true".to_string()
        } else {
            "false".to_string()
        }
    })
}

fn column_widths(headers: &[&str; 3], rows: &[[Option<String>; 3]]) -> [usize; 3] {
    let mut widths = headers.map(str::len);
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell.as_deref().unwrap_or("").len());
        }
    }
    widths
}

fn print_table_row(cells: &[&str; 3], widths: &[usize; 3]) {
    println!(
        "{:<w0$}  {:<w1$}  {:<w2$}",
        cells[0],
        cells[1],
        cells[2],
        w0 = widths[0],
        w1 = widths[1],
        w2 = widths[2],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_documents::apply_desired_state_plan;
    use gents_documents::DesiredStateApplyPlan;
    use std::sync::Arc;

    #[tokio::test]
    async fn backend_reads_use_metadata_for_list_and_exact_show() -> Result<()> {
        let node = Arc::new(gents_host::test_support::node_builder().build().await?);
        gents_store::schema::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(node.node_identity_did().context("fixture Node identity")?)?,
        );
        for owner in ["owner-a", "owner-b"] {
            access.write("test.backend.metadata", &format!(
                r#"mutation {{ create_InferenceBackendMetadata(input: {{node_did: "{owner}", backend_id: "same", name: "{owner}", provider_kind: "OpenAiCompatible", endpoint: "http://127.0.0.1:9/v1", auth_kind: "credential", credential_id: "private-ref", credential_version: "version"}}) {{_docID}} }}"#
            )).await?;
        }
        let rows = query_collection(&access, BACKEND_SPEC, "owner-a", None).await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], "owner-a");
        assert_eq!(rows[0]["auth"]["kind"], "credential");
        assert!(rows[0]["_docID"].as_str().is_some());
        assert!(!rows[0].to_string().contains("secret"));
        let exact = load_one(&access, BACKEND_SPEC, "owner-b", "same").await?;
        assert_eq!(exact["name"], "owner-b");
        assert!(load_one(&access, BACKEND_SPEC, "owner-a", "missing")
            .await
            .is_err());
        node.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn canonical_reads_keep_shared_labels_in_the_selected_owner() -> Result<()> {
        let client = gents_host::test_support::ClientHome::new().await;
        let peer_dir = tempfile::tempdir()?;
        let peer = gents_store::identity::KeyIdentity::load_or_create(
            peer_dir.path().join("peer.key"),
            None,
        )?;
        let owner_a = client.node_did().to_owned();
        let owner_b = gents_store::identity::NodeIdentity::did(&peer).to_owned();
        let access = client.access.clone();
        for owner in [owner_a.as_str(), owner_b.as_str()] {
            let config = serde_json::from_value(json!({
                "node":{"node_did":owner},
                "tools":[{"node_did":owner,"tools_id":"same","display_name":owner,
                    "host":{"bash":{"allowed_argv_prefixes":[]}}}]
            }))?;
            let plan = DesiredStateApplyPlan::from_pack_config(&config)?;
            access
                .transact_write("test.config.crud.seed", |txn| {
                    let plan = &plan;
                    Box::pin(async move {
                        apply_desired_state_plan(
                            txn,
                            plan,
                            &gents_inference::resolved::ProviderProfilePolicy,
                        )
                        .await
                    })
                })
                .await?;
        }
        let rows = query_collection(&access, TOOLS_SPEC, &owner_a, None).await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["display_name"], owner_a.as_str());
        assert_eq!(rows[0]["host"]["bash"]["allowed_argv_prefixes"], json!([]));
        assert_eq!(
            load_one(&access, TOOLS_SPEC, &owner_b, "same").await?["display_name"],
            owner_b.as_str()
        );
        assert!(load_one(&access, TOOLS_SPEC, "absent", "same")
            .await
            .is_err());
        Ok(())
    }
}
