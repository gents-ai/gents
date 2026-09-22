//! Owned multi-turn completion and tool-execution loop, moved to
//! `gents-loop` (G-1): the loop assembles provider input, dispatches tools,
//! and threads messages with no DefraDB dependency, taking the persistence
//! hook as `H: SessionHook` (`DefraSessionHook` implements it, in
//! `crate::hook`) instead of owning it. Re-exported here so
//! `crate::agent::loop_stream` keeps every symbol this crate's callers
//! already use; the full test suite (DefraDB-backed end-to-end cases
//! included) stays here and exercises the loop through this re-export.
pub use gents_loop::loop_stream::*;

// Glue for the test suite below, which reaches these bare through
// `use super::*` the way it did before the move (gents-loop's own
// `loop_stream` module imports them privately for its own use, so they do
// not ride the glob re-export above).
#[cfg(test)]
use crate::llm::rig_compat;
#[cfg(test)]
use crate::rendered_request::{AssemblyBuildPath, AssemblyTrace, ContextCompactionReason};
#[cfg(test)]
use rig::agent::{MultiTurnStreamItem, StreamingError};
#[cfg(test)]
use rig::completion::{GetTokenUsage, Usage};

#[cfg(test)]
mod tests;
