use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::graphql::escape_graphql_string;
use gents::mailbox::{MailboxItem, MailboxStatus, MAILBOX_FIELDS};

use crate::cli::args::{
    MailboxAccessArgs, MailboxCommand, MailboxItemArgs, MailboxListArgs, MailboxReplyArgs,
};
use crate::cli::output_format::OutputFormat;
use crate::{print_json, resolve_config_access, resolve_node_did};

#[derive(Debug, thiserror::Error)]
pub(crate) enum MailboxError {
    #[error("MailboxItem not found")]
    NotFound,
    #[error("MailboxItem is not owned by the local node")]
    NotOwnedLocal,
    #[error("MailboxItem is no longer open")]
    NotOpen,
    #[error("MailboxItem does not accept request replies")]
    NotReplyable,
    #[error("local-self mailbox replies require the local node as target; use the paired client for a remote target")]
    LocalReplyTarget,
    #[error("only requester_did may dismiss a MailboxItem")]
    OnlyRequesterDismiss,
    #[error("MailboxItem has unknown status {status:?}")]
    UnknownStatus { status: String },
    #[error("MailboxItem _docID lookup returned more than one row")]
    DuplicateLookup,
    #[error("dismissed MailboxItem disappeared")]
    DismissedMissing,
    #[error("decoding MailboxItem rows")]
    DecodeRows(#[source] serde_json::Error),
}

pub(crate) async fn dispatch(command: MailboxCommand) -> Result<()> {
    match command {
        MailboxCommand::List(args) => list(args).await,
        MailboxCommand::Show(args) => show(args).await,
        MailboxCommand::Dismiss(args) => dismiss(args).await,
        MailboxCommand::Reply(args) => reply(args).await,
    }
}

async fn reply(args: MailboxReplyArgs) -> Result<()> {
    args.item
        .output
        .ensure_supported("mailbox reply", &[OutputFormat::Json])?;
    let home = args.item.access.home.as_deref();
    let principal = resolve_node_did(home, None)?;
    let graphql = crate::resolve_graphql_endpoint(args.item.access.graphql.as_deref(), home)?;
    let access = ConfigAccess::Graphql(graphql.clone());
    let item = load_item(&access, &args.item.doc_id)
        .await?
        .ok_or(MailboxError::NotFound)?;
    if item.requester_did != principal {
        return Err(MailboxError::NotOwnedLocal.into());
    }
    if item.parsed_status() != Some(MailboxStatus::Open) {
        return Err(MailboxError::NotOpen.into());
    }
    if item.parsed_action() != Some(gents::mailbox::MailboxAction::StartRequest) {
        return Err(MailboxError::NotReplyable.into());
    }
    if item.target_node_did != principal {
        return Err(MailboxError::LocalReplyTarget.into());
    }
    crate::request_helpers::ensure_local_request_signer(home, &item.target_node_did)?;
    let submitted = crate::create_agent_request(
        &graphql,
        &item.target_node_did,
        &args.message,
        item.session_id.as_deref(),
        Some(&item.target_agent_id),
        crate::RequestSubmitOptions {
            caused_by_source_doc_id: Some(item.doc_id.clone()),
            ..Default::default()
        },
    )
    .await?;
    Ok(print_json(&serde_json::json!({
        "request_id": submitted.request_id,
        "request_doc_id": submitted.request_doc_id,
        "session_id": submitted.session_id,
        "mailbox_item_id": item.doc_id,
    }))?)
}

async fn access_and_principal(
    args: &MailboxAccessArgs,
) -> Result<(gents::config_client::ConfigAccess, String)> {
    let principal =
        resolve_node_did(args.home.as_deref(), None).context("resolving mailbox node DID")?;
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    Ok((access, principal))
}

async fn list(args: MailboxListArgs) -> Result<()> {
    args.output
        .ensure_supported("mailbox list", &[OutputFormat::Json])?;
    let (access, principal) = access_and_principal(&args.access).await?;
    let items = if let Some(node) = access.local_node() {
        gents::mailbox::list_mailbox_items(
            node,
            &principal,
            (!args.all).then_some(MailboxStatus::Open),
        )
        .await?
    } else {
        let status = if !args.all {
            r#", status: { _eq: "open" }"#
        } else {
            Default::default()
        };
        decode_rows(
            access
                .execute(&format!(
                    r#"{{ MailboxItem(filter: {{ requester_did: {{ _eq: "{}" }}{status} }}, order: {{ created_at: DESC }}) {{ {MAILBOX_FIELDS} }} }}"#,
                    escape_graphql_string(&principal)
                ))
                .await?,
        )?
    };
    Ok(print_json(&serde_json::to_value(items)?)?)
}

async fn show(args: MailboxItemArgs) -> Result<()> {
    args.output
        .ensure_supported("mailbox show", &[OutputFormat::Json])?;
    let (access, principal) = access_and_principal(&args.access).await?;
    let item = load_item(&access, &args.doc_id)
        .await?
        .ok_or(MailboxError::NotFound)?;
    if item.requester_did != principal {
        return Err(MailboxError::NotOwnedLocal.into());
    }
    Ok(print_json(&serde_json::to_value(item)?)?)
}

async fn dismiss(args: MailboxItemArgs) -> Result<()> {
    args.output
        .ensure_supported("mailbox dismiss", &[OutputFormat::Json])?;
    let (access, principal) = access_and_principal(&args.access).await?;
    let item = if let Some(node) = access.local_node() {
        gents::mailbox::dismiss_mailbox_item(node, &args.doc_id, &principal).await?
    } else {
        let before = load_item(&access, &args.doc_id)
            .await?
            .ok_or(MailboxError::NotFound)?;
        if before.requester_did != principal {
            return Err(MailboxError::OnlyRequesterDismiss.into());
        }
        match before.parsed_status() {
            Some(MailboxStatus::Open) => {
                let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
                let mutation = format!(
                    r#"mutation {{ update_MailboxItem(filter: {{ _docID: {{ _eq: "{}" }}, requester_did: {{ _eq: "{}" }}, status: {{ _eq: "open" }} }}, input: {{ status: "dismissed", updated_at: "{now}", resolved_at: "{now}", resolved_doc_id: null }}) {{ _docID }} }}"#,
                    escape_graphql_string(&args.doc_id),
                    escape_graphql_string(&principal),
                );
                access.write("cli.mailbox.dismiss", &mutation).await?;
            }
            Some(_) => {}
            None => {
                return Err(MailboxError::UnknownStatus {
                    status: before.status.clone(),
                }
                .into());
            }
        }
        load_item(&access, &args.doc_id)
            .await?
            .ok_or(MailboxError::DismissedMissing)?
    };
    Ok(print_json(&serde_json::to_value(item)?)?)
}

async fn load_item(access: &ConfigAccess, doc_id: &str) -> Result<Option<MailboxItem>> {
    if let Some(node) = access.local_node() {
        return Ok(gents::mailbox::load_mailbox_item(node, doc_id).await?);
    }
    let response = access
        .execute(&format!(
            r#"{{ MailboxItem(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ {MAILBOX_FIELDS} }} }}"#,
            escape_graphql_string(doc_id)
        ))
        .await?;
    let mut items = decode_rows(response)?;
    if items.len() > 1 {
        return Err(MailboxError::DuplicateLookup.into());
    }
    Ok(items.pop())
}

fn decode_rows(response: serde_json::Value) -> Result<Vec<MailboxItem>, MailboxError> {
    serde_json::from_value(
        response
            .pointer("/data/MailboxItem")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Array(Vec::new())),
    )
    .map_err(MailboxError::DecodeRows)
}
