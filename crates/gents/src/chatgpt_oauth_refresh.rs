use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::oauth_credential::{OAuthAuthProblem, RefreshedTokens};

const REFRESH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IdTokenClaims {
    pub expires_at: Option<DateTime<Utc>>,
    pub account_id: Option<String>,
    pub is_fedramp: bool,
    pub plan_type: Option<String>,
}

#[derive(Serialize)]
struct RefreshRequest<'a> {
    client_id: &'static str,
    grant_type: &'static str,
    refresh_token: &'a str,
}

#[derive(Deserialize)]
struct RefreshResponse {
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
}

pub async fn refresh_chatgpt_token(
    refresh_token: &str,
    http: &reqwest::Client,
) -> Result<RefreshedTokens, OAuthAuthProblem> {
    let endpoint = std::env::var(codex_login::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| REFRESH_TOKEN_URL.to_string());
    let request = RefreshRequest {
        client_id: codex_login::CLIENT_ID,
        grant_type: "refresh_token",
        refresh_token,
    };
    let response = http
        .post(&endpoint)
        .header("Content-Type", "application/json")
        .json(&request)
        .send()
        .await
        .map_err(|error| {
            OAuthAuthProblem::Other(format!("ChatGPT token refresh request failed: {error}"))
        })?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(OAuthAuthProblem::Expired);
        }
        return Err(OAuthAuthProblem::Other(format!(
            "ChatGPT token refresh failed with HTTP {status}: {}",
            parse_error_message(&body)
        )));
    }

    let refreshed = response.json::<RefreshResponse>().await.map_err(|error| {
        OAuthAuthProblem::Other(format!("decoding ChatGPT token refresh response: {error}"))
    })?;
    let access_token = refreshed.access_token.ok_or_else(|| {
        OAuthAuthProblem::Other("ChatGPT token refresh response omitted access_token".to_string())
    })?;
    let id_claims = refreshed
        .id_token
        .as_deref()
        .map(decode_id_token_claims)
        .unwrap_or_default();
    let access_token_expires_at = jwt_expiration(&access_token)
        .or(id_claims.expires_at)
        .unwrap_or_else(|| Utc::now() + Duration::hours(1));

    Ok(RefreshedTokens {
        access_token,
        refresh_token: refreshed
            .refresh_token
            .unwrap_or_else(|| refresh_token.to_string()),
        id_token: refreshed.id_token,
        account_id: id_claims.account_id,
        is_fedramp: id_claims.is_fedramp,
        plan_type: id_claims.plan_type,
        access_token_expires_at,
    })
}

/// Decode the advisory claims (account id, plan, FedRAMP flag, expiry) from an OIDC ID token.
///
/// The signature is NOT verified: trust derives from the TLS-protected token endpoint these tokens
/// arrive from, per OIDC §3.1.3.7's allowance for tokens received directly over a secure channel.
/// The claims are used only to populate request headers sent back to OpenAI and to estimate expiry;
/// they are not an authorization boundary inside gents. If a claim ever gates a security
/// decision, verify the signature against OpenAI's JWKS first.
pub fn decode_id_token_claims(id_token: &str) -> IdTokenClaims {
    let Some(payload) = jwt_payload(id_token) else {
        return IdTokenClaims::default();
    };
    let expires_at = payload
        .get("exp")
        .and_then(Value::as_i64)
        .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0));
    let auth = payload
        .get("https://api.openai.com/auth")
        .and_then(Value::as_object);
    let lookup = |field: &str| {
        payload
            .get(field)
            .or_else(|| auth.and_then(|object| object.get(field)))
    };
    IdTokenClaims {
        expires_at,
        account_id: lookup("chatgpt_account_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        is_fedramp: lookup("chatgpt_account_is_fedramp")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        plan_type: lookup("chatgpt_plan_type")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    }
}

pub fn jwt_expiration(jwt: &str) -> Option<DateTime<Utc>> {
    jwt_payload(jwt)?
        .get("exp")
        .and_then(Value::as_i64)
        .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0))
}

fn jwt_payload(jwt: &str) -> Option<Value> {
    let mut parts = jwt.split('.');
    let (_header, payload, signature) = (parts.next()?, parts.next()?, parts.next()?);
    if payload.is_empty() || signature.is_empty() || parts.next().is_some() {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn parse_error_message(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return body.to_string();
    };
    value
        .get("error")
        .and_then(|error| error.get("message").or(Some(error)))
        .and_then(Value::as_str)
        .or_else(|| value.get("message").and_then(Value::as_str))
        .unwrap_or(body)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_nested_chatgpt_claims() {
        let jwt = unsigned_jwt(serde_json::json!({
            "exp": 1_700_000_000i64,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acc-123",
                "chatgpt_account_is_fedramp": true,
                "chatgpt_plan_type": "pro"
            }
        }));

        let claims = decode_id_token_claims(&jwt);

        assert_eq!(claims.account_id.as_deref(), Some("acc-123"));
        assert!(claims.is_fedramp);
        assert_eq!(claims.plan_type.as_deref(), Some("pro"));
        assert_eq!(claims.expires_at.unwrap().timestamp(), 1_700_000_000);
    }

    #[test]
    fn jwt_expiration_reads_exp_claim() {
        let jwt = unsigned_jwt(serde_json::json!({ "exp": 1_700_000_000i64 }));
        assert_eq!(jwt_expiration(&jwt).unwrap().timestamp(), 1_700_000_000);
        assert!(
            jwt_expiration("not-a-jwt").is_none(),
            "malformed token has no expiry"
        );
    }

    #[test]
    fn parse_error_message_prefers_nested_then_falls_back() {
        assert_eq!(
            parse_error_message(r#"{"error":{"message":"bad token"}}"#),
            "bad token"
        );
        assert_eq!(
            parse_error_message(r#"{"error":"flat error"}"#),
            "flat error"
        );
        assert_eq!(
            parse_error_message(r#"{"message":"top level"}"#),
            "top level"
        );
        assert_eq!(
            parse_error_message("plain text body"),
            "plain text body",
            "non-JSON body is returned verbatim"
        );
    }

    fn unsigned_jwt(payload: Value) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&payload).unwrap());
        format!("{header}.{payload}.sig")
    }
}
