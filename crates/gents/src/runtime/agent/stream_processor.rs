pub use gents_loop::stream_processor::*;

use crate::request::seam_error::RuntimeSeamError;

/// The processor's failure as the daemon classifies it: a seam's own error is
/// recovered whole, with the processor's context where it adds one.
pub(crate) fn processor_failure(error: StreamProcessorError) -> anyhow::Error {
    match error {
        StreamProcessorError::Writer(source)
        | StreamProcessorError::Hook(source)
        | StreamProcessorError::Lifecycle(source) => RuntimeSeamError::recover(source),
        StreamProcessorError::PartialAttemptClose { context, source } => {
            RuntimeSeamError::recover(source)
                .context(format!("{context}: closing partial provider attempt"))
        }
        StreamProcessorError::PartialTailReset { context, source } => {
            RuntimeSeamError::recover(source)
                .context(format!("{context}: resetting partial assistant tail"))
        }
        error => error.into(),
    }
}

#[cfg(test)]
use crate::runtime::agent::loop_stream::LoopStreamItem;
#[cfg(test)]
use crate::tools::call::hook::DefraSessionHook;

#[cfg(test)]
#[path = "stream_processor_tests.rs"]
mod tests;

#[cfg(test)]
mod seam_error_tests {
    use super::{processor_failure, RuntimeSeamError, StreamProcessorError};
    use crate::request::lifecycle::ExecutionOwnershipLost;

    fn seam(error: anyhow::Error) -> Box<dyn std::error::Error + Send + Sync> {
        RuntimeSeamError::from(error).into()
    }

    #[test]
    fn seam_errors_keep_their_type_across_the_loop_boundary() {
        let lost = || anyhow::Error::from(ExecutionOwnershipLost);
        let contextual = || lost().context("validating execution");
        for error in [
            StreamProcessorError::Lifecycle(seam(lost())),
            StreamProcessorError::Lifecycle(seam(contextual())),
            StreamProcessorError::Writer(seam(lost())),
            StreamProcessorError::Hook(seam(contextual())),
        ] {
            assert!(processor_failure(error).is::<ExecutionOwnershipLost>());
        }

        let closed = processor_failure(StreamProcessorError::PartialAttemptClose {
            context: "interrupt".into(),
            source: seam(lost()),
        });
        assert!(closed.is::<ExecutionOwnershipLost>());
        assert_eq!(
            closed.to_string(),
            "interrupt: closing partial provider attempt"
        );

        let foreign = processor_failure(StreamProcessorError::Writer(Box::new(
            std::io::Error::other("foreign writer"),
        )));
        assert_eq!(foreign.to_string(), "foreign writer");
        assert!(processor_failure(StreamProcessorError::EmptyResponse).is::<StreamProcessorError>());
    }
}
