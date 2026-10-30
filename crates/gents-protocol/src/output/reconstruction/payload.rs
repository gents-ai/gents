use super::extents::Extents;
use super::{invalid_extent, present_stream, ReconstructedStream};
use crate::message::DocumentSourceKind;
use crate::output::{
    MediaData, MediaKind, MediaType, PayloadPresentation, PayloadRef, PresentedPayload,
    ReconstructionError, StreamPayload,
};
use base64::Engine;

pub(super) fn expect_payload(
    stream: &ReconstructedStream,
    reference: &PayloadRef,
    allowed: &[fn(&StreamPayload) -> bool],
    presentation_is_full: bool,
    json: bool,
) -> Result<(), ReconstructionError> {
    if !allowed
        .iter()
        .any(|allowed| allowed(&stream.declaration.payload))
        || (!presentation_is_full
            && !matches!(
                stream.declaration.payload,
                StreamPayload::Text | StreamPayload::ToolOutput
            ))
    {
        return Err(invalid_extent(reference, stream.text.len() as u64));
    }
    if json && serde_json::from_str::<serde_json::Value>(&stream.text).is_err() {
        return Err(ReconstructionError::InvalidPayload {
            reference: reference.clone(),
        });
    }
    Ok(())
}

pub(super) fn is_text(payload: &StreamPayload) -> bool {
    matches!(payload, StreamPayload::Text)
}
pub(super) fn is_tool_output(payload: &StreamPayload) -> bool {
    matches!(payload, StreamPayload::ToolOutput)
}
pub(super) fn is_reasoning(payload: &StreamPayload) -> bool {
    matches!(payload, StreamPayload::Reasoning)
}
pub(super) fn is_reasoning_encrypted(payload: &StreamPayload) -> bool {
    matches!(payload, StreamPayload::ReasoningEncrypted)
}
pub(super) fn is_reasoning_redacted(payload: &StreamPayload) -> bool {
    matches!(payload, StreamPayload::ReasoningRedacted)
}
pub(super) fn is_reasoning_summary(payload: &StreamPayload) -> bool {
    matches!(payload, StreamPayload::ReasoningSummary)
}
pub(super) fn is_arguments(payload: &StreamPayload) -> bool {
    matches!(payload, StreamPayload::ToolArguments { .. })
}

pub(super) fn resolve_full(
    extents: &Extents<'_, '_>,
    reference: &PayloadRef,
    allowed: &[fn(&StreamPayload) -> bool],
    json: bool,
) -> Result<String, ReconstructionError> {
    let stream = extents.stream(reference)?;
    expect_payload(&stream, reference, allowed, true, json)?;
    Ok(stream.text)
}

pub(super) fn resolve_presented(
    extents: &Extents<'_, '_>,
    payload: &PresentedPayload,
    allowed: &[fn(&StreamPayload) -> bool],
) -> Result<String, ReconstructionError> {
    let stream = extents.stream(&payload.output)?;
    expect_payload(
        &stream,
        &payload.output,
        allowed,
        matches!(payload.presentation, PayloadPresentation::Full),
        false,
    )?;
    present_stream(&stream.text, payload)
}

fn media_data(
    extents: &Extents<'_, '_>,
    media: &crate::output::MediaBlock,
) -> Result<DocumentSourceKind, ReconstructionError> {
    let media_stream = |reference: &PayloadRef| {
        let stream = extents.stream(reference)?;
        match &stream.declaration.payload {
            StreamPayload::Media { media_kind } if media_kind == &media.kind => Ok(stream.text),
            _ => Err(invalid_extent(reference, stream.text.len() as u64)),
        }
    };
    match &media.data {
        MediaData::Url { url } => Ok(DocumentSourceKind::Url(url.clone())),
        MediaData::Unknown => Ok(DocumentSourceKind::Unknown),
        MediaData::Base64 { data } => Ok(DocumentSourceKind::Base64(media_stream(data)?)),
        MediaData::String { data } => Ok(DocumentSourceKind::String(media_stream(data)?)),
        MediaData::Raw { data } => {
            let encoded = media_stream(data)?;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| ReconstructionError::InvalidPayload {
                    reference: data.clone(),
                })?;
            Ok(DocumentSourceKind::Raw(decoded))
        }
    }
}

fn validate_media_shape(media: &crate::output::MediaBlock) -> bool {
    matches!(
        (&media.kind, &media.media_type, &media.detail),
        (MediaKind::Image, Some(MediaType::Image(_)) | None, _)
            | (MediaKind::Audio, Some(MediaType::Audio(_)) | None, None)
            | (MediaKind::Video, Some(MediaType::Video(_)) | None, None)
            | (
                MediaKind::Document,
                Some(MediaType::Document(_)) | None,
                None
            )
    )
}

pub(super) fn reconstruct_media(
    extents: &Extents<'_, '_>,
    media: &crate::output::MediaBlock,
) -> Result<(MediaKind, DocumentSourceKind), ReconstructionError> {
    if !validate_media_shape(media) {
        return Err(ReconstructionError::InvalidStructure {
            detail: "media kind, media type, and detail are not a native combination".to_owned(),
        });
    }
    Ok((media.kind, media_data(extents, media)?))
}
