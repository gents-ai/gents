//! The cloud gateway sign-in protocol of Claude Code 2.1.311 (`chunk-tz7haycp.js`,
//! refresh `WK` in `chunk-v2tvd4xz.js`), as pure functions over URLs and bodies.
//!
//! A gateway advertises its endpoints in its OAuth authorization server
//! metadata; only endpoints on the gateway's own origin are used, else the
//! `/oauth/device_authorization` and `/oauth/token` defaults. Sign-in is the
//! OAuth device authorization grant with the form `surface=claude_code`; the
//! token endpoint is polled with the device-code grant after waiting the
//! advertised interval (five seconds by default, five more after each
//! `slow_down`). The answer carries the gateway JWT, its lifetime, an optional
//! identity-provider refresh token and an optional email. A token is refreshed
//! with the `refresh_token` grant at the same endpoint once it is within five
//! minutes of expiry. The I/O lives with the caller.

use serde::Deserialize;

use crate::profile;

/// The `surface` form field of the device authorization request.
pub const SURFACE: &str = "claude_code";
/// The device-code grant of the token polls.
pub const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// Where a gateway publishes its endpoints.
pub const METADATA_PATH: &str = "/.well-known/oauth-authorization-server";
const DEFAULT_DEVICE_PATH: &str = "/oauth/device_authorization";
const DEFAULT_TOKEN_PATH: &str = "/oauth/token";
/// Bound on every gateway sign-in and refresh request.
pub const REQUEST_TIMEOUT_MS: u64 = 10_000;
/// Poll interval when the gateway names none.
pub const DEFAULT_INTERVAL_SECS: u64 = 5;
/// Seconds added to the interval after a `slow_down`.
pub const SLOW_DOWN_SECS: u64 = 5;
/// A token is refreshed once it is this close to expiry (`$K`).
pub const REFRESH_LEAD_MS: u64 = 300_000;
/// Longest email kept for display, in characters.
pub const EMAIL_CAP: usize = 254;

/// The `User-Agent` of gateway sign-in and refresh requests (`Bs`).
pub fn user_agent() -> String {
    format!("claude-code/{}", profile::VERSION)
}

/// Why a gateway answer cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GatewayError {
    /// The gateway URL is not an absolute http or https URL.
    #[error("the gateway URL must be an http or https URL")]
    Url,
    /// The device authorization answer is not the expected JSON.
    #[error("gateway device authorization endpoint returned malformed response")]
    DeviceCode,
    /// The token answer is not the expected JSON.
    #[error("gateway token endpoint returned malformed response")]
    Tokens,
}

/// The endpoints sign-in and refresh use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    pub device_authorization: String,
    pub token: String,
    /// Only a same-origin revocation endpoint is kept.
    pub revocation: Option<String>,
}

/// The gateway URL without a trailing slash, checked to be absolute http(s).
pub fn base_url(url: &str) -> Result<String, GatewayError> {
    let parsed = url::Url::parse(url.trim()).map_err(|_| GatewayError::Url)?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(GatewayError::Url);
    }
    Ok(url.trim().trim_end_matches('/').to_owned())
}

/// The metadata URL of `base`.
pub fn metadata_url(base: &str) -> String {
    format!("{base}{METADATA_PATH}")
}

/// `candidate` when it shares `base`'s origin (`X3r`).
fn same_origin(base: &str, candidate: Option<&str>) -> Option<String> {
    let candidate = candidate?;
    let origin = |text: &str| url::Url::parse(text).ok().map(|url| url.origin());
    (origin(candidate)? == origin(base)?).then(|| candidate.to_owned())
}

/// The endpoints of `base` given its metadata body, if one was read; an
/// unreadable or foreign-origin entry falls back to the default path (`Ft`).
pub fn endpoints(base: &str, metadata: Option<&[u8]>) -> Endpoints {
    #[derive(Deserialize, Default)]
    struct Metadata {
        device_authorization_endpoint: Option<String>,
        token_endpoint: Option<String>,
        revocation_endpoint: Option<String>,
    }
    let metadata: Metadata = metadata
        .and_then(|body| serde_json::from_slice(body).ok())
        .unwrap_or_default();
    let pick = |advertised: Option<&str>, default: &str| {
        same_origin(base, advertised).unwrap_or_else(|| format!("{base}{default}"))
    };
    Endpoints {
        device_authorization: pick(
            metadata.device_authorization_endpoint.as_deref(),
            DEFAULT_DEVICE_PATH,
        ),
        token: pick(metadata.token_endpoint.as_deref(), DEFAULT_TOKEN_PATH),
        revocation: same_origin(base, metadata.revocation_endpoint.as_deref()),
    }
}

/// A device authorization answer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    #[serde(default)]
    pub interval: Option<u64>,
}

impl DeviceCode {
    /// The page the person opens: the complete URI when the gateway gives one.
    pub fn open_url(&self) -> &str {
        self.verification_uri_complete
            .as_deref()
            .filter(|uri| !uri.is_empty())
            .unwrap_or(&self.verification_uri)
    }

    /// The first wait between polls, at least one second.
    pub fn interval_secs(&self) -> u64 {
        self.interval.unwrap_or(DEFAULT_INTERVAL_SECS).max(1)
    }
}

/// Parses a device authorization answer.
pub fn parse_device_code(body: &[u8]) -> Result<DeviceCode, GatewayError> {
    serde_json::from_slice(body).map_err(|_| GatewayError::DeviceCode)
}

/// A token answer (`uXo`). The secrets never print.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub expires_in: u64,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default, deserialize_with = "lenient_string")]
    pub email: Option<String>,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("expires_in", &self.expires_in)
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[redacted]"),
            )
            .field("email", &self.email)
            .finish_non_exhaustive()
    }
}

/// `email: ge().nullish().catch(void 0)`: anything but a string is absent.
fn lenient_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value.as_str().map(str::to_owned))
}

/// Parses a token answer.
pub fn parse_tokens(body: &[u8]) -> Result<Tokens, GatewayError> {
    serde_json::from_slice(body).map_err(|_| GatewayError::Tokens)
}

/// What a refused token poll means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll {
    /// Not approved yet: wait and poll again.
    Pending,
    /// Poll less often: add [`SLOW_DOWN_SECS`] to the interval.
    SlowDown,
    /// The device code expired before approval.
    Expired,
    /// The person denied the sign-in.
    Denied,
    /// Any other refusal, with the gateway's error code when it gave one.
    Failed(Option<String>),
}

/// The meaning of a refused poll's body (`J3r` on the OAuth `error` field).
pub fn poll_outcome(body: &[u8]) -> Poll {
    #[derive(Deserialize)]
    struct Refusal {
        error: Option<String>,
    }
    match serde_json::from_slice::<Refusal>(body)
        .ok()
        .and_then(|refusal| refusal.error)
        .as_deref()
    {
        Some("authorization_pending") => Poll::Pending,
        Some("slow_down") => Poll::SlowDown,
        Some("expired_token") => Poll::Expired,
        Some("access_denied") => Poll::Denied,
        other => Poll::Failed(other.map(str::to_owned)),
    }
}

/// The email as Claude Code keeps it: control characters dropped, cut to
/// [`EMAIL_CAP`] characters with an ellipsis.
pub fn display_email(email: &str) -> Option<String> {
    let clean: String = email.chars().filter(|c| !c.is_control()).collect();
    if clean.is_empty() {
        return None;
    }
    Some(if clean.chars().count() > EMAIL_CAP {
        let cut: String = clean.chars().take(EMAIL_CAP).collect();
        format!("{cut}\u{2026}")
    } else {
        clean
    })
}

/// The device authorization form.
pub fn device_form() -> [(&'static str, &'static str); 1] {
    [("surface", SURFACE)]
}

/// The token poll form for `device_code`.
pub fn poll_form(device_code: &str) -> [(&'static str, &str); 2] {
    [
        ("grant_type", DEVICE_CODE_GRANT),
        ("device_code", device_code),
    ]
}

/// The refresh form for `refresh_token`.
pub fn refresh_form(refresh_token: &str) -> [(&'static str, &str); 2] {
    [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
    ]
}

#[cfg(test)]
#[path = "gateway_tests.rs"]
mod tests;
