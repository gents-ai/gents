//! Provider-shaped preflight accounting moved to `gents-loop` (G-1): the
//! loop's own dispatch needs the exact projected byte size the provider
//! client would send, with no native dependency. Re-exported here so
//! `crate::provider_input` keeps every symbol this crate's callers already
//! use. The end-to-end tests that exercise the *real* transport clients
//! (`chatgpt_codex`, `xai_grok_oauth`, `inference_http`) stay here, native.
pub use gents_loop::provider_input::*;

// Glue for the test suite below, which reaches these bare through
// `use super::*` the way it did before the move (gents-loop's own
// `provider_input` module imports them privately, so they do not ride the
// glob re-export above).
#[cfg(test)]
use crate::backend_provider::BackendProviderKind;
#[cfg(test)]
use crate::openai_wire::OpenAiWireApi;
#[cfg(test)]
use serde_json::Value;

#[cfg(test)]
mod tests;
