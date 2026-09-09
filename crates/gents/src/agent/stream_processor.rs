//! Drains one owned-loop stream into durable writes, moved to `gents-loop`
//! (G-1): generic over `H: SessionHook`, `W: StreamWriter`, `L:
//! RequestLifecycleControl` instead of owning `DefraSessionHook`,
//! `DefraStreamWriter`, and `RequestLifecycle` directly. Re-exported so
//! `crate::agent::stream_processor` keeps every symbol this crate's callers
//! already use; `DefraSessionHook`, `DefraStreamWriter`, and
//! `RequestLifecycle` (native) implement the three traits.
pub use gents_loop::stream_processor::*;

#[cfg(test)]
#[path = "stream_processor_tests.rs"]
mod tests;
