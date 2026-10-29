//! The OAuth protocol of Claude Code's client: PKCE, the authorize URL, token
//! exchange, refresh, revoke and their error classes (`chunk-v2tvd4xz.js`,
//! `chunk-bz23k6kc.js`).

use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use url::Url;

/// Public OAuth client id (`CLIENT_ID`).
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// Console authorize endpoint.
pub const CONSOLE_AUTHORIZE_URL: &str = "https://platform.claude.com/oauth/authorize";
/// claude.ai authorize endpoint.
pub const CLAUDE_AI_AUTHORIZE_URL: &str = "https://claude.com/cai/oauth/authorize";
/// Token endpoint for exchange, refresh and (with `/revoke`) revoke.
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// Redirect of the manual (paste the code) flow.
pub const MANUAL_REDIRECT_URL: &str = "https://platform.claude.com/oauth/code/callback";
/// Profile endpoint.
pub const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
/// API key creation endpoint.
pub const API_KEY_URL: &str = "https://api.anthropic.com/api/oauth/claude_cli/create_api_key";
/// Roles endpoint.
pub const ROLES_URL: &str = "https://api.anthropic.com/api/oauth/claude_cli/roles";
/// `anthropic-beta` value of OAuth-authenticated calls.
pub const OAUTH_BETA: &str = "oauth-2025-04-20";
/// Timeout of the token exchange and refresh calls.
pub const EXCHANGE_TIMEOUT_MS: u64 = 30_000;
/// Timeout of the revoke call.
pub const REVOKE_TIMEOUT_MS: u64 = 5_000;

/// Scopes requested on refresh when the caller has no previously granted list (`Sxt`).
pub const REFRESH_SCOPES: [&str; 6] = [
    "user:profile",
    "user:inference",
    "user:sessions:claude_code",
    "user:mcp_servers",
    "user:file_upload",
    "user:plugins",
];
/// Scopes requested at login (`deo`).
pub const LOGIN_SCOPES: [&str; 7] = [
    "org:create_api_key",
    "user:profile",
    "user:inference",
    "user:sessions:claude_code",
    "user:mcp_servers",
    "user:file_upload",
    "user:plugins",
];
/// The only scope of an inference-only login.
pub const INFERENCE_SCOPE: &str = "user:inference";

/// Why an OAuth request or response could not be handled.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OAuthError {
    /// An endpoint URL did not parse.
    #[error("the OAuth endpoint URL is invalid")]
    InvalidUrl,
    /// The token response is not the expected JSON; names the failing category.
    #[error("the token response is malformed ({0})")]
    Malformed(&'static str),
    /// The granted lifetime does not fit a millisecond timestamp.
    #[error("the token lifetime is out of range")]
    ExpiryOutOfRange,
}

/// A JSON POST ready to send: URL, exact body bytes and timeout.
#[derive(Clone, PartialEq, Eq)]
pub struct JsonPost {
    /// Target URL.
    pub url: String,
    /// Exact JSON body; carries secrets.
    pub body: String,
    /// Request timeout in milliseconds.
    pub timeout_ms: u64,
}

impl fmt::Debug for JsonPost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JsonPost")
            .field("url", &self.url)
            .field("body", &"<redacted>")
            .field("timeout_ms", &self.timeout_ms)
            .finish()
    }
}

/// A PKCE verifier and its S256 challenge.
#[derive(Clone, PartialEq, Eq)]
pub struct Pkce {
    /// `code_verifier`; a secret.
    pub verifier: String,
    /// `code_challenge`.
    pub challenge: String,
}

impl fmt::Debug for Pkce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pkce")
            .field("verifier", &"<redacted>")
            .field("challenge", &self.challenge)
            .finish()
    }
}

/// PKCE pair from 32 caller-supplied random bytes.
pub fn pkce(verifier_bytes: [u8; 32]) -> Pkce {
    let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    Pkce {
        verifier,
        challenge,
    }
}

/// The `state` parameter from 32 caller-supplied random bytes.
pub fn state(bytes: [u8; 32]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// A custom OAuth client (`oauthClient`) replacing the default id and scopes.
#[derive(Debug, Clone, Copy)]
pub struct ClientOverride<'a> {
    /// Replacement client id; the default when `None`.
    pub client_id: Option<&'a str>,
    /// Scopes requested instead of the login scopes.
    pub scopes: &'a [String],
}

/// Inputs of the authorize URL (`s3r`).
#[derive(Debug, Clone, Copy)]
pub struct AuthorizeRequest<'a> {
    /// PKCE `code_challenge`.
    pub code_challenge: &'a str,
    /// Anti-forgery `state`.
    pub state: &'a str,
    /// Loopback callback port, unused when `manual`.
    pub port: u16,
    /// Manual flow: redirect to the hosted code page.
    pub manual: bool,
    /// Sign in through claude.ai rather than the console.
    pub login_with_claude_ai: bool,
    /// Request only `user:inference`.
    pub inference_only: bool,
    /// Pre-selected organization.
    pub org_uuid: Option<&'a str>,
    /// `login_hint`.
    pub login_hint: Option<&'a str>,
    /// `login_method`.
    pub login_method: Option<&'a str>,
    /// Custom client.
    pub client: Option<ClientOverride<'a>>,
    /// One scope appended to the login scopes.
    pub also_request_scope: Option<&'a str>,
}

/// The authorize URL, byte for byte what Claude Code opens.
pub fn authorize_url(request: &AuthorizeRequest<'_>) -> Result<String, OAuthError> {
    let base = if request.login_with_claude_ai {
        CLAUDE_AI_AUTHORIZE_URL
    } else {
        CONSOLE_AUTHORIZE_URL
    };
    let mut url = Url::parse(base).map_err(|_| OAuthError::InvalidUrl)?;
    let redirect = if request.manual {
        MANUAL_REDIRECT_URL.to_owned()
    } else {
        format!("http://localhost:{}/callback", request.port)
    };
    let scope = match (
        request.client,
        request.inference_only,
        request.also_request_scope,
    ) {
        (Some(client), _, _) => client.scopes.join(" "),
        (None, true, _) => INFERENCE_SCOPE.to_owned(),
        (None, false, Some(extra)) => format!("{} {extra}", LOGIN_SCOPES.join(" ")),
        (None, false, None) => LOGIN_SCOPES.join(" "),
    };
    let client_id = request
        .client
        .and_then(|c| c.client_id)
        .unwrap_or(CLIENT_ID);
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("code", "true")
            .append_pair("client_id", client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &redirect)
            .append_pair("scope", &scope)
            .append_pair("code_challenge", request.code_challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", request.state);
        for (key, value) in [
            ("orgUUID", request.org_uuid),
            ("login_hint", request.login_hint),
            ("login_method", request.login_method),
        ] {
            if let Some(value) = value.filter(|v| !v.is_empty()) {
                query.append_pair(key, value);
            }
        }
    }
    Ok(url.into())
}

/// Inputs of the authorization-code exchange (`c9o`).
#[derive(Debug, Clone, Copy)]
pub struct ExchangeRequest<'a> {
    /// Authorization code from the callback.
    pub code: &'a str,
    /// The `state` sent at authorize time.
    pub state: &'a str,
    /// The PKCE verifier.
    pub code_verifier: &'a str,
    /// Loopback callback port, unused when `manual`.
    pub port: u16,
    /// Manual flow redirect.
    pub manual: bool,
    /// Requested token lifetime in seconds.
    pub expires_in: Option<u64>,
    /// Custom client id.
    pub client_id: Option<&'a str>,
}

fn post(url: &str, body: Map<String, Value>, timeout_ms: u64) -> JsonPost {
    JsonPost {
        url: url.to_owned(),
        // A map of strings and integers always serializes.
        body: Value::Object(body).to_string(),
        timeout_ms,
    }
}

fn object<const N: usize>(entries: [(&str, Value); N]) -> Map<String, Value> {
    entries
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect()
}

/// The token exchange POST.
pub fn exchange_request(request: &ExchangeRequest<'_>) -> JsonPost {
    let redirect = if request.manual {
        MANUAL_REDIRECT_URL.to_owned()
    } else {
        format!("http://localhost:{}/callback", request.port)
    };
    let mut body = object([
        ("grant_type", json!("authorization_code")),
        ("code", json!(request.code)),
        ("redirect_uri", json!(redirect)),
        ("client_id", json!(request.client_id.unwrap_or(CLIENT_ID))),
        ("code_verifier", json!(request.code_verifier)),
        ("state", json!(request.state)),
    ]);
    if let Some(seconds) = request.expires_in {
        body.insert("expires_in".into(), json!(seconds));
    }
    post(TOKEN_URL, body, EXCHANGE_TIMEOUT_MS)
}

/// Inputs of the refresh call (`Lse`).
#[derive(Debug, Clone, Copy)]
pub struct RefreshRequest<'a> {
    /// The refresh token.
    pub refresh_token: &'a str,
    /// Previously granted scopes; the default refresh scopes when empty.
    pub scopes: &'a [String],
    /// Requested token lifetime in seconds.
    pub expires_in: Option<u64>,
    /// Custom client id.
    pub client_id: Option<&'a str>,
}

/// The refresh POST.
pub fn refresh_request(request: &RefreshRequest<'_>) -> JsonPost {
    let scope = if request.scopes.is_empty() {
        REFRESH_SCOPES.join(" ")
    } else {
        request.scopes.join(" ")
    };
    let mut body = object([
        ("grant_type", json!("refresh_token")),
        ("refresh_token", json!(request.refresh_token)),
        ("client_id", json!(request.client_id.unwrap_or(CLIENT_ID))),
        ("scope", json!(scope)),
    ]);
    if let Some(seconds) = request.expires_in {
        body.insert("expires_in".into(), json!(seconds));
    }
    post(TOKEN_URL, body, EXCHANGE_TIMEOUT_MS)
}

/// The revoke POST (`i3r`).
pub fn revoke_request(refresh_token: &str, client_id: Option<&str>) -> JsonPost {
    let body = object([
        ("token", json!(refresh_token)),
        ("token_type_hint", json!("refresh_token")),
        ("client_id", json!(client_id.unwrap_or(CLIENT_ID))),
    ]);
    post(&format!("{TOKEN_URL}/revoke"), body, REVOKE_TIMEOUT_MS)
}

/// The account a token was issued to.
#[derive(Clone, PartialEq, Eq)]
pub struct TokenAccount {
    /// Account UUID.
    pub uuid: String,
    /// Account email address.
    pub email_address: String,
}

impl fmt::Debug for TokenAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenAccount")
            .field("uuid", &self.uuid)
            .field("email_address", &"<redacted>")
            .finish()
    }
}

/// A parsed token response.
#[derive(Clone, PartialEq, Eq)]
pub struct Tokens {
    /// Bearer access token.
    pub access_token: String,
    /// Refresh token; on refresh the previous one when the response omits it.
    pub refresh_token: Option<String>,
    /// Access token expiry, milliseconds since the Unix epoch.
    pub expires_at_ms: u64,
    /// Refresh token expiry when the response states one.
    pub refresh_token_expires_at_ms: Option<u64>,
    /// Granted scopes.
    pub scopes: Vec<String>,
    /// Issuing account.
    pub account: Option<TokenAccount>,
    /// Organization UUID.
    pub organization_uuid: Option<String>,
}

impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tokens")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_at_ms", &self.expires_at_ms)
            .field(
                "refresh_token_expires_at_ms",
                &self.refresh_token_expires_at_ms,
            )
            .field("scopes", &self.scopes)
            .field("account", &self.account)
            .field("organization_uuid", &self.organization_uuid)
            .finish()
    }
}

#[derive(Deserialize)]
struct WireAccount {
    uuid: String,
    email_address: String,
}

#[derive(Deserialize)]
struct WireOrganization {
    uuid: String,
}

#[derive(Deserialize)]
struct WireTokens {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
    refresh_token_expires_in: Option<u64>,
    scope: Option<String>,
    account: Option<WireAccount>,
    organization: Option<WireOrganization>,
}

fn expiry(now_ms: u64, seconds: u64) -> Result<u64, OAuthError> {
    seconds
        .checked_mul(1000)
        .and_then(|ms| now_ms.checked_add(ms))
        .ok_or(OAuthError::ExpiryOutOfRange)
}

/// Parses a token response; `previous_refresh_token` is kept when the response omits one.
pub fn parse_token_response(
    body: &[u8],
    now_ms: u64,
    previous_refresh_token: Option<&str>,
) -> Result<Tokens, OAuthError> {
    let wire: WireTokens = serde_json::from_slice(body).map_err(|e| {
        OAuthError::Malformed(match e.classify() {
            serde_json::error::Category::Io => "io",
            serde_json::error::Category::Syntax => "syntax",
            serde_json::error::Category::Data => "fields",
            serde_json::error::Category::Eof => "truncated",
        })
    })?;
    Ok(Tokens {
        expires_at_ms: expiry(now_ms, wire.expires_in)?,
        refresh_token_expires_at_ms: wire
            .refresh_token_expires_in
            .map(|seconds| expiry(now_ms, seconds))
            .transpose()?,
        refresh_token: wire
            .refresh_token
            .or_else(|| previous_refresh_token.map(str::to_owned)),
        scopes: wire
            .scope
            .as_deref()
            .map(|scope| {
                scope
                    .split(' ')
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        account: wire.account.map(|a| TokenAccount {
            uuid: a.uuid,
            email_address: a.email_address,
        }),
        organization_uuid: wire.organization.map(|o| o.uuid),
        access_token: wire.access_token,
    })
}

/// How a failed token endpoint call must be handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshFailure {
    /// `invalid_scope` on refresh: retry exactly once with the previously granted scopes.
    InvalidScope,
    /// The account is on hold: keep the refresh token and surface a re-login.
    AccountOnHold,
    /// The refresh token is no longer valid: the account is signed out.
    SignedOut,
    /// A permanent client error that retrying cannot fix.
    Rejected,
    /// Network, server or throttling failure: retry later.
    Transient,
}

const ACCOUNT_ON_HOLD: &str = "account_on_hold";

/// `Ha`: the OAuth error code (a string `error` or an object's `type`) and description.
fn error_parts(value: &Value) -> (Option<&str>, Option<&str>) {
    let code = match value.get("error") {
        Some(Value::String(code)) => Some(code.as_str()),
        Some(Value::Object(error)) => error.get("type").and_then(Value::as_str),
        _ => None,
    };
    (code, value.get("error_description").and_then(Value::as_str))
}

/// `mCn`: the body is the account-on-hold error, as a JSON object or a JSON-encoded string.
fn is_account_on_hold(body: &[u8]) -> bool {
    let Ok(mut value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    if let Value::String(text) = &value {
        if !text.contains(ACCOUNT_ON_HOLD) {
            return false;
        }
        let Ok(inner) = serde_json::from_str(text) else {
            return false;
        };
        value = inner;
    }
    let (code, description) = error_parts(&value);
    matches!(code, Some("invalid_grant" | "access_denied"))
        && description == Some(ACCOUNT_ON_HOLD)
        && value.get("error_uri").is_none_or(Value::is_string)
}

/// Classifies a failed token call from its HTTP status (`None` for a network failure) and body.
pub fn classify_failure(status: Option<u16>, body: &[u8]) -> RefreshFailure {
    let Some(status) = status else {
        return RefreshFailure::Transient;
    };
    if matches!(status, 400 | 401 | 403) && is_account_on_hold(body) {
        return RefreshFailure::AccountOnHold;
    }
    let value = serde_json::from_slice::<Value>(body).unwrap_or(Value::Null);
    let code = error_parts(&value).0;
    if matches!(status, 400 | 401) && code == Some("invalid_grant") {
        return RefreshFailure::SignedOut;
    }
    if status == 400 && code == Some("invalid_scope") {
        return RefreshFailure::InvalidScope;
    }
    // `Nae`: a 4xx other than 408, 409 and 429 is permanent.
    if (400..500).contains(&status) && !matches!(status, 408 | 409 | 429) {
        RefreshFailure::Rejected
    } else {
        RefreshFailure::Transient
    }
}

#[cfg(test)]
#[path = "oauth_tests.rs"]
mod tests;
