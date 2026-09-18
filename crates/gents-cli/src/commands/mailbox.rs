use anyhow::{bail, Context, Result};
use gents::config_client::ConfigAccess;
use gents::graphql::escape_graphql_string;
use gents::mailbox::{MailboxItem, MailboxStatus, MAILBOX_FIELDS};

use crate::cli::args::{
    MailboxAccessArgs, MailboxCommand, MailboxItemArgs, MailboxListArgs, MailboxReplyArgs,
};
use crate::cli::output_format::OutputFormat;
use crate::{print_json, resolve_agent_did, resolve_config_access};

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
    let principal = resolve_agent_did(home, None)?;
    let graphql = crate::resolve_graphql_endpoint(args.item.access.graphql.as_deref(), home)?;
    let access = ConfigAccess::Graphql(graphql.clone());
    let item = load_item(&access, &args.item.doc_id)
        .await?
        .context("MailboxItem not found")?;
    anyhow::ensure!(
        item.requester_did == principal,
        "MailboxItem is not owned by the local principal"
    );
    anyhow::ensure!(
        item.parsed_status() == Some(MailboxStatus::Open),
        "MailboxItem is no longer open"
    );
    anyhow::ensure!(
        item.parsed_action() == Some(gents::mailbox::MailboxAction::StartRequest),
        "MailboxItem does not accept request replies"
    );
    anyhow::ensure!(
        item.target_agent_did == principal,
        "local-self mailbox replies require the local principal as target; use the paired client for a remote target"
    );
    crate::request_helpers::ensure_local_request_signer(home, &item.target_agent_did)?;
    let submitted = crate::create_agent_request(
        &graphql,
        &item.target_agent_did,
        &args.message,
        item.session_id.as_deref(),
        Some(&item.target_behavior_id),
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
                    let mutation = format!(
                        r#"mutation {{ update_MailboxItem(filter: {{ _docID: {{ _eq: "{}" }}, requester_did: {{ _eq: "{}" }}, status: {{ _eq: "open" }} }}, input: {{ status: "dismissed", updated_at: "{now}", resolved_at: "{now}", resolved_doc_id: null }}) {{ _docID }} }}"#,
                        escape_graphql_string(&args.doc_id),
                        escape_graphql_string(&principal),
                    );
                    access.write("cli.mailbox.dismiss", &mutation).await?;
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
