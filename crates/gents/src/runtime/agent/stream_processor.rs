pub use gents_loop::stream_processor::*;

use gents_session::request::seam_error::RuntimeSeamError;

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
