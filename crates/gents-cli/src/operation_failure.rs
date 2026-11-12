use std::error::Error;

use gents_protocol::error_chain::ErrorChain;
use gents_protocol::operations::request::RequestOperationError;
use gents_protocol::operations::session::SessionOperationError;
use gents_protocol::operations::NextCall;

/// A refused request or session operation, shown with its cause chain and the
/// CLI command that continues from it. The chain is part of the message, so no
/// source is reported again.
#[derive(Debug)]
pub(crate) struct OperationFailure {
    error: Box<dyn Error + Send + Sync>,
    next: NextCall,
}

impl From<RequestOperationError> for OperationFailure {
    fn from(error: RequestOperationError) -> Self {
        let next = error.next_call();
        Self {
            error: Box::new(error),
            next,
        }
    }
}

impl From<SessionOperationError> for OperationFailure {
    fn from(error: SessionOperationError) -> Self {
        let next = error.next_call();
        Self {
            error: Box::new(error),
            next,
        }
    }
}

fn command(next: NextCall) -> String {
    match next {
        NextCall::VerbHelp {
            resource: "request",
            verb,
        } => format!("gents utils debug request {verb} --help"),
        NextCall::VerbHelp {
            resource: "session",
            verb,
        } => format!("gents sessions {verb} --help"),
        NextCall::VerbHelp { resource, verb } => {
            resource_command(resource, &format!("{verb} --help"))
        }
        NextCall::Verb {
            resource: "request",
            verb,
        } => format!("gents utils debug request {verb}"),
        NextCall::Verb {
            resource: "session",
            verb,
        } => format!("gents sessions {verb}"),
        NextCall::Verb { resource, verb } => resource_command(resource, verb),
        NextCall::ResourceHelp { resource } => resource_command(resource, "--help"),
        NextCall::Inspect { resource } => match resource {
            "request" => "gents utils debug request show <REQUEST_ID>".to_owned(),
            "session" => "gents sessions show <SESSION_ID>".to_owned(),
            "agent" => "gents config agent show <AGENT_DID>".to_owned(),
            "mailbox" => "gents automation mailbox show <DOC_ID>".to_owned(),
            _ => "gents status".to_owned(),
        },
        NextCall::List { resource } => match resource {
            "request" | "session" => "gents sessions list".to_owned(),
            "agent" => "gents config agent list".to_owned(),
            "mailbox" => "gents automation mailbox list".to_owned(),
            _ => "gents status".to_owned(),
        },
    }
}

fn resource_command(resource: &str, tail: &str) -> String {
    match resource {
        "request" => format!("gents utils debug request {tail}"),
        "session" => format!("gents sessions {tail}"),
        "agent" => format!("gents config agent {tail}"),
        "mailbox" => format!("gents automation mailbox {tail}"),
        _ => "gents status".to_owned(),
    }
}

impl std::fmt::Display for OperationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}\nNext: {}",
            ErrorChain(self.error.as_ref()),
            command(self.next)
        )
    }
}

impl Error for OperationFailure {}
