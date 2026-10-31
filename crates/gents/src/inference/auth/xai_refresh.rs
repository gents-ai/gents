use chrono::Utc;
use serde::Deserialize;
use serde_json::Value;

use crate::inference::auth::credential::{OAuthAuthProblem, RefreshedTokens};

const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";

pub const XAI_OAUTH_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";

pub const XAI_OAUTH_TOKEN_URL_OVERRIDE_ENV: &str = "GENTS_XAI_OAUTH_TOKEN_URL";

#[derive(Deserialize)]
struct RefreshResponse {
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

pub async fn refresh_xai_token(
    refresh_token: &str,
    http: &reqwest::Client,
) -> Result<RefreshedTokens, OAuthAuthProblem> {
    let endpoint = std::env::var(XAI_OAUTH_TOKEN_URL_OVERRIDE_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| TOKEN_URL.to_string());

    let response = http
        .post(&endpoint)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", XAI_OAUTH_CLIENT_ID),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .map_err(|error| {
            OAuthAuthProblem::Other(format!("xAI token refresh request failed: {error}"))
        })?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::BAD_REQUEST
        {
            return Err(OAuthAuthProblem::Expired);
        }
        if status == reqwest::StatusCode::FORBIDDEN {
            return Err(OAuthAuthProblem::NotEntitled);
        }
        return Err(OAuthAuthProblem::Other(format!(
            "xAI token refresh failed with HTTP {status}: {}",
            parse_error_message(&body)
        )));
    }

    let refreshed = response.json::<RefreshResponse>().await.map_err(|error| {
        OAuthAuthProblem::Other(format!("decoding xAI token refresh response: {error}"))
    })?;
    let access_token = refreshed.access_token.ok_or_else(|| {
        OAuthAuthProblem::Other("xAI token refresh response omitted access_token".to_string())
    })?;
    let new_refresh = refreshed.refresh_token.ok_or_else(|| {
        OAuthAuthProblem::Other(
            "xAI token refresh response omitted refresh_token (rotation required)".to_string(),
        )
    })?;

    let access_token_expires_at = crate::inference::auth::credential::resolve_access_token_expiry(
        &access_token,
        refreshed.expires_in,
        Utc::now(),
    );

    Ok(RefreshedTokens {
        access_token,
        refresh_token: new_refresh,
        id_token: refreshed.id_token,
        account_id: None,
        is_fedramp: false,
        plan_type: None,
        access_token_expires_at,
    })
}

fn parse_error_message(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return body.to_string();
    };
    value
        .get("error_description")
        .and_then(Value::as_str)
        .or_else(|| {
            value.get("error").and_then(|error| {
                error
                    .as_str()
                    .or_else(|| error.get("message").and_then(Value::as_str))
            })
        })
        .or_else(|| value.get("message").and_then(Value::as_str))
        .unwrap_or(body)
        .to_string()
}
