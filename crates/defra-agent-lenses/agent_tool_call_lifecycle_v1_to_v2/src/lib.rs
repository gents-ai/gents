//! WASM Lens migration: AgentToolCall v1 -> v2.
//!
//! Computes `lifecycle_state` from the legacy `status` and `tool_failure_class`
//! fields, and rebuckets `tool_failure_class` from the 12-variant Rust
//! vocabulary to the 5-variant Lean vocabulary. Inverse drops the
//! `lifecycle_state` field for v2->v1 reads on a v1 peer.
//!
//! Implementation lives in subsequent tasks; this is the crate scaffold.

// Placeholder: real transform logic lands in Task 5.
