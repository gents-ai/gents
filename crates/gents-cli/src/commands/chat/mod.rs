mod approvals;
pub(crate) mod streaming;

use std::path::Path;

use anyhow::{bail, Result};
use gents::plugin::approval::Answer;
use gents::store::config_client::GraphqlEndpoint;

use crate::request_helpers::{
    ensure_local_request_signer, wait_for_terminal_response_with_progress,
};
use crate::{create_agent_request, RequestOutputEnvelope, RequestSubmitOptions};

pub(crate) use streaming::{load_existing_tool_call_keys, stream_turn_progress};

/// A turn's own deadlines end it sooner; this only bounds a runtime that stops answering.
const PRINT_TIMEOUT_SECS: u64 = 86_400;
const PRINT_POLL_SECS: u64 = 1;

/// Sends `message` as a turn of `session`, or of a new session, and prints the reply. A turn
/// still running in that session finishes first. Only the reply reaches stdout, so `gents -p`
/// can feed a pipe, and an approval the turn asks for is denied because nobody is there to
/// answer it.
pub(crate) async fn print_reply(
    home: &Path,
    graphql: &GraphqlEndpoint,
    node_did: &str,
    agent_id: Option<&str>,
    session: Option<&str>,
    message: &str,
) -> Result<()> {
    ensure_local_request_signer(Some(home), node_did)?;
    let session_id = session.map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_owned);
    let input = gents::request::lifecycle::prepare_user_message_input(
        &gents::ConfigAccess::Graphql(graphql.clone()),
        node_did,
        &session_id,
        Default::default(),
        gents_protocol::request_input::QueueDelivery::Queue,
    )
    .await?;
    let submitted = create_agent_request(
        graphql,
        node_did,
        message,
        Some(&session_id),
        agent_id,
        RequestSubmitOptions {
            input: Some(input),
            ..Default::default()
        },
    )
    .await?;
    let envelope = wait_for_terminal_response_with_progress(
        graphql,
        &submitted.request_id,
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
