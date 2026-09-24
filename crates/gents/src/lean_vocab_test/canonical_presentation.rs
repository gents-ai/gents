use serde::Deserialize;

use super::canonical_execution::ExecutionFuture;
use super::canonical_output::{LeanCanonicalMessage, LeanCanonicalSegment, LeanPayloadSpec};

#[cfg(test)]
#[path = "canonical_presentation/native_adapter.rs"]
mod native_adapter;

/// Payload presentation before provider-specific projection. These byte counts
/// exclude metadata, escaping, media URLs and request structure; they are neither
/// serialized request sizes nor token estimates. The existing provider_input
/// owner must still project and estimate the complete request.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanPayloadPresentationCase {
    pub(crate) name: String,
    pub(crate) segments: Vec<LeanCanonicalSegment>,
    pub(crate) message: LeanCanonicalMessage<LeanPayloadSpec>,
    /// `None` when the message cannot be reconstructed.
    pub(crate) expected: Option<LeanPayloadLengths>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanPayloadLengths {
    pub(crate) stored_payload_bytes: u64,
    pub(crate) presented_payload_bytes: u64,
}

/// Observe reconstructed payload fields at the native reconstruction boundary.
/// This is deliberately not an adapter for provider_input::estimate_request.
pub(crate) trait PayloadPresentationAdapter {
    type Error: std::fmt::Display;

    /// Report the sum of reconstructed payload-field bytes (including selected
    /// literal markers). Return None for an unreconstructable message, not a
    /// header-only or stored-stream fallback. Exclude inline metadata and URLs.
    fn presented_payload_bytes<'a>(
        &'a mut self,
        segments: &'a [LeanCanonicalSegment],
        message: &'a LeanCanonicalMessage<LeanPayloadSpec>,
    ) -> ExecutionFuture<'a, Result<Option<u64>, Self::Error>>;
}

/// Check presentation length, independently of provider serialization/sizing.
pub(crate) async fn assert_native_payload_presentation_case<A>(
    case: &LeanPayloadPresentationCase,
    adapter: &mut A,
) -> Result<(), String>
where
    A: PayloadPresentationAdapter,
{
    let name = &case.name;
    let actual = adapter
        .presented_payload_bytes(&case.segments, &case.message)
        .await
        .map_err(|error| format!("{name}: native presentation adapter failed: {error}"))?;
    match (case.expected, actual) {
        (None, None) => Ok(()),
        (None, Some(bytes)) => Err(format!(
            "{name}: measured an unreconstructable payload as {bytes} bytes"
        )),
        (Some(expected), None) => Err(format!(
            "{name}: refused a reconstructable message, expected {} presented payload bytes",
            expected.presented_payload_bytes
        )),
        (Some(expected), Some(bytes)) if bytes == expected.presented_payload_bytes => Ok(()),
        (Some(expected), Some(bytes))
            if bytes == expected.stored_payload_bytes
                && expected.stored_payload_bytes != expected.presented_payload_bytes =>
        {
            Err(format!(
                "{name}: observed stored payload length ({bytes}) instead of presented payload length ({})",
                expected.presented_payload_bytes
            ))
        }
        (Some(expected), Some(bytes)) => Err(format!(
            "{name}: expected {} presented payload bytes, got {bytes}",
            expected.presented_payload_bytes
        )),
    }
}
