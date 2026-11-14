//! AWS Signature Version 4 signing of an outgoing Claude request: the headers
//! the client built and the body bytes go to the shared signer, and the
//! signature headers come back to be written with them. One function serves the
//! Messages requests (Bedrock, Mantle, Claude Platform on AWS) and the Bedrock
//! control-plane calls, so the signed set and the written set cannot drift.
//! The URL parts come from the transport (`url_parts`), the owner of how the
//! request line and `Host` are written.

use std::time::{SystemTime, UNIX_EPOCH};

use gents_claude::headers::HeaderValue;
use gents_claude::sigv4::{query_pairs, Request as SigRequest, Scope, SigV4Error};

use super::route::AwsSigning;
use super::transport::UrlParts;

/// The headers `signing` adds to a `method` request to `url` carrying `headers`
/// and `body`: `host` and the signature headers, which the SDK's
/// `getAuthHeaders` merges into the request, so the transport writes `Host`
/// among the request headers rather than after them. `headers` are exactly
/// those the transport will write, so anything the client adds after signing
/// is left out of the call.
pub(crate) fn sign_request(
    signing: &AwsSigning,
    method: &str,
    url: &UrlParts,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<Vec<(&'static str, HeaderValue)>, SigV4Error> {
    let query = query_pairs(&url.query);
    let query: Vec<(&str, &str)> = query
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let mut added = vec![(
        "host",
        HeaderValue {
            value: url.host.clone(),
            sensitive: false,
        },
    )];
    added.extend(signing.signer.sign(
        &SigRequest {
            method,
            path: &url.path,
            query: &query,
            host: &url.host,
            headers,
            body,
        },
        &Scope {
            region: &signing.region,
            service: signing.service,
        },
        now,
    )?);
    Ok(added)
}
