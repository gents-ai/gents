use anyhow::{bail, Context, Result};
use gents::session::mailbox::{MailboxItem, MailboxStatus, MAILBOX_FIELDS};
use gents::store::config_client::ConfigAccess;
use gents::store::graphql::escape_graphql_string;

use crate::cli::args::{MailboxCommand, MailboxItemArgs, MailboxListArgs, MailboxReplyArgs};
use crate::cli::output_format::OutputFormat;
use crate::{print_json, resolve_config_access};

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
    let graphql = crate::resolve_graphql_endpoint(args.item.access.graphql.as_deref(), home)?;
    let access = ConfigAccess::Graphql(graphql.clone());
    let item = load_item(&access, &args.item.doc_id)
        .await?
        .context("MailboxItem not found")?;
    anyhow::ensure!(
        item.parsed_status() == Some(MailboxStatus::Open),
        "MailboxItem is no longer open"
    );
    anyhow::ensure!(
        item.parsed_action() == Some(gents::session::mailbox::MailboxAction::StartRequest),
        "MailboxItem does not accept request replies"
    );
    let submitted = crate::create_agent_request(
        &graphql,
        &item.target_node_did,
        &args.message,
        item.session_id.as_deref(),
        Some(&item.target_agent_did),
        crate::RequestSubmitOptions {
            caused_by_source_doc_id: Some(item.doc_id.clone()),
            ..Default::default()
        },
    )
    .await?;
    print_json(&serde_json::json!({
        "request_id": submitted.request_id,
        "request_doc_id": submitted.request_doc_id,
        "session_id": submitted.session_id,
        "mailbox_item_id": item.doc_id,
    }))
}

async fn list(args: MailboxListArgs) -> Result<()> {
    args.output
        .ensure_supported("mailbox list", &[OutputFormat::Json])?;
    let (access, _) =
        resolve_config_access(args.access.home.as_deref(), args.access.graphql.as_deref()).await?;
    let filter = if args.all {
        String::new()
    } else {
        r#"filter: { status: { _eq: "open" } },"#.to_owned()
    };
    let items = decode_rows(
        access
            .execute(&format!(
                "{{ MailboxItem({filter} order: {{ created_at: DESC }}) {{ {MAILBOX_FIELDS} }} }}"
            ))
            .await?,
    )?;
    print_json(&serde_json::to_value(items)?)
}

async fn show(args: MailboxItemArgs) -> Result<()> {
    args.output
        .ensure_supported("mailbox show", &[OutputFormat::Json])?;
    let (access, _) =
        resolve_config_access(args.access.home.as_deref(), args.access.graphql.as_deref()).await?;
    let item = load_item(&access, &args.doc_id)
        .await?
        .context("MailboxItem not found")?;
    print_json(&serde_json::to_value(item)?)
}

async fn dismiss(args: MailboxItemArgs) -> Result<()> {
    args.output
        .ensure_supported("mailbox dismiss", &[OutputFormat::Json])?;
    let (access, _) =
        resolve_config_access(args.access.home.as_deref(), args.access.graphql.as_deref()).await?;
    let item = gents::session::mailbox::dismiss_mailbox_item_on(&access, &args.doc_id).await?;
    print_json(&serde_json::to_value(item)?)
}

async fn load_item(access: &ConfigAccess, doc_id: &str) -> Result<Option<MailboxItem>> {
    let response = access
        .execute(&format!(
            r#"{{ MailboxItem(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ {MAILBOX_FIELDS} }} }}"#,
            escape_graphql_string(doc_id)
        ))
        .await?;
    let mut items = decode_rows(response)?;
    if items.len() > 1 {
        bail!("MailboxItem _docID lookup returned more than one row");
    }
    Ok(items.pop())
}

fn decode_rows(response: serde_json::Value) -> Result<Vec<MailboxItem>> {
    serde_json::from_value(
        response
            .pointer("/data/MailboxItem")
            .cloned()
            .context("mailbox query omitted rows")?,
    )
    .context("decoding MailboxItem rows")
}
