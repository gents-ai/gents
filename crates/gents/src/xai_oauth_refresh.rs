use chrono::Utc;
use serde::Deserialize;

use crate::oauth_credential::token_endpoint::{
    endpoint_url, BadRequest, TokenEndpoint, TransportDetail,
};
use crate::oauth_credential::{OAuthAuthProblem, RefreshedTokens};

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

const TOKEN_ENDPOINT: TokenEndpoint = TokenEndpoint {
    provider: "xAI",
    bad_request: BadRequest::Expired,
    forbidden_is_not_entitled: true,
    reads_error_description: true,
    transport_detail: TransportDetail::Display,
};

pub async fn refresh_xai_token(
    refresh_token: &str,
    http: &reqwest::Client,
) -> Result<RefreshedTokens, OAuthAuthProblem> {
    let endpoint = endpoint_url(XAI_OAUTH_TOKEN_URL_OVERRIDE_ENV, || TOKEN_URL.to_string());
    refresh_xai_token_at(&endpoint, refresh_token, http).await
}

async fn refresh_xai_token_at(
    endpoint: &str,
    refresh_token: &str,
    http: &reqwest::Client,
) -> Result<RefreshedTokens, OAuthAuthProblem> {
    let grant: RefreshResponse = TOKEN_ENDPOINT
        .exchange(
            http.post(endpoint)
                .header("Accept", "application/json")
                .header("Content-Type", "application/x-www-form-urlencoded")
                .form(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", XAI_OAUTH_CLIENT_ID),
                    ("refresh_token", refresh_token),
                ]),
        )
        .await?;
    let access_token = TOKEN_ENDPOINT.access_token(grant.access_token)?;
    let new_refresh = grant.refresh_token.ok_or_else(|| {
        OAuthAuthProblem::Other(
            "xAI token refresh response omitted refresh_token (rotation required)".to_string(),
        )
    })?;

    let access_token_expires_at = crate::oauth_credential::resolve_access_token_expiry(
        &access_token,
        grant.expires_in,
        Utc::now(),
    );

    Ok(RefreshedTokens {
        access_token,
        refresh_token: new_refresh,
        id_token: grant.id_token,
        account_id: None,
        is_fedramp: false,
        plan_type: None,
        access_token_expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth_credential::test_support::one_shot_token_server;

    async fn refresh_against(
        status: u16,
        body: &'static str,
    ) -> (Result<RefreshedTokens, OAuthAuthProblem>, String) {
        let (url, handle) = one_shot_token_server(status, body).await;
        let result = refresh_xai_token_at(&url, "refresh-OLD", &reqwest::Client::new()).await;
        (result, handle.await.expect("server"))
    }

    #[tokio::test]
    async fn refresh_posts_a_form_with_client_id_and_requires_rotation() {
        let (result, request) = refresh_against(
            200,
            r#"{"access_token":"access-NEW","refresh_token":"refresh-NEW","id_token":"id-NEW","expires_in":900}"#,
        )
        .await;
        let refreshed = result.expect("refreshed");
        assert!(
            request.contains("content-type: application/x-www-form-urlencoded"),
            "{request}"
        );
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        assert!(body.contains("grant_type=refresh_token"), "{body}");
        assert!(
            body.contains(&format!("client_id={XAI_OAUTH_CLIENT_ID}")),
            "{body}"
        );
        assert!(body.contains("refresh_token=refresh-OLD"), "{body}");
        assert_eq!(refreshed.refresh_token, "refresh-NEW");
        assert_eq!(refreshed.id_token.as_deref(), Some("id-NEW"));

        let (result, _) =
            refresh_against(200, r#"{"access_token":"access-NEW","expires_in":900}"#).await;
        assert!(
            matches!(result, Err(OAuthAuthProblem::Other(text)) if text.contains("rotation required"))
        );
    }

    #[tokio::test]
    async fn every_400_and_401_is_expired_and_403_is_not_entitled() {
        for (status, body, expected) in [
            (
                400,
                r#"{"error":"invalid_request"}"#,
                OAuthAuthProblem::Expired,
            ),
            (
                401,
                r#"{"error":"invalid_token"}"#,
                OAuthAuthProblem::Expired,
            ),
            (
                403,
                r#"{"error":"forbidden"}"#,
                OAuthAuthProblem::NotEntitled,
            ),
        ] {
            let (result, _) = refresh_against(status, body).await;
            assert_eq!(result.unwrap_err(), expected, "{status}: {body}");
        }
        let (result, _) = refresh_against(
            500,
            r#"{"error":"server_error","error_description":"try later"}"#,
        )
        .await;
        assert!(
            matches!(result, Err(OAuthAuthProblem::Other(text)) if text.contains("xAI") && text.contains("500") && text.contains("try later"))
        );
    }
}
