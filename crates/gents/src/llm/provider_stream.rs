//! Moved to `gents-loop` (G-1): the rendered-request capturing transport
//! (`crate::rendered_request::transport`) needs this EOF guard with no
//! DefraDB dependency. Re-exported so `crate::llm::provider_stream` keeps
//! every symbol this crate's callers already use. Its test suite stays here:
//! one case drives a real `rig` OpenAI client over `inference_http`, native.
pub use gents_loop::provider_stream::*;

// Glue for the test suite below, which reaches `http_client` and
// `StreamingResponse` bare through `use super::*` the way it did before the
// move.
#[cfg(test)]
use futures::StreamExt;
#[cfg(test)]
use rig::http_client::{self, StreamingResponse};

#[cfg(test)]
mod tests;
