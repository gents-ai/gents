/// The error the stream writer, session hook and request lifecycle
/// hand the loop. The loop erases seam errors to `Box<dyn Error>`, and an
/// `anyhow::Error` boxed directly can no longer be downcast or `is`-checked,
/// so the runtime boxes this wrapper and [`RuntimeSeamError::recover`] returns
/// the original application error with its whole typed chain.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct RuntimeSeamError(#[from] anyhow::Error);

impl RuntimeSeamError {
    pub fn into_inner(self) -> anyhow::Error {
        self.0
    }

    pub fn recover(error: Box<dyn std::error::Error + Send + Sync>) -> anyhow::Error {
        match error.downcast::<Self>() {
            Ok(seam) => seam.into_inner(),
            Err(other) => anyhow::Error::from_boxed(other),
        }
    }
}
