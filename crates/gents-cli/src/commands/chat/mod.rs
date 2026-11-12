mod approvals;
pub(crate) mod streaming;

use std::path::Path;

use anyhow::{bail, Result};
use gents_protocol::operations::request::{DeliveryChoice, MessageSendInput};
use gents_store::config_client::{ConfigAccess, GraphqlEndpoint};
use gents_tools::plugin::approval::Answer;

use crate::operation_failure::OperationFailure;
use crate::request_helpers::{client_signer, request_ref, wait_for_request};
use crate::RequestOutputEnvelope;

pub(crate) use streaming::stream_turn_progress;

/// A turn's own deadlines end it sooner; this only bounds a runtime that stops answering.
const PRINT_TIMEOUT_SECS: u64 = 86_400;
const PRINT_POLL_SECS: u64 = 1;

/// Sends `message` as a turn of `session`, or of a new session, and prints the reply. A turn
/// still running in that session finishes first unless `steer` asks to steer it. Only the reply
/// reaches stdout, so `gents -p` can feed a pipe, and an approval the turn asks for is denied
/// because nobody is there to answer it.
pub(crate) async fn print_reply(
    home: &Path,
    graphql: &GraphqlEndpoint,
    node_did: &str,
    agent_did: Option<&str>,
    session: Option<&str>,
    message: &str,
    steer: bool,
) -> Result<()> {
    // The turn is the User's: it is signed by the CLI's client key, never the Node's.
    gents_service::home_state::load_client_identity().await?;
    let session_id = session.map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_owned);
    let access = ConfigAccess::Graphql(graphql.clone());
    let signer = client_signer(&access)?;
    let receipt = gents::operations::request::send(
        &access,
        &signer,
        MessageSendInput {
            node_did: node_did.to_owned(),
            session_id: Some(session_id.clone()),
            agent_did: agent_did.map(str::to_owned),
            content: message.to_owned(),
            delivery: DeliveryChoice::steer_when(steer),
            ..Default::default()
        },
    )
    .await
    .map_err(OperationFailure::from)?;
    let envelope = wait_for_request(
        &access,
        request_ref(&receipt.node_did, &receipt.request_id),
        PRINT_TIMEOUT_SECS,
        PRINT_POLL_SECS,
        || deny_pending_approvals(home, &session_id),
    )
    .await?;
    if let Some(reason) = envelope
        .request
        .failure_reason
        .as_deref()
        .filter(|reason| !reason.trim().is_empty())
    {
        bail!("the agent's turn failed: {reason}");
    }
    let reply = chat_turn_text_content(&envelope);
    if !reply.trim().is_empty() {
        println!("{reply}");
    }
    Ok(())
}

async fn deny_pending_approvals(home: &Path, session_id: &str) -> Result<bool> {
    let (home, session_id) = (home.to_path_buf(), session_id.to_owned());
    tokio::task::spawn_blocking(move || {
        approvals::answer_pending(&home, &session_id, |request| {
            eprintln!("{} Denied: `gents -p` has no one to ask.", request.prompt());
            Answer::Deny
        })
    })
    .await?
}

fn terminal_presentation(
    envelope: &RequestOutputEnvelope,
) -> Option<&crate::CliOutputPresentation> {
    match &envelope.output {
        crate::CliOutputObservation::TerminalMessage { presentation, .. } => Some(presentation),
        crate::CliOutputObservation::TerminalNoMessage => None,
        _ => None,
    }
}

pub(crate) fn chat_turn_text_content(envelope: &RequestOutputEnvelope) -> &str {
    terminal_presentation(envelope)
        .map(|presentation| presentation.body_markdown.as_str())
        .unwrap_or("")
}
