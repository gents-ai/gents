//! Device-code OAuth login against xAI (`auth.x.ai`), public Grok CLI client.
//!
//! Browser authorization-code + PKCE is supported by discovery but not required
//! for v1; device-code works on SSH/VPS without a loopback redirect.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::Deserialize;
use serde_json::Value;

use crate::oauth_credential::{oauth_credential_id, OAuthCredential};
use crate::xai_oauth_refresh::XAI_OAUTH_CLIENT_ID;

const DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";
const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";

/// Scopes required for Grok CLI proxy chat (see spike design note).
pub const XAI_OAUTH_SCOPES: &str = "openid profile email offline_access grok-cli:access api:access conversations:read conversations:write";

pub const XAI_OAUTH_DEVICE_URL_OVERRIDE_ENV: &str = "GENTS_XAI_OAUTH_DEVICE_URL";
pub const XAI_OAUTH_TOKEN_URL_OVERRIDE_ENV: &str = "GENTS_XAI_OAUTH_TOKEN_URL";

#[derive(Debug, Clone)]
pub struct XaiLoginTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: Option<String>,
    pub expires_in: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct DeviceCodeChallenge {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct TokenSuccess {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

#[derive(Deserialize)]
struct TokenErrorBody {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    error_description: Option<String>,
}

pub async fn request_device_code(http: &reqwest::Client) -> Result<DeviceCodeChallenge> {
    let endpoint = std::env::var(XAI_OAUTH_DEVICE_URL_OVERRIDE_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEVICE_CODE_URL.to_string());

    let response = http
        .post(&endpoint)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("client_id", XAI_OAUTH_CLIENT_ID),
            ("scope", XAI_OAUTH_SCOPES),
        ])
        .send()
        .await
        .context("xAI device-code request failed")?;

    let status = response.status();
    let body = response
        .text()
        .await
        .context("reading xAI device-code response")?;
    if !status.is_success() {
        anyhow::bail!("xAI device-code request failed with HTTP {status}: {body}");
    }
    let parsed: DeviceCodeResponse =
        serde_json::from_str(&body).context("decoding xAI device-code response")?;
    Ok(DeviceCodeChallenge {
        device_code: parsed.device_code,
        user_code: parsed.user_code,
        verification_uri: parsed.verification_uri,
        verification_uri_complete: parsed
            .verification_uri_complete
            .filter(|value| !value.trim().is_empty()),
        expires_in: parsed.expires_in,
        interval: parsed.interval.unwrap_or(5).max(1),
    })
}

pub async fn poll_device_token(
    http: &reqwest::Client,
    challenge: &DeviceCodeChallenge,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<XaiLoginTokens> {
    let endpoint = std::env::var(XAI_OAUTH_TOKEN_URL_OVERRIDE_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| TOKEN_URL.to_string());

    let deadline = Instant::now() + Duration::from_secs(challenge.expires_in.max(30));
    let mut interval = Duration::from_secs(challenge.interval.max(1));

    loop {
        if cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::SeqCst))
        {
            anyhow::bail!("xAI device-code login was cancelled");
        }
        if Instant::now() >= deadline {
            anyhow::bail!("xAI device-code authorization timed out before approval");
        }

        let response = http
            .post(&endpoint)
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&[
                (
                    "grant_type",
                    "urn:ietf:params:oauth:grant-type:device_code",
                ),
                ("client_id", XAI_OAUTH_CLIENT_ID),
                ("device_code", challenge.device_code.as_str()),
            ])
            .send()
            .await
            .context("xAI device-code token poll failed")?;

        let status = response.status();
        let body = response
            .text()
            .await
            .context("reading xAI device-code token response")?;

        if status.is_success() {
            let success: TokenSuccess =
                serde_json::from_str(&body).context("decoding xAI device-code token response")?;
            return Ok(XaiLoginTokens {
                access_token: success.access_token,
                refresh_token: success.refresh_token,
                id_token: success.id_token,
                expires_in: success.expires_in,
            });
        }

        let error = serde_json::from_str::<TokenErrorBody>(&body)
            .ok()
            .and_then(|value| value.error)
            .or_else(|| {
                serde_json::from_str::<Value>(&body)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("error")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                    })
            })
            .unwrap_or_default();

        match error.as_str() {
            "authorization_pending" => {
                tokio::time::sleep(interval).await;
            }
            "slow_down" => {
                interval = (interval + Duration::from_secs(5)).min(Duration::from_secs(30));
                tokio::time::sleep(interval).await;
            }
            "access_denied" | "authorization_denied" => {
                anyhow::bail!("xAI device-code authorization was denied");
            }
            "expired_token" => {
                anyhow::bail!("xAI device-code expired before approval");
            }
            other if other.is_empty() => {
                anyhow::bail!("xAI device-code token poll failed with HTTP {status}: {body}");
            }
            other => {
                anyhow::bail!("xAI device-code token poll failed ({other}): {body}");
            }
        }
    }
}

/// Full device-code login: request code, print URL, poll until approved.
///
/// `open_browser` is reserved for future loopback/browser helpers; v1 always
/// prints the verification URL so operators can open it on any device (SSH-safe).
pub async fn run_device_code_login(
    http: &reqwest::Client,
    _open_browser: bool,
) -> Result<XaiLoginTokens> {
    let challenge = request_device_code(http).await?;
    let open_url = challenge
        .verification_uri_complete
        .as_deref()
        .unwrap_or(challenge.verification_uri.as_str());

    eprintln!("Open this URL to sign in with Grok / xAI:");
    eprintln!("{open_url}");
    if challenge.verification_uri_complete.is_none() {
        eprintln!("When prompted, enter code: {}", challenge.user_code);
    }

    poll_device_token(http, &challenge, None).await
}

/// Device-code login that returns the verification URL for UI surfaces, then polls.
pub async fn run_device_code_login_with_url_callback<F>(
    http: &reqwest::Client,
    cancel: Option<Arc<AtomicBool>>,
    on_url: F,
) -> Result<XaiLoginTokens>
where
    F: FnOnce(&str),
{
    let challenge = request_device_code(http).await?;
    let open_url = challenge
        .verification_uri_complete
        .as_deref()
        .unwrap_or(challenge.verification_uri.as_str());
    on_url(open_url);
    poll_device_token(http, &challenge, cancel).await
}

pub fn credential_from_login_tokens(
    agent_did: impl Into<String>,
    provider: impl Into<String>,
    tokens: &XaiLoginTokens,
    now: chrono::DateTime<Utc>,
) -> OAuthCredential {
    let agent_did = agent_did.into();
    let provider = provider.into();
    let access_token_expires_at = crate::chatgpt_oauth_refresh::jwt_expiration(&tokens.access_token)
        .or_else(|| {
            tokens
                .expires_in
                .filter(|seconds| *seconds > 0)
                .map(|seconds| now + chrono::Duration::seconds(seconds))
        })
        .unwrap_or_else(|| now + chrono::Duration::minutes(15));

    OAuthCredential {
        doc_id: None,
        credential_id: oauth_credential_id(&agent_did, &provider),
        agent_did,
        provider,
        access_token: tokens.access_token.clone(),
        refresh_token: tokens.refresh_token.clone(),
        id_token: tokens.id_token.clone(),
        account_id: None,
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at,
        last_refresh: Some(now),
        enabled: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;

    #[test]
    fn credential_from_tokens_leaves_chatgpt_fields_unused() {
        let tokens = XaiLoginTokens {
            access_token: "acc".into(),
            refresh_token: "ref".into(),
            id_token: Some("id".into()),
            expires_in: Some(900),
        };
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let credential = credential_from_login_tokens("did:key:zA", "xai-oauth", &tokens, now);
        assert_eq!(credential.provider, "xai-oauth");
        assert_eq!(credential.credential_id, "xai-oauth:did:key:zA");
        assert!(credential.chatgpt_plan_type.is_none());
        assert!(!credential.is_fedramp);
        assert!(credential.enabled);
        assert_eq!(
            credential.access_token_expires_at,
            now + chrono::Duration::seconds(900)
        );
    }
}
