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

impl From<String> for ChartError {
    fn from(message: String) -> Self {
        Self(message)
    }
}

impl From<&str> for ChartError {
    fn from(message: &str) -> Self {
        Self(message.to_owned())
    }
}

/// Result of every fallible step.
pub type Res<T> = Result<T, ChartError>;

/// Fails with `message`.
pub fn fail<T>(message: impl Into<String>) -> Res<T> {
    Err(ChartError(message.into()))
}
