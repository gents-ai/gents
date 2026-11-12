use gents_protocol::error_chain::ErrorChain;
use gents_protocol::operations::inference::{InferenceDocument, InferenceOperationError};
use gents_protocol::operations::NextCall;

/// A refused inference operation, shown with the command that corrects it.
#[derive(Debug)]
pub(crate) struct InferenceFailure {
    error: InferenceOperationError,
    next: NextCall,
}

impl InferenceFailure {
    pub(crate) fn new(
        error: InferenceOperationError,
        document: InferenceDocument,
        verb: &'static str,
    ) -> Self {
        let next = error.recovery(document, verb);
        Self { error, next }
    }
}

impl std::fmt::Display for InferenceFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let next = match self.next {
            NextCall::VerbHelp {
                resource,
                verb: "update" | "create",
            } => format!("gents config {resource} set --help"),
            NextCall::VerbHelp { resource, verb } => {
                format!("gents config {resource} {verb} --help")
            }
            NextCall::ResourceHelp { resource } => format!("gents config {resource} --help"),
            NextCall::Inspect { resource } => format!("gents config {resource} show <ID>"),
            NextCall::List { resource } => format!("gents config {resource} list"),
            NextCall::Verb {
                resource: "backend",
                verb: "accounts",
            } => "gents accounts list".to_owned(),
            NextCall::Verb { resource, verb } => format!("gents config {resource} {verb}"),
        };
        write!(formatter, "{}\nNext: {next}", ErrorChain(&self.error))
    }
}

impl std::error::Error for InferenceFailure {}
