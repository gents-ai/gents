use anyhow::{Context, Result};
use gents_protocol::operations::workspace_root::{
    EnabledUpdate, ReaderChange, WorkspaceRootError, WorkspaceRootPublish,
    WorkspaceRootReaderInput, WorkspaceRootVerb,
};
use gents_protocol::operations::StringUpdate;
use serde_json::json;

use crate::cli::output_format::OutputFormat;
use crate::cli::*;
use crate::commands::refusal::Refusal;
use crate::config_writes::ConfigAccess;
use crate::request_helpers::resolve_dual_id;
use crate::{print_json, resolve_config_access};

fn refused(verb: WorkspaceRootVerb) -> impl Fn(WorkspaceRootError) -> Refusal<WorkspaceRootError> {
    move |error| {
        let next = error.recovery(verb);
        Refusal::new(error, next)
    }
}

fn path_text(path: &std::path::Path) -> Result<&str> {
    path.to_str().with_context(|| {
        format!(
            "--path must be valid UTF-8 to be stored as a root_path, got {}",
            path.display()
        )
    })
}

fn operator_access(graphql: &str) -> Result<ConfigAccess> {
    Ok(ConfigAccess::Graphql(
        gents_service::home_state::resolve_graphql_endpoint(Some(graphql), None)?
            .with_client_signed_agent_creates(),
    ))
}

pub(super) async fn workspace_root_list(args: ConfigListArgs) -> Result<()> {
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let roots = gents_tools::surface::list_workspace_roots(&access)
        .await
        .map_err(refused(WorkspaceRootVerb::Show))?;
    let rows = roots
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()?;
    match args.output.ensure_supported(
        "config workspace-root list",
        &[OutputFormat::Table, OutputFormat::Json],
    )? {
        OutputFormat::Json => {
            print_json(&json!({"collection":"WorkspaceRoot","count":rows.len(),"items":rows}))
        }
        OutputFormat::Table => {
            super::crud::print_document_table("root_path", &rows);
            Ok(())
        }
        _ => unreachable!("validated output"),
    }
}

pub(super) async fn workspace_root_show(args: ConfigShowArgs) -> Result<()> {
    let id = resolve_dual_id(
        "workspace root",
        "--id",
        args.id.as_deref(),
        args.id_flag.as_deref(),
    )?;
    args.output
        .ensure_supported("config workspace-root show", &[OutputFormat::Json])?;
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let root = gents_tools::surface::show_workspace_root(&access, &id)
        .await
        .map_err(refused(WorkspaceRootVerb::Show))?;
    print_json(&root)
}

pub(super) async fn workspace_root_set(args: WorkspaceRootUpsertArgs) -> Result<()> {
    let input = WorkspaceRootPublish {
        node_did: args.node_did,
        root_path: path_text(&args.path)?.to_owned(),
        display_name: args.display_name.map(StringUpdate::Set).unwrap_or_default(),
        enabled: match (args.enabled, args.disabled) {
            (true, _) => EnabledUpdate::Enable,
            (_, true) => EnabledUpdate::Disable,
            _ => EnabledUpdate::Omitted,
        },
        reader_agents: args.reader_agents,
    };
    let receipt =
        gents_tools::surface::publish_workspace_root(&operator_access(&args.graphql)?, &input)
            .await
            .map_err(refused(WorkspaceRootVerb::Publish))?;
    print_json(&json!({
        "doc_id": receipt.doc_id,
        "root_path": receipt.root_path,
        "display_name": receipt.display_name,
        "enabled": receipt.enabled,
    }))
}

async fn change_reader(args: WorkspaceRootReaderArgs, change: ReaderChange) -> Result<()> {
    let verb = match change {
        ReaderChange::Grant => WorkspaceRootVerb::GrantReader,
        ReaderChange::Revoke => WorkspaceRootVerb::RevokeReader,
    };
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let receipt = gents_tools::surface::change_workspace_root_reader(
        &access,
        &WorkspaceRootReaderInput {
            root_path: path_text(&args.path)?.to_owned(),
            agent_did: args.agent_did,
            change,
        },
    )
    .await
    .map_err(refused(verb))?;
    let agent = match change {
        ReaderChange::Grant => "granted_agent",
        ReaderChange::Revoke => "revoked_agent",
    };
    print_json(&json!({"root_path": receipt.root_path, agent: receipt.agent_did}))
}

pub(super) async fn workspace_root_grant_reader(args: WorkspaceRootReaderArgs) -> Result<()> {
    change_reader(args, ReaderChange::Grant).await
}

pub(super) async fn workspace_root_revoke_reader(args: WorkspaceRootReaderArgs) -> Result<()> {
    change_reader(args, ReaderChange::Revoke).await
}

pub(super) async fn workspace_root_rm(args: ConfigShowArgs) -> Result<()> {
    let root_path = resolve_dual_id(
        "workspace root",
        "--id",
        args.id.as_deref(),
        args.id_flag.as_deref(),
    )?;
    args.output
        .ensure_supported("config workspace-root rm", &[OutputFormat::Json])?;
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let removed = gents_tools::surface::remove_workspace_root(&access, &root_path)
        .await
        .map_err(refused(WorkspaceRootVerb::Remove))?;
    print_json(&json!({ "deleted": 1, "root_path": removed.root_path }))
}
