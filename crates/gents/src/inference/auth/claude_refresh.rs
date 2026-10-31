use chrono::{DateTime, Utc};
use gents_claude::oauth::{self, RefreshFailure};

use crate::inference::auth::claude::{CLAUDE_OAUTH_PRODUCT, CLAUDE_OAUTH_TOKEN_URL_OVERRIDE_ENV};
use crate::inference::auth::credential::token_endpoint::{
    endpoint_url, BadRequest, TokenEndpoint, TransportDetail,
};
use crate::inference::auth::credential::{OAuthAuthProblem, RefreshedTokens};

const TOKEN_ENDPOINT: TokenEndpoint = TokenEndpoint {
    provider: "Claude",
    bad_request: BadRequest::InvalidGrant { nested_code: false },
    forbidden_is_not_entitled: true,
    reads_error_description: true,
    transport_detail: TransportDetail::Kind,
};

pub async fn refresh_claude_token(
    refresh_token: &str,
    http: &reqwest::Client,
) -> Result<RefreshedTokens, OAuthAuthProblem> {
    let endpoint = endpoint_url(CLAUDE_OAUTH_TOKEN_URL_OVERRIDE_ENV, || {
        oauth::TOKEN_URL.to_string()
    });
    refresh_claude_token_at(&endpoint, refresh_token, http).await
}

pub(crate) async fn refresh_claude_token_at(
    endpoint: &str,
    refresh_token: &str,
    http: &reqwest::Client,
) -> Result<RefreshedTokens, OAuthAuthProblem> {
    // Gents stores no granted-scope list, so the owner's default refresh scopes apply.
    let post = oauth::refresh_request(&oauth::RefreshRequest {
        refresh_token,
        scopes: &[],
        expires_in: None,
        client_id: None,
    })
    .map_err(|error| OAuthAuthProblem::Other(format!("Claude token refresh failed: {error}")))?;
    // The caller's client bounds the exchange (`REFRESH_REQUEST_TIMEOUT`, Claude Code's 30 s);
    // a per-request timeout would replace that bound.
    let (status, body) = TOKEN_ENDPOINT
        .respond(
            http.post(endpoint)
                .header("Accept", "application/json")
                .header("Content-Type", "application/json")
                .body(post.body),
        )
        .await?;
    if !status.is_success() {
        return Err(refresh_problem(status, &body));
    }

    let now = Utc::now();
    let tokens = oauth::parse_token_response(
        &body,
        u64::try_from(now.timestamp_millis()).unwrap_or_default(),
        Some(refresh_token),
    )
    .map_err(|error| {
        OAuthAuthProblem::Other(format!("decoding Claude token refresh response: {error}"))
    })?;
    let access_token_expires_at = i64::try_from(tokens.expires_at_ms)
        .ok()
        .and_then(DateTime::from_timestamp_millis)
        .ok_or_else(|| {
            OAuthAuthProblem::Other("Claude token refresh response lifetime is out of range".into())
        })?;

    Ok(RefreshedTokens {
        access_token: tokens.access_token,
        refresh_token: tokens
            .refresh_token
            .unwrap_or_else(|| refresh_token.to_string()),
        id_token: None,
        account_id: None,
        is_fedramp: false,
        plan_type: None,
        access_token_expires_at,
    })
}

/// Maps a failed token call onto the credential problem: the Claude owner's classification
/// decides a signed-out or on-hold account, and the shared endpoint semantics decide the rest
/// (an unreadable 401 is an invalid token and a 403 is the plan gate).
fn refresh_problem(status: reqwest::StatusCode, body: &[u8]) -> OAuthAuthProblem {
    match oauth::classify_failure(Some(status.as_u16()), body) {
        RefreshFailure::SignedOut => OAuthAuthProblem::Expired,
        RefreshFailure::AccountOnHold => OAuthAuthProblem::Other(format!(
            "the Claude account is on hold; resolve the hold with Anthropic, then sign in again with `gents {}`",
            CLAUDE_OAUTH_PRODUCT.login_command
        )),
        RefreshFailure::InvalidScope | RefreshFailure::Rejected | RefreshFailure::Transient => {
            TOKEN_ENDPOINT.rejection(status, &String::from_utf8_lossy(body))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::auth::credential::test_support::one_shot_token_server;

    async fn refresh_against(
        status: u16,
        body: &'static str,
    ) -> (Result<RefreshedTokens, OAuthAuthProblem>, String) {
        let (url, handle) = one_shot_token_server(status, body).await;
        let http = reqwest::Client::new();
        let result = refresh_claude_token_at(&url, "refresh-OLD", &http).await;
        (result, handle.await.expect("server"))
    }

    #[tokio::test]
    async fn refresh_posts_json_with_client_id_and_scope() {
        let (result, request) = refresh_against(
            200,
            r#"{"access_token":"access-NEW","refresh_token":"refresh-NEW","expires_in":28800}"#,
        )
        .await;
        let refreshed = result.expect("refreshed");
        assert!(request.starts_with("POST /v1/oauth/token"), "{request}");
        assert!(
            request.contains("content-type: application/json"),
            "{request}"
        );
        let body: serde_json::Value =
            serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["grant_type"], "refresh_token");
        assert_eq!(body["refresh_token"], "refresh-OLD");
        assert_eq!(body["client_id"], oauth::CLIENT_ID);
        assert_eq!(body["scope"], oauth::REFRESH_SCOPES.join(" "));
        assert_eq!(refreshed.access_token, "access-NEW");
        assert_eq!(refreshed.refresh_token, "refresh-NEW");
        assert!(
            refreshed.access_token_expires_at > chrono::Utc::now() + chrono::Duration::hours(7)
        );
        assert_eq!(refreshed.id_token, None);
    }

    #[tokio::test]
    async fn refresh_keeps_old_refresh_token_when_not_rotated() {
        let (result, _) =
            refresh_against(200, r#"{"access_token":"access-NEW","expires_in":3600}"#).await;
        assert_eq!(result.expect("refreshed").refresh_token, "refresh-OLD");
    }

    #[tokio::test]
    async fn refresh_401_is_expired() {
        let (result, _) = refresh_against(401, r#"{"error":"invalid_token"}"#).await;
        assert_eq!(result.unwrap_err(), OAuthAuthProblem::Expired);
    }

    #[tokio::test]
    async fn refresh_400_invalid_grant_is_expired_but_other_400_is_other() {
        let (result, _) = refresh_against(
            400,
            r#"{"error":"invalid_grant","error_description":"revoked"}"#,
        )
        .await;
        assert_eq!(result.unwrap_err(), OAuthAuthProblem::Expired);
        let (result, _) = refresh_against(400, r#"{"error":"invalid_request"}"#).await;
        assert!(
            matches!(result.unwrap_err(), OAuthAuthProblem::Other(text) if text.contains("400") && text.contains("invalid_request"))
        );
    }

    #[tokio::test]
    async fn refresh_403_is_not_entitled() {
        let (result, _) = refresh_against(403, r#"{"error":"forbidden"}"#).await;
        assert_eq!(result.unwrap_err(), OAuthAuthProblem::NotEntitled);
    }

    #[tokio::test]
    async fn refresh_account_on_hold_is_an_actionable_problem() {
        let (result, _) = refresh_against(
            400,
            r#"{"error":"invalid_grant","error_description":"account_on_hold"}"#,
        )
        .await;
        match result.unwrap_err() {
            OAuthAuthProblem::Other(text) => {
                assert!(text.contains("on hold"), "{text}");
                assert!(text.contains("gents claude-login"), "{text}");
            }
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn refresh_server_error_and_invalid_scope_are_other() {
        for (status, body) in [
            (500, r#"{"error":"server_error"}"#),
            (
                400,
                r#"{"error":"invalid_scope","error_description":"scope"}"#,
            ),
        ] {
            let (result, _) = refresh_against(status, body).await;
            assert!(
                matches!(result.unwrap_err(), OAuthAuthProblem::Other(text) if text.contains(&status.to_string())),
                "{status}"
            );
        }
    }

    #[tokio::test]
    async fn refresh_response_without_lifetime_is_rejected() {
        let (result, _) = refresh_against(200, r#"{"access_token":"access-NEW"}"#).await;
        assert!(matches!(
            result.unwrap_err(),
            OAuthAuthProblem::Other(text) if text.contains("malformed")
        ));
    }

    #[tokio::test]
    async fn refresh_honors_the_token_url_override_env() {
        let (url, handle) =
            one_shot_token_server(200, r#"{"access_token":"access-NEW","expires_in":3600}"#).await;
        let _env = crate::inference::auth::credential::test_support::TOKEN_URL_ENV
            .lock()
            .await;
        std::env::set_var(CLAUDE_OAUTH_TOKEN_URL_OVERRIDE_ENV, &url);
        let result = refresh_claude_token("refresh-OLD", &reqwest::Client::new()).await;
        std::env::remove_var(CLAUDE_OAUTH_TOKEN_URL_OVERRIDE_ENV);
        assert_eq!(result.expect("refreshed").access_token, "access-NEW");
        let request = handle.await.expect("server");
        assert!(request.starts_with("POST /v1/oauth/token"), "{request}");
    }
}
