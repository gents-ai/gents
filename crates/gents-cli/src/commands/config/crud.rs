use anyhow::{Context, Result};
use gents::graphql::escape_graphql_string;
use gents::Collection;
use serde_json::{json, Value};

use crate::cli::output_format::OutputFormat;
use crate::cli::{ConfigListArgs, ConfigShowArgs};
use crate::config_import::apply_delete_collection;
use crate::config_writes::ConfigAccess;
use crate::desired_state;
use crate::request_helpers::resolve_dual_id;
use crate::{graphql_rows, print_json, resolve_config_access};

#[derive(Clone, Copy)]
pub(super) struct ConfigDocumentSpec {
    pub(super) noun: &'static str,
    pub(super) collection: Collection,
}

pub(super) const BACKEND_SPEC: ConfigDocumentSpec = ConfigDocumentSpec {
    noun: "backend",
    collection: Collection::InferenceBackend,
};

pub(super) const BEHAVIOR_SPEC: ConfigDocumentSpec = ConfigDocumentSpec {
    noun: "behavior",
    collection: Collection::AgentBehavior,
};

pub(super) const TOOLS_SPEC: ConfigDocumentSpec = ConfigDocumentSpec {
    noun: "tools",
    collection: Collection::Tools,
};

pub(super) const PROFILE_SPEC: ConfigDocumentSpec = ConfigDocumentSpec {
    noun: "profile",
    collection: Collection::InferenceProfile,
};

pub(super) const TRIGGER_SPEC: ConfigDocumentSpec = ConfigDocumentSpec {
    noun: "trigger",
    collection: Collection::Trigger,
};

pub(super) const SCHEDULE_SPEC: ConfigDocumentSpec = ConfigDocumentSpec {
    noun: "schedule",
    collection: Collection::Schedule,
};

pub(super) const MCP_SPEC: ConfigDocumentSpec = ConfigDocumentSpec {
    noun: "mcp",
    collection: Collection::ToolServiceRegistry,
};

pub(super) async fn config_list(spec: ConfigDocumentSpec, args: ConfigListArgs) -> Result<()> {
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref())
        .await
        .with_context(|| format!("resolving access for config {} list", spec.noun))?;
    let agent_did = super::binding::resolve_target_agent_did(
        None,
        None,
        args.home.as_deref(),
        args.graphql.as_deref(),
        Some(&access),
    )
    .await?;
    let mut rows = query_collection(&access, spec, &agent_did, None).await?;
    sort_rows(&mut rows, spec.collection.unique_field());

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
    let agent_did = super::binding::resolve_target_agent_did(
        None,
        None,
        args.home.as_deref(),
        args.graphql.as_deref(),
        Some(&access),
    )
    .await?;
    let row = load_one(&access, spec, &agent_did, &id).await?;

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
    let live = live_manifest_for_delete(
        &access,
        spec,
        &id,
        args.home.as_deref(),
        args.graphql.as_deref(),
    )
    .await?;
    let mut desired = live.clone();
    remove_target(&mut desired, spec.collection, &id)?;

    let deletes = desired_state::prune::prune_safe_deletes(&desired, &live)?;
    let target_selected = deletes
        .iter()
        .any(|doc| doc.collection == spec.collection && doc.id == id);
    if !target_selected {
        anyhow::bail!("refused: {} {} is still referenced", spec.noun, id);
    }
    if deletes
        .iter()
        .any(|doc| doc.collection != spec.collection || doc.id != id)
    {
        anyhow::bail!(
            "refused: deleting {} {} would select additional documents",
            spec.noun,
            id
        );
    }

    let collection = spec.collection;
    let agent_did = &live.agent_principal.agent_did;
    let id_ref = &id;
    let deleted = access
        .transact("cli.config.delete", move |txn| {
            Box::pin(async move {
                apply_delete_collection(txn, collection, agent_did, std::slice::from_ref(id_ref))
                    .await
            })
        })
        .await?;
    match output {
        OutputFormat::Json => print_json(&json!({
            "collection": spec.collection.graphql_type(),
            "id": id,
            "deleted": deleted,
        })),
        _ => unreachable!("ensure_supported restricts config rm output formats"),
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

async fn live_manifest_for_delete(
    access: &ConfigAccess,
    spec: ConfigDocumentSpec,
    id: &str,
    home: Option<&std::path::Path>,
    graphql: Option<&str>,
) -> Result<desired_state::DesiredStateManifest> {
    let agent_did =
        super::binding::resolve_target_agent_did(None, None, home, graphql, Some(access)).await?;
    let bundle = crate::build_config_export_bundle(access, &agent_did).await?;
    let docs = bundle.docs_for_collection(spec.collection)?;
    anyhow::ensure!(
        docs.iter().any(|row| row
            .get(spec.collection.unique_field())
            .and_then(Value::as_str)
            == Some(id)),
        "target configuration is absent from the selected owner"
    );
    desired_state::manifest_from_export_bundle(&bundle)
}

async fn query_collection(
    access: &ConfigAccess,
    spec: ConfigDocumentSpec,
    agent_did: &str,
    id: Option<&str>,
) -> Result<Vec<Value>> {
    let mut filter = format!(
        r#"agent_did: {{ _eq: "{}" }}"#,
        escape_graphql_string(agent_did)
    );
    if let Some(id) = id {
        filter.push_str(&format!(
            r#", {}: {{ _eq: "{}" }}"#,
            spec.collection.unique_field(),
            escape_graphql_string(id)
        ));
    }
    let fields = gents::config_client::config_projection(spec.collection, None)?
        .0
        .join(" ");
    let query = format!(
        "{{ {}(filter: {{ {filter} }}) {{ _docID {fields} }} }}",
        spec.collection.graphql_type()
    );
    let rows = graphql_rows(access, spec.collection.graphql_type(), &query).await?;
    let mut ids = std::collections::BTreeSet::new();
    for row in &rows {
        anyhow::ensure!(
            row.get("agent_did").and_then(Value::as_str) == Some(agent_did),
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

async fn load_one(
    access: &ConfigAccess,
    spec: ConfigDocumentSpec,
    agent_did: &str,
    id: &str,
) -> Result<Value> {
    query_collection(access, spec, agent_did, Some(id))
        .await?
        .into_iter()
        .next()
        .with_context(|| {
            format!(
                "not found: {} {agent_did:?}/{id:?}",
                spec.collection.graphql_type()
            )
        })
}

fn remove_target(
    manifest: &mut desired_state::DesiredStateManifest,
    collection: Collection,
    id: &str,
) -> Result<()> {
    let key = collection
        .dir_name()
        .context("principal deletion is not a config document removal")?;
    let mut encoded = serde_json::to_value(&*manifest)?;
    if let Some(rows) = encoded.get_mut(key).and_then(Value::as_array_mut) {
        rows.retain(|row| row.get(collection.unique_field()).and_then(Value::as_str) != Some(id));
    }
    *manifest = serde_json::from_value(encoded)?;
    Ok(())
}

fn sort_rows(rows: &mut [Value], id_field: &str) {
    rows.sort_by(|a, b| {
        a.get(id_field)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .cmp(b.get(id_field).and_then(Value::as_str).unwrap_or_default())
    });
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
    use gents::config_client::{apply_desired_state_plan, DesiredStateApplyPlan};
    use std::sync::Arc;

    #[tokio::test]
    async fn canonical_reads_keep_shared_labels_in_the_selected_owner() -> Result<()> {
        let node = Arc::new(gents::defra_node::EmbeddedNode::builder().build().await?);
        gents::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::Local(node.clone());
        for owner in ["owner-a", "owner-b"] {
            let config = serde_json::from_value(json!({
                "agent_principal":{"agent_did":owner},
                "tools":[{"agent_did":owner,"tools_id":"same","display_name":owner,
                    "host":{"bash":{"allowed_argv_prefixes":[]}}}]
            }))?;
            let plan = DesiredStateApplyPlan::from_pack_config(&config)?;
            access
                .transact("test.config.crud.seed", |txn| {
                    let plan = &plan;
                    Box::pin(async move { apply_desired_state_plan(txn, plan).await })
                })
                .await?;
        }
        let rows = query_collection(&access, TOOLS_SPEC, "owner-a", None).await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["display_name"], "owner-a");
        assert_eq!(rows[0]["host"]["bash"]["allowed_argv_prefixes"], json!([]));
        assert_eq!(
            load_one(&access, TOOLS_SPEC, "owner-b", "same").await?["display_name"],
            "owner-b"
        );
        assert!(load_one(&access, TOOLS_SPEC, "absent", "same")
            .await
            .is_err());
        node.shutdown().await;
        Ok(())
    }
}
