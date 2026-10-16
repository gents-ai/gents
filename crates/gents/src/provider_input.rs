//! Provider-shaped preflight accounting moved to `gents-loop` (G-1);
//! re-exported so `crate::provider_input` keeps every symbol this crate's
//! callers already use. The end-to-end tests that exercise the real transport
//! clients (`chatgpt_codex`, `xai_grok_oauth`, `inference_http`) stay here.
pub use gents_loop::provider_input::*;

pub(crate) mod replay;

#[cfg(test)]
use crate::backend_provider::BackendProviderKind;
#[cfg(test)]
use crate::openai_wire::OpenAiWireApi;
#[cfg(test)]
use serde_json::Value;

#[cfg(test)]
mod tests;
