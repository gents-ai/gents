//! The Claude Code 2.1.311 Messages API, reproduced byte for byte.
//!
//! A request rendered here is the request Claude Code 2.1.311 renders for the
//! same logical input: the attribution and identity system blocks, the cache
//! breakpoints, `metadata.user_id`, the beta set, the header set and the JSON
//! key order. Gents supplies the conversation, tools and account; this crate
//! owns the protocol around them, the OAuth protocol of Claude Code's client,
//! and the account rotation that spreads requests over several subscriptions.
//!
//! The provider-input rules are modeled in the Lean proofs
//! (`PromptAssembly.ClaudeMap`, `ClaudeAttribution`, `ClaudeCache`,
//! `ClaudeOrigin`, `ClaudeBody`, `AccountRotation`, `CompletionRetry`) and bound by their generated
//! conformance cases. The default build performs no I/O and compiles for
//! `wasm32-wasip1`; the `transport` feature adds the native HTTP/1.1 client.

pub mod attribution;
pub mod betas;
pub mod body;
pub mod cache;
pub mod catalog;
pub mod headers;
pub mod metadata;
pub mod oauth;
pub mod origin;
pub mod pool;
pub mod profile;
pub mod ratelimit;
pub mod request;
pub mod retry;
pub mod rotation;
pub mod sse;
pub mod system;
#[cfg(feature = "transport")]
pub mod transport;
pub mod wire;
