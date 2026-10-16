//! Re-export of `gents_loop::loop_stream` so `crate::agent::loop_stream`
//! keeps every symbol this crate's callers already use; the test suite
//! (DefraDB-backed end-to-end cases included) lives here and exercises the
//! loop through this re-export.
pub use gents_loop::loop_stream::*;

// The re-export above does not carry these; the test suite reaches them
// bare through `use super::*`.
#[cfg(test)]
use crate::llm::rig_compat;
#[cfg(test)]
use crate::rendered_request::{AssemblyBuildPath, AssemblyTrace, ContextCompactionReason};
#[cfg(test)]
use rig::agent::StreamingError;
#[cfg(test)]
use rig::completion::{GetTokenUsage, Usage};

#[cfg(test)]
mod tests;
