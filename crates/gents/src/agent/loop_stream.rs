//! Owned multi-turn completion and tool-execution loop, moved to
//! `gents-loop` (G-1): the loop assembles provider input, dispatches tools,
//! and threads messages with no DefraDB dependency, taking the persistence
//! hook as `H: SessionHook` (`DefraSessionHook` implements it, in
//! `crate::hook`) instead of owning it. Re-exported here so
//! `crate::agent::loop_stream` keeps every symbol this crate's callers
//! already use; the full test suite (DefraDB-backed end-to-end cases
//! included) stays here and exercises the loop through this re-export.
pub use gents_loop::loop_stream::*;

#[cfg(test)]
mod tests;
