use super::canonical_execution::ExecutionFuture;
use super::canonical_output::LeanCanonicalMessage;
use crate::generated::*;

pub mod native_adapter;

/// Deliberately not an adapter for provider_input::estimate_request.
pub trait PayloadPresentationAdapter {
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

pub async fn assert_native_payload_presentation_case<A>(
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
