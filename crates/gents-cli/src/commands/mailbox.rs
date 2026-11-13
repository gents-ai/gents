use anyhow::Result;
use gents_protocol::operations::mailbox::{
    MailboxListInput, MailboxListScope, MailboxOperationError, MailboxReply, MailboxReplyRoute,
    PreparedMailboxReply,
};
use gents_protocol::operations::request::{MessageSendInput, RequestReceipt};
use gents_protocol::request_input::RequestInput;
use gents_session::operations::mailbox as operations;
use gents_store::config_client::ConfigAccess;

use crate::cli::args::{MailboxCommand, MailboxItemArgs, MailboxListArgs, MailboxReplyArgs};
use crate::cli::output_format::OutputFormat;
use crate::operation_failure::OperationFailure;
use crate::request_helpers::client_signer;
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
        .ensure_supported("automation mailbox reply", &[OutputFormat::Json])?;
    let home = args.item.access.home.as_deref();
    let graphql = gents_service::home_state::resolve_graphql_endpoint(
        args.item.access.graphql.as_deref(),
        home,
    )?;
    let access = ConfigAccess::Graphql(graphql);
    let item = operations::get(&access, &args.item.doc_id)
        .await
        .map_err(failure)?;
    let prepared = operations::prepare_reply(
        (&item).into(),
        MailboxReply::Text(args.message),
        &MailboxReplyRoute::default(),
    )
    .map_err(failure)?;
    let submitted = send_reply(&access, prepared).await?;
    print_json(&serde_json::json!({
        "request_id": submitted.request_id,
        "request_doc_id": submitted.request_doc_id,
        "session_id": submitted.session_id,
        "mailbox_item_id": item.doc_id,
    }))
}

async fn list(args: MailboxListArgs) -> Result<()> {
    args.output
        .ensure_supported("automation mailbox list", &[OutputFormat::Json])?;
    let (access, _) =
        resolve_config_access(args.access.home.as_deref(), args.access.graphql.as_deref()).await?;
    let scope = if args.all {
        MailboxListScope::All
    } else {
        MailboxListScope::Waiting
    };
    let items = operations::list(
        &access,
        &MailboxListInput {
            scope,
            ..Default::default()
        },
    )
    .await
    .map_err(failure)?;
    print_json(&serde_json::to_value(items)?)
}

async fn show(args: MailboxItemArgs) -> Result<()> {
    args.output
        .ensure_supported("automation mailbox show", &[OutputFormat::Json])?;
    let (access, _) =
        resolve_config_access(args.access.home.as_deref(), args.access.graphql.as_deref()).await?;
    let item = operations::get(&access, &args.doc_id)
        .await
        .map_err(failure)?;
    print_json(&serde_json::to_value(item)?)
}

async fn dismiss(args: MailboxItemArgs) -> Result<()> {
    args.output
        .ensure_supported("automation mailbox dismiss", &[OutputFormat::Json])?;
    let (access, _) =
        resolve_config_access(args.access.home.as_deref(), args.access.graphql.as_deref()).await?;
    let item = operations::dismiss(&access, &args.doc_id)
        .await
        .map_err(failure)?;
    print_json(&serde_json::to_value(item)?)
}

/// Sends an admitted reply as a request the item caused, on the item's route.
/// The request owner refuses an item that cannot cause it.
pub(crate) async fn send_reply(
    access: &ConfigAccess,
    prepared: PreparedMailboxReply,
) -> Result<RequestReceipt> {
    let signer = client_signer(access)?;
    let receipt = gents::operations::request::send(
        access,
        &signer,
        MessageSendInput {
            node_did: prepared.node_did,
            session_id: prepared.session_id,
            agent_did: Some(prepared.agent_did),
            content: prepared.content,
            input: RequestInput {
                question_reply: prepared.question_reply,
                ..Default::default()
            },
            mailbox_item_id: Some(prepared.mailbox_item_id),
            ..Default::default()
        },
    )
    .await
    .map_err(OperationFailure::from)?;
    Ok(receipt)
}

/// A refused mailbox operation, shown with the command that corrects it.
pub(crate) fn failure(error: MailboxOperationError) -> anyhow::Error {
    let next = error.recovery();
    crate::commands::refusal::Refusal::new(error, next).into()
}
