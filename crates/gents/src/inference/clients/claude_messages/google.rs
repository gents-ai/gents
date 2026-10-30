//! Google credentials of the Vertex AI and Claude Platform on Google Cloud
//! routes, as the google-auth-library vendored in Claude Code 2.1.311 uses a
//! key file: a service account key signs an RS256 assertion
//! (`{iss, scope, aud, exp, iat}`, header `{alg}`) exchanged with the
//! `jwt-bearer` grant, and an authorized user file trades its refresh token.
//! Both go to `https://oauth2.googleapis.com/token` with the cloud-platform
//! scope. Access tokens are cached process-wide per credential, one exchange
//! in flight, and refreshed five minutes before they expire (the library's
//! eager refresh). The secrets never print and never reach an error message.

use std::fmt;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{RsaKeyPair, RSA_PKCS1_SHA256};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use kovan_map::HopscotchMap;
use rapidhash::fast::RandomState;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::OnceCell;

use crate::oauth_credential::BearerSource;

/// The scope Claude Code asks Google for.
pub(crate) const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
/// The library's token endpoint; a key file's `token_uri` is not consulted.
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const JWT_BEARER: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
/// Lifetime of a signed assertion.
const ASSERTION_SECONDS: u64 = 3600;
/// How long before expiry a token is replaced (`eagerRefreshThresholdMillis`).
const EAGER_REFRESH: Duration = Duration::from_secs(300);
/// Bound on one token exchange.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest token response read.
const MAX_TOKEN_BYTES: usize = 64 * 1024;
/// Credentials whose tokens are remembered before the cache starts over.
const TOKEN_CACHE_CAP: usize = 64;

/// Why Google credentials cannot produce a token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum GoogleCredentialError {
    /// The stored value is not a JSON key file.
    #[error("the Google credentials are not a JSON key file (line {line}, column {column})")]
    NotJson { line: usize, column: usize },
    /// The key file lacks a field its type needs.
    #[error("the Google {kind} credentials need the {field} field")]
    Missing {
        kind: &'static str,
        field: &'static str,
    },
    /// The key file is of a type this build does not read.
    #[error("Google {0} credentials are not supported; use a service account key or an authorized user file")]
    Unsupported(String),
    /// The service account's private key is not an RSA key in PEM.
    #[error("the service account private key must be a PEM RSA private key")]
    PrivateKey,
    /// The assertion could not be signed.
    #[error("the service account assertion could not be signed")]
    Sign,
    /// The token endpoint refused the exchange or could not be reached.
    #[error("Google refused the token exchange: {0}")]
    Exchange(String),
}

/// What a key file trades for an access token.
enum Grant {
    ServiceAccount {
        client_email: String,
        key: RsaKeyPair,
    },
    AuthorizedUser {
        client_id: String,
        client_secret: String,
        refresh_token: String,
    },
}

/// A parsed key file. Only the client email prints.
pub(crate) struct GoogleCredentials {
    grant: Grant,
    /// `quota_project_id`: Vertex sends it as `x-goog-user-project`.
    quota_project: Option<String>,
    /// The cache key: a digest of the stored JSON, never the JSON itself.
    fingerprint: [u8; 32],
}

impl fmt::Debug for GoogleCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match &self.grant {
            Grant::ServiceAccount { client_email, .. } => client_email.as_str(),
            Grant::AuthorizedUser { .. } => "authorized_user",
        };
        f.debug_struct("GoogleCredentials")
            .field("account", &kind)
            .field("quota_project", &self.quota_project)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct KeyFile {
    #[serde(rename = "type")]
    kind: Option<String>,
    client_email: Option<String>,
    private_key: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    refresh_token: Option<String>,
    quota_project_id: Option<String>,
}

fn required(
    value: Option<String>,
    kind: &'static str,
    field: &'static str,
) -> Result<String, GoogleCredentialError> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or(GoogleCredentialError::Missing { kind, field })
}

/// The DER of a PEM block, or `None` when `pem` is not one.
fn pem_der(pem: &str) -> Option<(bool, Vec<u8>)> {
    let pkcs8 = pem.contains("-----BEGIN PRIVATE KEY-----");
    let pkcs1 = pem.contains("-----BEGIN RSA PRIVATE KEY-----");
    if !pkcs8 && !pkcs1 {
        return None;
    }
    let body: String = pem
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("-----"))
        .collect();
    STANDARD.decode(body).ok().map(|der| (pkcs8, der))
}

impl GoogleCredentials {
    /// Parses a stored key file.
    pub(crate) fn parse(json: &str) -> Result<Self, GoogleCredentialError> {
        let file: KeyFile =
            serde_json::from_str(json).map_err(|error| GoogleCredentialError::NotJson {
                line: error.line(),
                column: error.column(),
            })?;
        let grant = match file.kind.as_deref() {
            Some("service_account") => {
                let kind = "service account";
                let client_email = required(file.client_email, kind, "client_email")?;
                let pem = required(file.private_key, kind, "private_key")?;
                let (pkcs8, der) = pem_der(&pem).ok_or(GoogleCredentialError::PrivateKey)?;
                let key = if pkcs8 {
                    RsaKeyPair::from_pkcs8(&der)
                } else {
                    RsaKeyPair::from_der(&der)
                }
                .map_err(|_| GoogleCredentialError::PrivateKey)?;
                Grant::ServiceAccount { client_email, key }
            }
            Some("authorized_user") => {
                let kind = "authorized user";
                Grant::AuthorizedUser {
                    client_id: required(file.client_id, kind, "client_id")?,
                    client_secret: required(file.client_secret, kind, "client_secret")?,
                    refresh_token: required(file.refresh_token, kind, "refresh_token")?,
                }
            }
            Some(other) => return Err(GoogleCredentialError::Unsupported(other.to_owned())),
            None => {
                return Err(GoogleCredentialError::Missing {
                    kind: "key file",
                    field: "type",
                })
            }
        };
        Ok(Self {
            grant,
            quota_project: file.quota_project_id.filter(|id| !id.is_empty()),
            fingerprint: Sha256::digest(json.as_bytes()).into(),
        })
    }

    /// The `x-goog-user-project` value the key file names.
    pub(crate) fn quota_project(&self) -> Option<&str> {
        self.quota_project.as_deref()
    }

    /// The signed `jwt-bearer` assertion at `iat` (Unix seconds).
    fn assertion(&self, iat: u64) -> Result<Option<String>, GoogleCredentialError> {
        #[derive(Serialize)]
        struct Claims<'a> {
            iss: &'a str,
            scope: &'a str,
            aud: &'a str,
            exp: u64,
            iat: u64,
        }
        let Grant::ServiceAccount { client_email, key } = &self.grant else {
            return Ok(None);
        };
        let claims = serde_json::to_vec(&Claims {
            iss: client_email,
            scope: CLOUD_PLATFORM_SCOPE,
            aud: TOKEN_URL,
            exp: iat + ASSERTION_SECONDS,
            iat,
        })
        .map_err(|_| GoogleCredentialError::Sign)?;
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#),
            URL_SAFE_NO_PAD.encode(claims)
        );
        let mut signature = vec![0; key.public_modulus_len()];
        key.sign(
            &RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            input.as_bytes(),
            &mut signature,
        )
        .map_err(|_| GoogleCredentialError::Sign)?;
        Ok(Some(format!(
            "{input}.{}",
            URL_SAFE_NO_PAD.encode(signature)
        )))
    }

    /// The token request's form fields.
    fn form(&self, iat: u64) -> Result<Vec<(&'static str, String)>, GoogleCredentialError> {
        Ok(match &self.grant {
            Grant::ServiceAccount { .. } => vec![
                ("grant_type", JWT_BEARER.to_owned()),
                (
                    "assertion",
                    self.assertion(iat)?.ok_or(GoogleCredentialError::Sign)?,
                ),
            ],
            Grant::AuthorizedUser {
                client_id,
                client_secret,
                refresh_token,
            } => vec![
                ("refresh_token", refresh_token.clone()),
                ("client_id", client_id.clone()),
                ("client_secret", client_secret.clone()),
                ("grant_type", "refresh_token".to_owned()),
            ],
        })
    }
}

/// An access token and when it stops being used.
struct Token {
    value: String,
    replace_at: Instant,
}

type TokenCache =
    HopscotchMap<[u8; 32], Arc<OnceCell<Result<Token, GoogleCredentialError>>>, RandomState>;

static TOKENS: LazyLock<TokenCache> =
    LazyLock::new(|| HopscotchMap::with_hasher(RandomState::new()));

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct TokenError {
    error: Option<String>,
    error_description: Option<String>,
}

/// One token exchange at `url`.
async fn exchange(
    http: &reqwest::Client,
    url: &str,
    credentials: &GoogleCredentials,
) -> Result<Token, GoogleCredentialError> {
    let unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let form = credentials.form(unix)?;
    let started = Instant::now();
    let failed = |reason: String| GoogleCredentialError::Exchange(reason);
    let mut response = http
        .post(url)
        .form(&form)
        .timeout(EXCHANGE_TIMEOUT)
        .send()
        .await
        .map_err(|error| failed(format!("the token endpoint could not be reached ({error})")))?;
    let status = response.status();
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| failed(format!("the token answer was cut off ({error})")))?
    {
        if body.len() + chunk.len() > MAX_TOKEN_BYTES {
            return Err(failed("the token answer is too large".to_owned()));
        }
        body.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        // The library reports `<error>: <error_description>`; both are Google's codes, not secrets.
        let reason = serde_json::from_slice::<TokenError>(&body)
            .ok()
            .and_then(|error| {
                error.error.map(|code| match error.error_description {
                    Some(description) => format!("{code}: {description}"),
                    None => code,
                })
            })
            .unwrap_or_else(|| format!("status {status}"));
        return Err(failed(reason));
    }
    let answer: TokenResponse = serde_json::from_slice(&body)
        .map_err(|_| failed("the token answer is not JSON".to_owned()))?;
    let value = answer
        .access_token
        .filter(|token| !token.is_empty())
        .ok_or_else(|| failed("the token answer has no access token".to_owned()))?;
    let lifetime = Duration::from_secs(answer.expires_in.unwrap_or(ASSERTION_SECONDS));
    Ok(Token {
        value,
        replace_at: started + lifetime.saturating_sub(EAGER_REFRESH),
    })
}

/// A bearer source minting Google access tokens from one key file.
pub(crate) struct GoogleTokenSource {
    credentials: Arc<GoogleCredentials>,
    http: reqwest::Client,
    token_url: String,
}

impl fmt::Debug for GoogleTokenSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GoogleTokenSource")
            .field("credentials", &self.credentials)
            .finish_non_exhaustive()
    }
}

impl GoogleTokenSource {
    pub(crate) fn new(credentials: Arc<GoogleCredentials>) -> Self {
        Self::at(credentials, TOKEN_URL)
    }

    /// A source exchanging at `token_url`, for loopback tests.
    pub(crate) fn at(credentials: Arc<GoogleCredentials>, token_url: &str) -> Self {
        Self {
            credentials,
            http: reqwest::Client::new(),
            token_url: token_url.to_owned(),
        }
    }

    /// The current access token, exchanging once when none is fresh. A failed
    /// exchange is not remembered: the next request tries again.
    pub(crate) async fn access_token(&self) -> Result<String, GoogleCredentialError> {
        let key = self.credentials.fingerprint;
        let slot = match TOKENS.get(&key) {
            Some(slot)
                if slot.get().is_none_or(|token| {
                    token
                        .as_ref()
                        .is_ok_and(|token| Instant::now() < token.replace_at)
                }) =>
            {
                slot
            }
            stale => {
                if stale.is_none() && TOKENS.len() >= TOKEN_CACHE_CAP {
                    TOKENS.clear();
                }
                let fresh = Arc::new(OnceCell::new());
                TOKENS.insert(key, fresh.clone());
                fresh
            }
        };
        slot.get_or_init(|| exchange(&self.http, &self.token_url, &self.credentials))
            .await
            .as_ref()
            .map(|token| token.value.clone())
            .map_err(Clone::clone)
    }

    /// Drops the cached token so the next request exchanges again.
    pub(crate) fn invalidate(&self) {
        TOKENS.remove(&self.credentials.fingerprint);
    }
}

impl BearerSource for GoogleTokenSource {
    async fn current_bearer(&self) -> anyhow::Result<String> {
        Ok(self.access_token().await?)
    }

    async fn invalidate(&self) {
        GoogleTokenSource::invalidate(self);
    }
}

#[cfg(test)]
#[path = "google_tests.rs"]
mod tests;
