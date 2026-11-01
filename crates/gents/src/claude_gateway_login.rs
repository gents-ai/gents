//! Sign-in to and refresh of a Claude Code cloud gateway, the I/O around the
//! `gents_claude::gateway` protocol. A sign-in becomes an `OAuthCredential` of
//! provider [`CLAUDE_GATEWAY_PROVIDER`] whose `account_id` is the gateway URL,
//! so accounts of two gateways never merge; the refresh reads that URL's
//! endpoints again, since the credential keeps no token endpoint of its own.
//! Every request is bounded by the protocol's ten seconds and never follows a
//! redirect, as Claude Code's are.

use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use gents_claude::gateway::{self, DeviceCode, Endpoints, Poll, Tokens};

pub use gents_claude::gateway::base_url;

use crate::oauth_credential::{
    oauth_credential_id, OAuthAuthProblem, OAuthCredential, OAuthProduct, RefreshedTokens,
};

/// The `OAuthCredential.provider` of a gateway sign-in.
pub const CLAUDE_GATEWAY_PROVIDER: &str = "claude-gateway";

pub const CLAUDE_GATEWAY_PRODUCT: OAuthProduct = OAuthProduct {
    name: "Claude gateway",
    backend_label: "Claude gateway backend",
    login_command: "claude-gateway-login",
    not_entitled_guidance: "Ask the gateway's administrator for access to Claude.",
};

const TIMEOUT: Duration = Duration::from_millis(gateway::REQUEST_TIMEOUT_MS);
/// Largest gateway answer read.
const MAX_ANSWER_BYTES: usize = 64 * 1024;

/// The client every gateway call goes through: bounded, no redirects.
pub fn gateway_http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("building the gateway sign-in client")
}

/// An answer's status and at most [`MAX_ANSWER_BYTES`] of its body.
async fn read(response: reqwest::Response) -> Result<(reqwest::StatusCode, Vec<u8>)> {
    let status = response.status();
    let mut response = response;
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("reading the gateway answer")?
    {
        anyhow::ensure!(
            body.len() + chunk.len() <= MAX_ANSWER_BYTES,
            "the gateway answer is larger than {MAX_ANSWER_BYTES} bytes"
        );
        body.extend_from_slice(&chunk);
    }
    Ok((status, body))
}

async fn post_form(
    http: &reqwest::Client,
    url: &str,
    form: &[(&str, &str)],
) -> Result<(reqwest::StatusCode, Vec<u8>)> {
    let response = http
        .post(url)
        .header("User-Agent", gateway::user_agent())
        .form(form)
        .send()
        .await
        .with_context(|| format!("the gateway at {url} could not be reached"))?;
    read(response).await
}

/// The endpoints of the gateway at `base`. Metadata that cannot be read is no
/// metadata: the default paths apply, as in Claude Code.
pub async fn discover(http: &reqwest::Client, base: &str) -> Endpoints {
    let metadata = async {
        let response = http
            .get(gateway::metadata_url(base))
            .header("User-Agent", gateway::user_agent())
            .send()
            .await
            .ok()?;
        let (status, body) = read(response).await.ok()?;
        status.is_success().then_some(body)
    }
    .await;
    gateway::endpoints(base, metadata.as_deref())
}

/// Requests a device code from the gateway.
pub async fn request_device_code(
    http: &reqwest::Client,
    endpoints: &Endpoints,
) -> Result<DeviceCode> {
    let (status, body) = post_form(
        http,
        &endpoints.device_authorization,
        &gateway::device_form(),
    )
    .await?;
    if !status.is_success() {
        if gateway::poll_outcome(&body) == Poll::SlowDown {
            anyhow::bail!(
                "The gateway is limiting sign-in attempts right now. Wait a few minutes and try again."
            );
        }
        anyhow::bail!("the gateway refused the sign-in with HTTP {status}");
    }
    Ok(gateway::parse_device_code(&body)?)
}

/// Polls until the person approves, denies or lets the code expire. The wait
/// comes before each poll; the device code's lifetime bounds the whole loop.
pub async fn poll_tokens(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    code: &DeviceCode,
) -> Result<Tokens> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(code.expires_in.max(1));
    let mut interval = code.interval_secs();
    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "Sign-in timed out before the browser flow completed. Try again."
        );
        let (status, body) = post_form(
            http,
            &endpoints.token,
            &gateway::poll_form(&code.device_code),
        )
        .await?;
        if status.is_success() {
            return Ok(gateway::parse_tokens(&body)?);
        }
        match gateway::poll_outcome(&body) {
            Poll::Pending => {}
            Poll::SlowDown => interval += gateway::SLOW_DOWN_SECS,
            Poll::Expired => {
                anyhow::bail!("Sign-in timed out before the browser flow completed. Try again.")
            }
            Poll::Denied => anyhow::bail!("Sign-in was denied in the browser."),
            Poll::Failed(Some(code)) => {
                anyhow::bail!("the gateway refused the sign-in ({code}, HTTP {status})")
            }
            Poll::Failed(None) => {
                anyhow::bail!("the gateway refused the sign-in with HTTP {status}")
            }
        }
    }
}

/// The credential a sign-in at `base` stores.
pub fn credential_from_tokens(
    node_did: &str,
    base: &str,
    tokens: &Tokens,
    now: chrono::DateTime<Utc>,
) -> OAuthCredential {
    let email = tokens.email.as_deref().and_then(gateway::display_email);
    OAuthCredential {
        doc_id: None,
        credential_id: oauth_credential_id(node_did, CLAUDE_GATEWAY_PROVIDER),
        node_did: node_did.to_owned(),
        provider: CLAUDE_GATEWAY_PROVIDER.to_owned(),
        access_token: tokens.access_token.clone(),
        refresh_token: tokens.refresh_token.clone().unwrap_or_default(),
        id_token: None,
        account_id: Some(base.to_owned()),
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at: expires_at(now, tokens.expires_in),
        last_refresh: Some(now),
        enabled: true,
        account_ref: None,
        connected_at: None,
        provider_account_key: email.clone(),
        label: email,
    }
}

fn expires_at(now: chrono::DateTime<Utc>, expires_in: u64) -> chrono::DateTime<Utc> {
    now + chrono::Duration::seconds(i64::try_from(expires_in).unwrap_or(i64::MAX / 1_000))
}

/// Refreshes the gateway JWT of the credential whose gateway is `base`.
pub async fn refresh_gateway_token(
    base: Option<&str>,
    refresh_token: &str,
    http: &reqwest::Client,
) -> Result<RefreshedTokens, OAuthAuthProblem> {
    let base = base.ok_or_else(|| {
        OAuthAuthProblem::Other("the gateway sign-in does not name its gateway".to_owned())
    })?;
    if refresh_token.is_empty() {
        // Claude Code: an expired session without a refresh token is signed out.
        return Err(OAuthAuthProblem::Expired);
    }
    let endpoints = discover(http, base).await;
    let (status, body) = post_form(
        http,
        &endpoints.token,
        &gateway::refresh_form(refresh_token),
    )
    .await
    .map_err(|error| OAuthAuthProblem::Other(format!("{error:#}")))?;
    if !status.is_success() {
        return Err(match gateway::poll_outcome(&body) {
            Poll::Failed(Some(code)) if code == "invalid_grant" => OAuthAuthProblem::Expired,
            _ if status == reqwest::StatusCode::UNAUTHORIZED => OAuthAuthProblem::Expired,
            _ if status == reqwest::StatusCode::FORBIDDEN => OAuthAuthProblem::NotEntitled,
            _ => OAuthAuthProblem::Other(format!(
                "the gateway refused the token refresh with HTTP {status}"
            )),
        });
    }
    let tokens =
        gateway::parse_tokens(&body).map_err(|error| OAuthAuthProblem::Other(error.to_string()))?;
    Ok(RefreshedTokens {
        access_token: tokens.access_token,
        refresh_token: tokens
            .refresh_token
            .unwrap_or_else(|| refresh_token.to_owned()),
        id_token: None,
        account_id: None,
        is_fedramp: false,
        plan_type: None,
        access_token_expires_at: expires_at(Utc::now(), tokens.expires_in),
    })
}

#[cfg(test)]
#[path = "claude_gateway_login_tests.rs"]
mod tests;
