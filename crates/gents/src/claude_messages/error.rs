//! The non-success Messages response as a provider error: status, `request-id`
//! and a bounded body prefix, never request headers.

use futures::StreamExt;
use rig::completion::CompletionError;
use rig::http_client;

pub(crate) const NON_SUCCESS_BODY_PREFIX_BYTES: usize = 512;

/// Non-2xx Messages response as a `ProviderError`. Carries the status, the
/// `request-id` header and a bounded body prefix (Anthropic's
/// `error.type`/`error.message`); never request headers.
pub(crate) fn non_success_error(
    status: reqwest::StatusCode,
    request_id: Option<&str>,
    body_prefix: &str,
) -> CompletionError {
    CompletionError::ProviderError(non_success_message(status, request_id, body_prefix))
}

/// The text of [`non_success_error`], for callers that annotate it before wrapping.
pub(crate) fn non_success_message(
    status: reqwest::StatusCode,
    request_id: Option<&str>,
    body_prefix: &str,
) -> String {
    let mut message = format!(
        "Claude Messages HTTP {status} (request-id {})",
        request_id.unwrap_or("-")
    );
    if !body_prefix.is_empty() {
        message.push_str(" body=");
        message.push_str(body_prefix);
    }
    message
}

pub(crate) fn body_prefix(bytes: &[u8]) -> String {
    let end = bytes.len().min(NON_SUCCESS_BODY_PREFIX_BYTES);
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

pub(super) async fn read_body_prefix(mut body: http_client::sse::BoxedStream) -> String {
    let mut bytes: Vec<u8> = Vec::new();
    while bytes.len() < NON_SUCCESS_BODY_PREFIX_BYTES {
        match body.next().await {
            Some(Ok(chunk)) => bytes.extend_from_slice(&chunk),
            Some(Err(_)) | None => break,
        }
    }
    body_prefix(&bytes)
}
