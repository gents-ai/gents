//! Moved to `gents-loop` (G-1): the tool vocabulary (`Tool`, `ToolDyn`,
//! `ToolError`, argument repair) has no DefraDB dependency. Re-exported so
//! `crate::llm::tool` keeps every symbol this crate's callers already use.
pub use gents_loop::tool::*;
