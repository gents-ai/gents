mod approvals;
pub(crate) mod streaming;

use std::path::Path;

use anyhow::{bail, Result};
use gents::plugin::approval::Answer;
use gents::store::config_client::GraphqlEndpoint;

use crate::request_helpers::wait_for_terminal_response_with_progress;
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
    agent_did: Option<&str>,
    session: Option<&str>,
    message: &str,
) -> Result<()> {
    // The turn is the User's: it is signed by the CLI's client key, never the Node's.
    crate::load_client_identity().await?;
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
        agent_did,
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
        bail!("{}", turn_failure(reason));
    }
    let reply = chat_turn_text_content(&envelope);
    if !reply.trim().is_empty() {
        println!("{reply}");
    }
    Ok(())
}

/// A turn refused because its agent has no model that answers is fixed in the shell, which
/// connects one, so that failure names it.
fn turn_failure(reason: &str) -> String {
    let unready = gents_protocol::node_readiness::unavailable_rejection_reason(reason);
    match unready.is_some_and(crate::commands::start::wants_a_model) {
        true => format!(
            "the agent's turn failed: {reason}. Run `gents` in a terminal to connect a model."
        ),
        false => format!("the agent's turn failed: {reason}"),
    }
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

#[cfg(test)]
mod tests {
    use gents_protocol::node_readiness::AgentReadinessUnavailableReason as Reason;

    use super::turn_failure;

    /// `gents -p` has no terminal to set a model up in, so a turn refused for want of one
    /// names the shell, never a file to edit.
    #[test]
    fn a_turn_refused_for_want_of_a_model_names_the_shell() {
        for reason in [
            Reason::BackendNotConfigured,
            Reason::BackendTemporarilyUnavailable,
            Reason::CredentialsRequired,
        ] {
            let failure = turn_failure(reason.public_message());
            assert!(failure.contains(reason.public_message()), "{failure}");
            assert!(
                failure.ends_with("Run `gents` in a terminal to connect a model."),
                "{failure}"
            );
            assert!(!failure.contains(".json"), "{failure}");
        }
        for other in [Reason::AgentDisabled.public_message(), "the model said no"] {
            assert_eq!(
                turn_failure(other),
                format!("the agent's turn failed: {other}")
            );
        }
    }
}
