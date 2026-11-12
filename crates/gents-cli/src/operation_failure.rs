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

impl std::fmt::Display for OperationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}\nNext: {}",
            ErrorChain(self.error.as_ref()),
            crate::next_call::command(self.next, None)
        )
    }
}

impl Error for OperationFailure {}
