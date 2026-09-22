//! Domain-specific error types for the Gents runtime, moved to `gents-loop`
//! (G-1): the owned loop classifies completion failures with no DefraDB
//! dependency. Re-exported here so `crate::error` keeps every symbol this
//! crate's callers already use.
pub use gents_loop::error::*;
