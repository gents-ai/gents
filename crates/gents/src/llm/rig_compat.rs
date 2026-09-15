//! Moved to `gents-loop` (G-1): the rig <-> native message conversion
//! boundary the owned loop uses has no DefraDB dependency. Re-exported so
//! `crate::llm::rig_compat` keeps every symbol this crate's callers already
//! use.
pub use gents_loop::rig_compat::*;
