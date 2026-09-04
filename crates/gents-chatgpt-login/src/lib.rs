//! Minimal ChatGPT OAuth flows used by the Gents CLI and desktop app.
//!
//! This crate intentionally owns only the browser PKCE and device-code
//! exchanges. Tokens are returned to the caller for immediate persistence in
//! DefraDB; no Codex configuration, keyring, provider, sandbox, or app-server
//! code is linked into Gents.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use rand::RngCore;
use reqwest::StatusCode;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use tiny_http::{Header, Response, Server, StatusCode as TinyStatusCode};
use tokio::sync::{mpsc, Notify};
use url::Url;

pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const DEFAULT_ISSUER: &str = "https://auth.openai.com";
pub const REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR: &str = "CODEX_REFRESH_TOKEN_URL_OVERRIDE";
/// The refresh-token endpoint under [`DEFAULT_ISSUER`]. Kept as its own
/// constant (rather than formatted at each call site) for callers — like
/// `gents::chatgpt_oauth_refresh` — that only ever refresh against the
/// default issuer; `refresh_tokens` in this crate still formats
/// `{issuer}/oauth/token` directly since it supports a caller-supplied
/// issuer. `refresh_token_url_matches_default_issuer` guards the two
/// against drift.
pub const REFRESH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

const DEFAULT_PORT: u16 = 1455;
const FALLBACK_PORT: u16 = 1457;
const DEVICE_AUTH_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const DEFAULT_DEVICE_POLL_INTERVAL: u64 = 5;
const ORIGINATOR: &str = "codex_cli_rs";
const CODEX_CA_CERT_ENV: &str = "CODEX_CA_CERTIFICATE";
const SSL_CERT_FILE_ENV: &str = "SSL_CERT_FILE";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginOptions {
    pub client_id: String,
    pub issuer: String,
    pub port: u16,
    pub open_browser: bool,
    /// Test hook for deterministic callback-state checks.
    pub force_state: Option<String>,
}

impl Default for LoginOptions {
    fn default() -> Self {
        Self {
            client_id: CLIENT_ID.to_string(),
            issuer: DEFAULT_ISSUER.to_string(),
            port: DEFAULT_PORT,
            open_browser: true,
            force_state: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginTokens {
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: String,
}

#[derive(Clone, Debug)]
pub struct ShutdownHandle {
    notify: Arc<Notify>,
}

impl ShutdownHandle {
    pub fn shutdown(&self) {
        self.notify.notify_one();
    }
}

pub struct LoginServer {
    pub auth_url: String,
    pub actual_port: u16,
    task: tokio::task::JoinHandle<io::Result<LoginTokens>>,
    shutdown: ShutdownHandle,
}

impl LoginServer {
    pub async fn block_until_done(self) -> io::Result<LoginTokens> {
        self.task
            .await
            .map_err(|error| io::Error::other(format!("login callback task failed: {error}")))?
    }

    pub fn cancel_handle(&self) -> ShutdownHandle {
        self.shutdown.clone()
    }
}

/// Start a loopback-only OAuth callback server.
pub fn run_login_server(options: LoginOptions) -> io::Result<LoginServer> {
    let pkce = generate_pkce();
    let state = options.force_state.clone().unwrap_or_else(generate_state);
    let server = bind_server(options.port)?;
    let actual_port = server
        .server_addr()
        .to_ip()
        .map(|address| address.port())
        .ok_or_else(|| io::Error::other("login callback server did not expose an IP port"))?;
    let server = Arc::new(server);
    let redirect_uri = format!("http://localhost:{actual_port}/auth/callback");
    let auth_url = build_authorize_url(&options, &redirect_uri, &pkce, &state)?;

    if options.open_browser {
        if let Err(error) = webbrowser::open(&auth_url) {
            tracing::warn!(%error, "could not open the ChatGPT login URL in a browser");
        }
    }

    let (sender, mut receiver) = mpsc::channel(8);
    let receive_server = server.clone();
    thread::spawn(move || {
        while let Ok(request) = receive_server.recv() {
            if sender.blocking_send(request).is_err() {
                break;
            }
        }
    });

    let notify = Arc::new(Notify::new());
    let task_notify = notify.clone();
    let task_server = server.clone();
    let task = tokio::spawn(async move {
        let result = loop {
            tokio::select! {
                _ = task_notify.notified() => {
                    break Err(io::Error::other("ChatGPT login was cancelled"));
                }
                request = receiver.recv() => {
                    let Some(request) = request else {
                        break Err(io::Error::other("ChatGPT login callback server stopped"));
                    };
                    let outcome = handle_callback_request(
                        request.url(),
                        &options,
                        &redirect_uri,
                        &pkce,
                        &state,
                    ).await;
                    let (status, body, completed) = outcome.into_parts();
                    let response = text_response(status, body);
                    let _ = tokio::task::spawn_blocking(move || request.respond(response)).await;
                    if let Some(result) = completed {
                        break result;
                    }
                }
            }
        };
        task_server.unblock();
        result
    });

    Ok(LoginServer {
        auth_url,
        actual_port,
        task,
        shutdown: ShutdownHandle { notify },
    })
}

enum CallbackOutcome {
    Continue {
        status: u16,
        body: String,
    },
    Complete {
        status: u16,
        body: String,
        result: io::Result<LoginTokens>,
    },
}

impl CallbackOutcome {
    fn into_parts(self) -> (u16, String, Option<io::Result<LoginTokens>>) {
        match self {
            Self::Continue { status, body } => (status, body, None),
            Self::Complete {
                status,
                body,
                result,
            } => (status, body, Some(result)),
        }
    }
}

async fn handle_callback_request(
    request_target: &str,
    options: &LoginOptions,
    redirect_uri: &str,
    pkce: &PkceCodes,
    expected_state: &str,
) -> CallbackOutcome {
    if request_target == "/cancel" {
        return CallbackOutcome::Complete {
            status: 200,
            body: "ChatGPT login cancelled. You may close this window.".to_string(),
            result: Err(io::Error::other("ChatGPT login was cancelled")),
        };
    }

    let parsed = match Url::parse(&format!("http://localhost{request_target}")) {
        Ok(parsed) => parsed,
        Err(_) => {
            return CallbackOutcome::Continue {
                status: 400,
                body: "Invalid callback request.".to_string(),
            };
        }
    };
    if parsed.path() != "/auth/callback" {
        return CallbackOutcome::Continue {
            status: 404,
            body: "Not found.".to_string(),
        };
    }

    let params: HashMap<String, String> = parsed.query_pairs().into_owned().collect();
    if params.get("state").map(String::as_str) != Some(expected_state) {
        tracing::warn!("rejected ChatGPT OAuth callback with mismatched state");
        return CallbackOutcome::Continue {
            status: 400,
            body: "OAuth state mismatch. Return to the terminal and retry sign-in.".to_string(),
        };
    }
    if let Some(error) = params.get("error") {
        let description = params
            .get("error_description")
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty());
        let message = description.unwrap_or(error);
        return CallbackOutcome::Complete {
            status: 400,
            body: "ChatGPT sign-in was not completed. Return to Gents for details.".to_string(),
            result: Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("ChatGPT authorization failed: {message}"),
            )),
        };
    }
    let Some(code) = params
        .get("code")
        .map(String::as_str)
        .filter(|value| !value.is_empty())
    else {
        return CallbackOutcome::Continue {
            status: 400,
            body: "The callback omitted its authorization code.".to_string(),
        };
    };

    match exchange_code_for_tokens(options, redirect_uri, pkce, code).await {
        Ok(tokens) => CallbackOutcome::Complete {
            status: 200,
            body: "ChatGPT sign-in complete. You may close this window.".to_string(),
            result: Ok(tokens),
        },
        Err(error) => CallbackOutcome::Complete {
            status: 502,
            body: "ChatGPT token exchange failed. Return to Gents for details.".to_string(),
            result: Err(error),
        },
    }
}

fn text_response(status: u16, body: String) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut response = Response::from_string(body).with_status_code(TinyStatusCode(status));
    if let Ok(header) = Header::from_bytes("Content-Type", "text/plain; charset=utf-8") {
        response.add_header(header);
    }
    response
}

fn bind_server(port: u16) -> io::Result<Server> {
    match Server::http(format!("127.0.0.1:{port}")) {
        Ok(server) => Ok(server),
        Err(primary) if port == DEFAULT_PORT => Server::http(format!("127.0.0.1:{FALLBACK_PORT}"))
            .map_err(|fallback| {
                io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!(
                        "ChatGPT login callback ports {DEFAULT_PORT} and {FALLBACK_PORT} are unavailable: {primary}; {fallback}"
                    ),
                )
            }),
        Err(error) => Err(io::Error::other(error)),
    }
}

#[derive(Clone, Debug)]
struct PkceCodes {
    verifier: String,
    challenge: String,
}

fn generate_pkce() -> PkceCodes {
    let mut bytes = [0u8; 64];
    rand::rng().fill_bytes(&mut bytes);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    PkceCodes {
        verifier,
        challenge,
    }
}

fn generate_state() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn build_authorize_url(
    options: &LoginOptions,
    redirect_uri: &str,
    pkce: &PkceCodes,
    state: &str,
) -> io::Result<String> {
    let mut url = Url::parse(&format!(
        "{}/oauth/authorize",
        options.issuer.trim_end_matches('/')
    ))
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &options.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair(
            "scope",
            "openid profile email offline_access api.connectors.read api.connectors.invoke",
        )
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("state", state)
        .append_pair("originator", ORIGINATOR);
    Ok(url.into())
}

#[derive(Deserialize)]
struct TokenResponse {
    id_token: String,
    access_token: String,
    refresh_token: String,
}

async fn exchange_code_for_tokens(
    options: &LoginOptions,
    redirect_uri: &str,
    pkce: &PkceCodes,
    code: &str,
) -> io::Result<LoginTokens> {
    let endpoint = format!("{}/oauth/token", options.issuer.trim_end_matches('/'));
    let response = build_http_client()?
        .post(endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", options.client_id.as_str()),
            ("code_verifier", pkce.verifier.as_str()),
        ])
        .send()
        .await
        .map_err(redacted_transport_error)?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(io::Error::other(format!(
            "ChatGPT token endpoint returned HTTP {status}: {}",
            oauth_error_message(&body)
        )));
    }
    let tokens = response
        .json::<TokenResponse>()
        .await
        .map_err(|error| io::Error::other(format!("decoding ChatGPT OAuth tokens: {error}")))?;
    Ok(LoginTokens {
        id_token: tokens.id_token,
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
    })
}

fn redacted_transport_error(error: reqwest::Error) -> io::Error {
    let kind = if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "could not connect"
    } else {
        "failed"
    };
    io::Error::other(format!("ChatGPT token exchange {kind}"))
}

fn build_http_client() -> io::Result<reqwest::Client> {
    let custom_ca_path = [CODEX_CA_CERT_ENV, SSL_CERT_FILE_ENV]
        .into_iter()
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .map(|value| (name, value))
        });
    let mut builder = reqwest::Client::builder();
    if let Some((source_env, path)) = custom_ca_path {
        let pem = std::fs::read(&path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("reading custom CA bundle from {source_env}={path:?}: {error}"),
            )
        })?;
        let certificates = reqwest::Certificate::from_pem_bundle(&pem).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("parsing custom CA bundle from {source_env}={path:?}: {error}"),
            )
        })?;
        if certificates.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("custom CA bundle from {source_env}={path:?} contains no certificates"),
            ));
        }
        for certificate in certificates {
            builder = builder.add_root_certificate(certificate);
        }
    }
    builder
        .build()
        .map_err(|error| io::Error::other(format!("building ChatGPT OAuth HTTP client: {error}")))
}

fn oauth_error_message(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return if body.trim().is_empty() {
            "empty error response".to_string()
        } else {
            "unstructured error response".to_string()
        };
    };
    value
        .pointer("/error/message")
        .or_else(|| value.get("error_description"))
        .or_else(|| value.get("message"))
        .or_else(|| value.get("error"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown OAuth error")
        .to_string()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCode {
    pub verification_url: String,
    pub user_code: String,
    device_auth_id: String,
    interval: u64,
}

#[derive(Serialize)]
struct UserCodeRequest<'a> {
    client_id: &'a str,
}

#[derive(Deserialize)]
struct UserCodeResponse {
    device_auth_id: String,
    #[serde(alias = "usercode")]
    user_code: String,
    #[serde(default, deserialize_with = "deserialize_interval")]
    interval: u64,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum IntervalValue {
    Number(u64),
    String(String),
}

fn deserialize_interval<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<IntervalValue>::deserialize(deserializer)?;
    match value {
        None => Ok(DEFAULT_DEVICE_POLL_INTERVAL),
        Some(IntervalValue::Number(value)) => Ok(value.max(1)),
        Some(IntervalValue::String(value)) => value
            .trim()
            .parse::<u64>()
            .map(|value| value.max(1))
            .map_err(serde::de::Error::custom),
    }
}

#[derive(Serialize)]
struct TokenPollRequest<'a> {
    device_auth_id: &'a str,
    user_code: &'a str,
}

#[derive(Deserialize)]
struct DeviceAuthorizationResponse {
    authorization_code: String,
    code_challenge: String,
    code_verifier: String,
}

pub async fn request_device_code(options: &LoginOptions) -> io::Result<DeviceCode> {
    let base = options.issuer.trim_end_matches('/');
    let response = build_http_client()?
        .post(format!("{base}/api/accounts/deviceauth/usercode"))
        .json(&UserCodeRequest {
            client_id: &options.client_id,
        })
        .send()
        .await
        .map_err(redacted_transport_error)?;
    if !response.status().is_success() {
        let status = response.status();
        let message = if status == StatusCode::NOT_FOUND {
            "device-code login is not enabled by this issuer"
        } else {
            "device-code request was rejected"
        };
        return Err(io::Error::other(format!("{message} (HTTP {status})")));
    }
    let response = response
        .json::<UserCodeResponse>()
        .await
        .map_err(|error| io::Error::other(format!("decoding ChatGPT device code: {error}")))?;
    Ok(DeviceCode {
        verification_url: format!("{base}/codex/device"),
        user_code: response.user_code,
        device_auth_id: response.device_auth_id,
        interval: response.interval.max(1),
    })
}

pub async fn complete_device_code_login(
    options: &LoginOptions,
    device_code: DeviceCode,
) -> io::Result<LoginTokens> {
    let base = options.issuer.trim_end_matches('/');
    let endpoint = format!("{base}/api/accounts/deviceauth/token");
    let client = build_http_client()?;
    let started = Instant::now();
    let authorization = loop {
        let response = client
            .post(&endpoint)
            .json(&TokenPollRequest {
                device_auth_id: &device_code.device_auth_id,
                user_code: &device_code.user_code,
            })
            .send()
            .await
            .map_err(redacted_transport_error)?;
        if response.status().is_success() {
            break response
                .json::<DeviceAuthorizationResponse>()
                .await
                .map_err(|error| {
                    io::Error::other(format!("decoding device authorization: {error}"))
                })?;
        }
        if !matches!(
            response.status(),
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
        ) {
            return Err(io::Error::other(format!(
                "device authorization failed with HTTP {}",
                response.status()
            )));
        }
        let remaining = DEVICE_AUTH_TIMEOUT.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "device authorization timed out after 15 minutes",
            ));
        }
        tokio::time::sleep(Duration::from_secs(device_code.interval).min(remaining)).await;
    };

    let pkce = PkceCodes {
        verifier: authorization.code_verifier,
        challenge: authorization.code_challenge,
    };
    let redirect_uri = format!("{base}/deviceauth/callback");
    exchange_code_for_tokens(
        options,
        &redirect_uri,
        &pkce,
        &authorization.authorization_code,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_token_url_matches_default_issuer() {
        assert_eq!(REFRESH_TOKEN_URL, format!("{DEFAULT_ISSUER}/oauth/token"));
    }

    #[test]
    fn authorize_url_preserves_the_codex_compatible_contract() {
        let options = LoginOptions {
            open_browser: false,
            ..LoginOptions::default()
        };
        let pkce = PkceCodes {
            verifier: "verifier".to_string(),
            challenge: "challenge".to_string(),
        };
        let url = build_authorize_url(
            &options,
            "http://localhost:1455/auth/callback",
            &pkce,
            "state",
        )
        .expect("authorize URL");
        let url = Url::parse(&url).expect("valid URL");
        let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(url.path(), "/oauth/authorize");
        assert_eq!(query.get("client_id").map(String::as_str), Some(CLIENT_ID));
        assert_eq!(
            query.get("code_challenge").map(String::as_str),
            Some("challenge")
        );
        assert_eq!(
            query.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(query.get("state").map(String::as_str), Some("state"));
        assert_eq!(
            query.get("originator").map(String::as_str),
            Some(ORIGINATOR)
        );
        assert!(query
            .get("scope")
            .is_some_and(|scope| scope.contains("offline_access")));
    }

    #[test]
    fn pkce_values_have_the_required_entropy_and_encoding() {
        let pkce = generate_pkce();
        assert!(pkce.verifier.len() >= 43 && pkce.verifier.len() <= 128);
        assert!(!pkce.verifier.contains('='));
        assert!(!pkce.challenge.contains('='));
        assert_ne!(pkce.verifier, generate_pkce().verifier);
    }

    #[test]
    fn oauth_errors_do_not_echo_unstructured_response_bodies() {
        assert_eq!(
            oauth_error_message(r#"{"error":{"message":"denied"}}"#),
            "denied"
        );
        assert_eq!(
            oauth_error_message("secret=do-not-log"),
            "unstructured error response"
        );
    }

    #[test]
    fn device_interval_accepts_string_and_number_encodings() {
        let string: UserCodeResponse = serde_json::from_value(serde_json::json!({
            "device_auth_id": "id",
            "user_code": "code",
            "interval": "7"
        }))
        .expect("string interval");
        let number: UserCodeResponse = serde_json::from_value(serde_json::json!({
            "device_auth_id": "id",
            "user_code": "code",
            "interval": 3
        }))
        .expect("numeric interval");
        assert_eq!(string.interval, 7);
        assert_eq!(number.interval, 3);
    }

    #[tokio::test]
    async fn callback_state_mismatch_is_rejected_without_completing_login() {
        let outcome = handle_callback_request(
            "/auth/callback?code=secret&state=wrong",
            &LoginOptions::default(),
            "http://localhost:1455/auth/callback",
            &generate_pkce(),
            "expected",
        )
        .await;
        let (status, body, completed) = outcome.into_parts();
        assert_eq!(status, 400);
        assert!(body.contains("state mismatch"));
        assert!(completed.is_none(), "a legitimate callback may still retry");
    }

    #[tokio::test]
    async fn callback_server_can_be_cancelled_without_leaking_tokens() {
        let server = run_login_server(LoginOptions {
            port: 0,
            open_browser: false,
            ..LoginOptions::default()
        })
        .expect("callback server");
        let cancel = server.cancel_handle();
        cancel.shutdown();
        let error = server.block_until_done().await.expect_err("cancelled");
        assert!(error.to_string().contains("cancelled"));
    }
}
