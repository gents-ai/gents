use crate::cli::{AgentTargetEntryArgs, ConfigShowArgs, ToolsSetArgs};
use anyhow::{Context, Result};
use gents_documents::AgentTargetDocument;
use gents_protocol::operations::document::WriteMode;
use gents_protocol::operations::tools::ToolsSaveInput;
use serde_json::{json, Value};

use crate::commands::refusal::Refusal;

pub(super) fn agent_target_entry_command(args: AgentTargetEntryArgs) -> Result<()> {
    crate::print_json(&serde_json::to_value(AgentTargetDocument {
        target_id: args.target_id,
        node_did: args.node_did,
        target_node_did: args.target_node_did,
        agent_did: args.agent_did,
        name: args.name,
        description: args.description,
        tags: Vec::new(),
    })?)
}

pub(super) async fn tools_set(args: ToolsSetArgs) -> Result<()> {
    let contents = std::fs::read(&args.file)
        .with_context(|| format!("reading Tools document {}", args.file.display()))?;
    let Value::Object(mut document) =
        serde_json::from_slice(&contents).context("decoding the Tools JSON document")?
    else {
        anyhow::bail!("the Tools file holds a JSON object");
    };
    let mut identity = |field: &str| match document.remove(field) {
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(value),
        _ => Err(anyhow::anyhow!("the Tools file names its {field}")),
    };
    let tools_id = identity("tools_id")?;
    let owner = identity("node_did")?;
    let input = ToolsSaveInput {
        tools_id,
        mode: WriteMode::Save,
        patch: document
            .into_iter()
            .map(|(group, value)| (group, (!value.is_null()).then_some(value)))
            .collect(),
        allow_drop: args
            .allow_drop
            .iter()
            .flat_map(|groups| groups.split(','))
            .map(|group| group.trim().to_owned())
            .filter(|group| !group.is_empty())
            .collect(),
        shared_acknowledged: args.shared_acknowledge,
    };
    let (access, home) =
        crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let installed = |plugin: &str| {
        let (namespace, name) = gents_documents::plugin_coordinate::parse_coordinate(plugin)?;
        gents_pack::plugin::store::read_record(&home, namespace, name).map(|_| ())
    };
    let receipt = gents::operations::tools::save(
        &access,
        &owner,
        &input,
        args.graphql
            .is_none()
            .then_some(&installed as gents::operations::tools::InstalledPlugins<'_>),
    )
    .await
    .map_err(|error| {
        let next = error.recovery();
        Refusal::new(error, next)
    })?;
    crate::print_json(&json!({
        "doc_id": receipt.document.doc_id,
        "tools_id": receipt.document.id,
        "node_did": receipt.document.node_did,
        "created": receipt.document.created,
        "changed": receipt
            .document
            .changed
            .iter()
            .map(|change| &change.field)
            .collect::<Vec<_>>(),
        "shared_with": receipt.shared_with,
    }))
}

pub(super) async fn tools_rm(args: ConfigShowArgs) -> Result<()> {
    let id = crate::request_helpers::resolve_dual_id(
        "tools",
        "--id",
        args.id.as_deref(),
        args.id_flag.as_deref(),
    )?;
    args.output.ensure_supported(
        "config tools rm",
        &[crate::cli::output_format::OutputFormat::Json],
    )?;
    let (access, _) =
        crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let owner = super::binding::resolve_target_node_did(
        None,
        None,
        args.home.as_deref(),
        args.graphql.as_deref(),
        Some(&access),
    )
    .await?;
    let receipt = gents::operations::tools::delete(&access, &owner, &id)
        .await
        .map_err(|error| {
            let next = error.recovery();
            Refusal::new(error, next)
        })?;
    crate::print_json(&json!({
        "collection": "Tools",
        "id": id,
        "deleted": receipt.targets.len(),
    }))
}
