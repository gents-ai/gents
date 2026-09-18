//! Completion retry/retract decision engine, moved to `gents-loop` (G-1):
//! the owned loop (`agent/loop_stream.rs`) drives every transition here, with
//! no DefraDB dependency. Re-exported so `crate::agent::completion_retry`
//! keeps every symbol this crate's callers already use.
pub use gents_loop::completion_retry::*;
