use anyhow::Result;
use gents_protocol::operations::agent::NextCall;
use gents_protocol::operations::mailbox::{
    MailboxListInput, MailboxListScope, MailboxOperationError, MailboxReply, MailboxReplyRoute,
    PreparedMailboxReply,
};
use gents_protocol::request_input::RequestInput;
use gents_session::operations::mailbox as operations;
use gents_store::config_client::{ConfigAccess, GraphqlEndpoint};

use crate::cli::args::{MailboxCommand, MailboxItemArgs, MailboxListArgs, MailboxReplyArgs};
use crate::cli::output_format::OutputFormat;
use crate::{print_json, resolve_config_access, SubmittedRequest};

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
    let access = ConfigAccess::Graphql(graphql.clone());
    let item = operations::get(&access, &args.item.doc_id)
        .await
        .map_err(failure)?;
    let prepared = operations::prepare_reply(
        (&item).into(),
        MailboxReply::Text(args.message),
        &MailboxReplyRoute::default(),
    )
    .map_err(failure)?;
    let submitted = send_reply(&graphql, prepared).await?;
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
pub(crate) async fn send_reply(
    graphql: &GraphqlEndpoint,
    prepared: PreparedMailboxReply,
) -> Result<SubmittedRequest> {
    let input = prepared.question_reply.map(|question_reply| RequestInput {
        question_reply: Some(question_reply),
        ..Default::default()
    });
    crate::create_agent_request(
        graphql,
        &prepared.node_did,
        &prepared.content,
        prepared.session_id.as_deref(),
        Some(&prepared.agent_did),
        crate::RequestSubmitOptions {
            caused_by_source_doc_id: Some(prepared.mailbox_item_id),
            input,
            ..Default::default()
        },
    )
    .await
}

/// A refused mailbox operation, shown with the command that corrects it.
pub(crate) fn failure(error: MailboxOperationError) -> anyhow::Error {
    let next = match error.recovery() {
        NextCall::VerbHelp { verb, .. } => format!("gents automation mailbox {verb} --help"),
        NextCall::ResourceHelp { .. } => "gents automation mailbox --help".to_owned(),
        NextCall::Inspect { .. } => "gents automation mailbox show <DOC_ID>".to_owned(),
        NextCall::List { .. } => "gents automation mailbox list".to_owned(),
    };
    let message = format!("{error}\nNext: {next}");
    anyhow::Error::new(error).context(message)
}
