//! The Google Vertex AI rules of Claude Code 2.1.311 that are not shared with
//! the other platforms: the `streamRawPredict` path that names the project,
//! region and model, and the body rewrite of the `AnthropicVertex` middleware
//! (`chunk-ezwqdctg.js`): `anthropic_version` appended when absent, `model`
//! removed, `stream` kept. The `anthropic-beta` header is unchanged and the
//! answer is the Messages SSE stream.

use serde_json::value::RawValue;

use crate::wire::{RawObject, WireError};

/// The `anthropic_version` Vertex takes in the body.
pub const ANTHROPIC_VERSION: &str = "vertex-2023-10-16";

/// The region Claude Code uses when none is configured (`d8t`).
pub const DEFAULT_REGION: &str = "us-east5";

/// Why a Vertex body could not be rewritten.
#[derive(Debug, thiserror::Error)]
pub enum VertexError {
    /// The planned body is not a JSON object.
    #[error("the planned Messages body cannot be rewritten for Vertex AI: {0}")]
    Body(#[from] WireError),
    /// The planned body is not UTF-8.
    #[error("the planned Messages body is not valid UTF-8")]
    BodyNotUtf8,
}

/// The WHATWG URL path percent-encode set, which the `pathname` setter applies
/// to the interpolated values: controls, space, `"#<>?\`{}` and non-ASCII.
fn escape_path(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte <= 0x20
            || byte >= 0x7f
            || matches!(byte, b'"' | b'#' | b'<' | b'>' | b'?' | b'`' | b'{' | b'}')
        {
            out.push_str(&format!("%{byte:02X}"));
        } else {
            out.push(char::from(byte));
        }
    }
    out
}

/// The streaming predict path under the `/v1` base:
/// `/projects/<project>/locations/<region>/publishers/anthropic/models/<model>:streamRawPredict`.
pub fn stream_path(project: &str, region: &str, model: &str) -> String {
    format!(
        "/projects/{}/locations/{}/publishers/anthropic/models/{}:streamRawPredict",
        escape_path(project),
        escape_path(region),
        escape_path(model)
    )
}

/// Rewrites a planned Messages body for Vertex: `anthropic_version` is appended
/// when absent and `model` is removed; every other key keeps its place.
pub fn transform_body(body: &[u8]) -> Result<Vec<u8>, VertexError> {
    let text = std::str::from_utf8(body).map_err(|_| VertexError::BodyNotUtf8)?;
    let mut object = RawObject::from_json(text)?;
    let wire = |error| VertexError::Body(WireError::Serialize(error));
    if !object.contains("anthropic_version") {
        object.push(
            "anthropic_version",
            RawValue::from_string(format!("\"{ANTHROPIC_VERSION}\"")).map_err(wire)?,
        );
    }
    object.remove("model");
    serde_json::to_vec(&object).map_err(wire)
}

#[cfg(test)]
#[path = "vertex_tests.rs"]
mod tests;
