use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::OAuthAuthProblem;

/// How one provider's OAuth token endpoint reports a refresh outcome.
///
/// Each field is a difference between the Claude, ChatGPT and xAI endpoints;
/// everything else about a refresh exchange is shared.
pub(crate) struct TokenEndpoint {
    /// Product name used in error text.
    pub(crate) provider: &'static str,
    /// Which HTTP 400 bodies mean the refresh token is expired or revoked.
    pub(crate) bad_request: BadRequest,
    /// Whether HTTP 403 means the account is valid but not entitled.
    pub(crate) forbidden_is_not_entitled: bool,
    /// Whether an RFC 6749 `error_description` is preferred in error text.
    pub(crate) reads_error_description: bool,
    pub(crate) transport_detail: TransportDetail,
}

pub(crate) enum BadRequest {
    /// Every HTTP 400 is an expired or revoked grant.
    Expired,
    /// Only an `invalid_grant` error code is; display text never decides it.
    InvalidGrant { nested_code: bool },
}

pub(crate) enum TransportDetail {
    /// The failure kind only (timed out, could not connect, failed).
    Kind,
    /// The transport error's full display.
    Display,
}

/// The token endpoint URL: the override variable when it is set and non-blank, else the default.
pub(crate) fn endpoint_url<F>(override_env: &str, default: F) -> String
where
    F: FnOnce() -> String,
{
    std::env::var(override_env)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(default)
}

impl TokenEndpoint {
    /// Send a refresh `request`, classify a rejection into an [`OAuthAuthProblem`] and decode a
    /// success as `R`.
    ///
    /// `R` declares only the response fields its provider reads, so a field the provider ignores
    /// can never fail the decode.
    pub(crate) async fn exchange<R>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<R, OAuthAuthProblem>
    where
        R: DeserializeOwned,
    {
        let provider = self.provider;
        let response = request.send().await.map_err(|error| {
            OAuthAuthProblem::Other(format!(
                "{provider} token refresh request failed: {}",
                self.transport(&error)
            ))
        })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(self.rejection(status, &body));
        }
        response.json::<R>().await.map_err(|error| {
            OAuthAuthProblem::Other(format!(
                "decoding {provider} token refresh response: {error}"
            ))
        })
    }

    /// The decoded access token, or the refresh failure its absence is.
    pub(crate) fn access_token(&self, token: Option<String>) -> Result<String, OAuthAuthProblem> {
        token.ok_or_else(|| {
            OAuthAuthProblem::Other(format!(
                "{} token refresh response omitted access_token",
                self.provider
            ))
        })
    }

    fn transport(&self, error: &reqwest::Error) -> String {
        match self.transport_detail {
            TransportDetail::Display => error.to_string(),
            TransportDetail::Kind if error.is_timeout() => "timed out".to_owned(),
            TransportDetail::Kind if error.is_connect() => "could not connect".to_owned(),
            TransportDetail::Kind => "failed".to_owned(),
        }
    }

    fn rejection(&self, status: StatusCode, body: &str) -> OAuthAuthProblem {
        let value = serde_json::from_str::<Value>(body).ok();
        let expired = match status {
            StatusCode::UNAUTHORIZED => true,
            StatusCode::BAD_REQUEST => match self.bad_request {
                BadRequest::Expired => true,
                BadRequest::InvalidGrant { nested_code } => {
                    error_code(value.as_ref(), nested_code) == Some("invalid_grant")
                }
            },
            _ => false,
        };
        if expired {
            return OAuthAuthProblem::Expired;
        }
        if status == StatusCode::FORBIDDEN && self.forbidden_is_not_entitled {
            return OAuthAuthProblem::NotEntitled;
        }
        OAuthAuthProblem::Other(format!(
            "{} token refresh failed with HTTP {status}: {}",
            self.provider,
            error_message(value.as_ref(), body, self.reads_error_description)
        ))
    }
}

fn error_code(value: Option<&Value>, nested_code: bool) -> Option<&str> {
    let error = value?.get("error")?;
    error
        .as_str()
        .or_else(|| nested_code.then(|| error.get("code")?.as_str()).flatten())
}

fn error_message<'a>(value: Option<&'a Value>, body: &'a str, reads_description: bool) -> &'a str {
    let Some(value) = value else {
        return body;
    };
    reads_description
        .then(|| value.get("error_description").and_then(Value::as_str))
        .flatten()
        .or_else(|| {
            value.get("error").and_then(|error| {
                error
                    .as_str()
                    .or_else(|| error.get("message").and_then(Value::as_str))
            })
        })
        .or_else(|| value.get("message").and_then(Value::as_str))
        .unwrap_or(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(body: &str, reads_description: bool) -> String {
        let value = serde_json::from_str::<Value>(body).ok();
        error_message(value.as_ref(), body, reads_description).to_owned()
    }

    #[test]
    fn error_message_prefers_nested_then_falls_back() {
        assert_eq!(
            message(r#"{"error":{"message":"bad token"}}"#, false),
            "bad token"
        );
        assert_eq!(message(r#"{"error":"flat error"}"#, false), "flat error");
        assert_eq!(message(r#"{"message":"top level"}"#, false), "top level");
        assert_eq!(
            message("plain text body", false),
            "plain text body",
            "non-JSON body is returned verbatim"
        );
    }

    #[test]
    fn error_description_is_read_only_where_the_endpoint_uses_it() {
        let body =
            r#"{"error":"invalid_request","error_description":"refresh token is malformed"}"#;
        assert_eq!(message(body, true), "refresh token is malformed");
        assert_eq!(message(body, false), "invalid_request");
    }

    #[test]
    fn nested_error_codes_are_read_only_where_the_endpoint_uses_them() {
        let nested = serde_json::json!({ "error": { "code": "invalid_grant" } });
        assert_eq!(error_code(Some(&nested), true), Some("invalid_grant"));
        assert_eq!(error_code(Some(&nested), false), None);
        let flat = serde_json::json!({ "error": "invalid_grant" });
        assert_eq!(error_code(Some(&flat), false), Some("invalid_grant"));
    }
}
