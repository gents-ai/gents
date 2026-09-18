//! Moved to `gents-loop` (G-1): `provider_input`'s Responses-wire projection
//! needs this normalization with no DefraDB dependency. Re-exported so
//! `crate::llm::responses_normalize` keeps every symbol this crate's
//! callers already use.
pub use gents_loop::responses_normalize::*;
