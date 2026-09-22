//! Shared provider-usage charge semantics, moved to `gents-loop` (G-1): the
//! loop's own aggregate-budget ledger needs the same charge arithmetic as
//! the durable `InferenceCall` write path, with no DefraDB dependency.
//! Re-exported so `crate::provider_usage` keeps every symbol this crate's
//! callers already use.
pub use gents_loop::provider_usage::*;
