//! The one error type: a single plain sentence that says what happened and
//! what to do. It is what the caller reads, so it carries no internal names.

use std::fmt;

/// A failed call, as the sentence shown to the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChartError(pub String);

impl fmt::Display for ChartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ChartError {}

/// Longest error sentence; caller text inside it is cut beyond this.
const MAX_SENTENCE: usize = 600;

impl ChartError {
    /// An error with `message`, cut to a sentence-sized length.
    pub fn new(message: impl AsRef<str>) -> Self {
        Self(crate::text::limit_chars(message.as_ref(), MAX_SENTENCE).into_owned())
    }
}

impl From<String> for ChartError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

impl From<&str> for ChartError {
    fn from(message: &str) -> Self {
        Self::new(message)
    }
}

/// Result of every fallible step.
pub type Res<T> = Result<T, ChartError>;

/// Fails with `message`.
pub fn fail<T>(message: impl Into<String>) -> Res<T> {
    Err(ChartError::new(message.into()))
}
