//! Drains one owned-loop stream into durable writes, moved to `gents-loop`
//! (G-1): generic over `H: SessionHook`, `W: StreamWriter`, `L:
//! RequestLifecycleControl` instead of owning `DefraSessionHook`,
//! `DefraStreamWriter`, and `RequestLifecycle` directly. Re-exported so
//! `crate::agent::stream_processor` keeps every symbol this crate's callers
//! already use; `DefraSessionHook`, `DefraStreamWriter`, and
//! `RequestLifecycle` (native) implement the three traits.
pub use gents_loop::stream_processor::*;

// Glue for the test suite below, which reaches these bare through
// `use super::*` the way it did before the move.
#[cfg(test)]
use crate::agent::loop_stream::LoopStreamItem;
#[cfg(test)]
use crate::hook::DefraSessionHook;

#[cfg(test)]
#[path = "stream_processor_tests.rs"]
mod tests;
