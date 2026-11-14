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
        let next = crate::next_call::command(self.next, None);
        write!(formatter, "{}\nNext: {next}", ErrorChain(&self.error))
    }
}

impl std::error::Error for InferenceFailure {}
