use anyhow::{bail, Context, Result};
use gents::config_client::ConfigAccess;
use gents::graphql::escape_graphql_string;
use gents::mailbox::{MailboxItem, MailboxStatus, MAILBOX_FIELDS};

use crate::cli::args::{MailboxAccessArgs, MailboxCommand, MailboxItemArgs, MailboxListArgs};
use crate::cli::output_format::OutputFormat;
use crate::{print_json, resolve_agent_did, resolve_config_access};

pub(crate) async fn dispatch(command: MailboxCommand) -> Result<()> {
    match command {
        MailboxCommand::List(args) => list(args).await,
        MailboxCommand::Show(args) => show(args).await,
        MailboxCommand::Dismiss(args) => dismiss(args).await,
    }
}

async fn access_and_principal(args: &MailboxAccessArgs) -> Result<(ConfigAccess, String)> {
    // The principal comes from the local identity, never a caller-supplied
    // requester flag. Remote storage enforcement remains the paired-client
    // trust boundary documented by the mailbox design.
    let principal =
        resolve_agent_did(args.home.as_deref(), None).context("resolving mailbox principal DID")?;
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    Ok((access, principal))
}

async fn list(args: MailboxListArgs) -> Result<()> {
    args.output
        .ensure_supported("mailbox list", &[OutputFormat::Json])?;
    let (access, principal) = access_and_principal(&args.access).await?;
    let items = match &access {
        ConfigAccess::Local(node) => {
            gents::mailbox::list_mailbox_items(
                node,
                &principal,
                (!args.all).then_some(MailboxStatus::Open),
            )
            .await?
        }
        ConfigAccess::Graphql(_) => {
            let status = (!args.all)
                .then_some(r#", status: { _eq: "open" }"#)
                .unwrap_or_default();
            decode_rows(
                access
                    .execute(&format!(
                        r#"{{ MailboxItem(filter: {{ requester_did: {{ _eq: "{}" }}{status} }}, order: {{ created_at: DESC }}) {{ {MAILBOX_FIELDS} }} }}"#,
                        escape_graphql_string(&principal)
                    ))
                    .await?,
            )?
        }
    };
    print_json(&serde_json::to_value(items)?)
}

async fn show(args: MailboxItemArgs) -> Result<()> {
    args.output
        .ensure_supported("mailbox show", &[OutputFormat::Json])?;
    let (access, principal) = access_and_principal(&args.access).await?;
    let item = load_item(&access, &args.doc_id)
        .await?
        .context("MailboxItem not found")?;
    if item.requester_did != principal {
        bail!("MailboxItem is not owned by the local principal");
    }
    print_json(&serde_json::to_value(item)?)
}

async fn dismiss(args: MailboxItemArgs) -> Result<()> {
    args.output
        .ensure_supported("mailbox dismiss", &[OutputFormat::Json])?;
    let (access, principal) = access_and_principal(&args.access).await?;
    let item = match &access {
        ConfigAccess::Local(node) => {
            gents::mailbox::dismiss_mailbox_item(node, &args.doc_id, &principal).await?
        }
        ConfigAccess::Graphql(_) => {
            let before = load_item(&access, &args.doc_id)
                .await?
                .context("MailboxItem not found")?;
            if before.requester_did != principal {
                bail!("only requester_did may dismiss a MailboxItem");
            }
            match before.parsed_status() {
                Some(MailboxStatus::Open) => {
                    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
                    access
                        .execute(&format!(
                            r#"mutation {{ update_MailboxItem(filter: {{ _docID: {{ _eq: "{}" }}, requester_did: {{ _eq: "{}" }}, status: {{ _eq: "open" }} }}, input: {{ status: "dismissed", updated_at: "{now}", resolved_at: "{now}", resolved_doc_id: null }}) {{ _docID }} }}"#,
                            escape_graphql_string(&args.doc_id),
                            escape_graphql_string(&principal),
                        ))
                        .await?;
                }
                Some(_) => {}
                None => bail!("MailboxItem has unknown status {:?}", before.status),
            }
            load_item(&access, &args.doc_id)
                .await?
                .context("dismissed MailboxItem disappeared")?
        }
    };
    print_json(&serde_json::to_value(item)?)
}

async fn load_item(access: &ConfigAccess, doc_id: &str) -> Result<Option<MailboxItem>> {
    if let ConfigAccess::Local(node) = access {
        return gents::mailbox::load_mailbox_item(node, doc_id).await;
    }
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
            .unwrap_or_else(|| serde_json::Value::Array(Vec::new())),
    )
    .context("decoding MailboxItem rows")
}
