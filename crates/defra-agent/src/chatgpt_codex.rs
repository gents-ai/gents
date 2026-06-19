use std::future::Future;
use std::sync::Arc;
use std::{fmt, fmt::Formatter};

use anyhow::{Context, Result};
use bytes::Bytes;
use chrono::{DateTime, Duration, Utc};
use codex_login::default_client::default_headers;
use codex_model_provider_info::CHATGPT_CODEX_BASE_URL;
use defra_agent_protocol::row::OAuthCredentialRow;
use defra_node::EmbeddedNode;
use rig::http_client::{
    self, HeaderMap, HeaderValue, HttpClientExt, LazyBody, MultipartForm, Request, ReqwestClient,
    Response, StreamingResponse,
};
use rig::wasm_compat::WasmCompatSend;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

pub const CHATGPT_CODEX_PROVIDER: &str = "chatgpt-codex";
const OAUTH_CREDENTIAL_FIELDS: &str = "_docID credential_id agent_did provider access_token refresh_token id_token account_id chatgpt_plan_type is_fedramp access_token_expires_at last_refresh enabled";
const REFRESH_SKEW: Duration = Duration::minutes(5);

pub fn default_backend_endpoint() -> &'static str {
    CHATGPT_CODEX_BASE_URL
}

/// A user-actionable classification of why ChatGPT OAuth could not be used.
#[derive(Debug, Clone)]
pub enum ChatGptAuthProblem {
    /// No matching OAuthCredential document exists.
    Missing,
    /// A credential exists but is not usable for ChatGPT OAuth.
    WrongMode { found_mode: String },
    /// Credentials are ChatGPT OAuth but the token is expired or revoked.
    Expired,
    /// Anything else, with the underlying message.
    Other(String),
}

/// Render an actionable, multi-line message for a ChatGPT auth failure.
pub fn classify_chatgpt_auth_error(
    agent_did: &str,
    provider: &str,
    problem: &ChatGptAuthProblem,
) -> String {
    match problem {
        ChatGptAuthProblem::Missing => format!(
            "No OAuthCredential document found for agent {agent_did} and provider {provider}.\n\
             To use the ChatGPT subscription backend, run \
             `defra-agent codex-login --agent-did {agent_did}`."
        ),
        ChatGptAuthProblem::WrongMode { found_mode } => format!(
            "OAuthCredential for agent {agent_did} and provider {provider} is {found_mode}, \
             but the ChatGPT subscription backend needs an enabled ChatGPT OAuth credential.\n\
             Run `defra-agent codex-login --agent-did {agent_did}` or select an API-key backend."
        ),
        ChatGptAuthProblem::Expired => format!(
            "ChatGPT OAuth credential for agent {agent_did} and provider {provider} is expired or revoked.\n\
             Re-authenticate with `defra-agent codex-login --agent-did {agent_did}`."
        ),
        ChatGptAuthProblem::Other(detail) => {
            format!(
                "ChatGPT OAuth credential for agent {agent_did} and provider {provider} could not be used: {detail}"
            )
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthCredential {
    #[serde(default)]
    pub doc_id: Option<String>,
    pub credential_id: String,
    pub agent_did: String,
    pub provider: String,
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub chatgpt_plan_type: Option<String>,
    pub is_fedramp: bool,
    pub access_token_expires_at: DateTime<Utc>,
    #[serde(default)]
    pub last_refresh: Option<DateTime<Utc>>,
    pub enabled: bool,
}

impl OAuthCredential {
    pub fn from_login_token_data(
        agent_did: impl Into<String>,
        provider: impl Into<String>,
        token_data: &codex_login::TokenData,
        now: DateTime<Utc>,
    ) -> Self {
        let agent_did = agent_did.into();
        let provider = provider.into();
        let id_claims =
            crate::chatgpt_oauth_refresh::decode_id_token_claims(&token_data.id_token.raw_jwt);
        let access_token_expires_at =
            crate::chatgpt_oauth_refresh::jwt_expiration(&token_data.access_token)
                .or(id_claims.expires_at)
                .unwrap_or_else(|| now + Duration::hours(1));
        Self {
            doc_id: None,
            credential_id: oauth_credential_id(&agent_did, &provider),
            agent_did,
            provider,
            access_token: token_data.access_token.clone(),
            refresh_token: token_data.refresh_token.clone(),
            id_token: Some(token_data.id_token.raw_jwt.clone()),
            account_id: token_data.account_id.clone().or(id_claims.account_id),
            chatgpt_plan_type: token_data
                .id_token
                .get_chatgpt_plan_type_raw()
                .or(id_claims.plan_type),
            is_fedramp: token_data.id_token.is_fedramp_account() || id_claims.is_fedramp,
            access_token_expires_at,
            last_refresh: Some(now),
            enabled: true,
        }
    }
}

pub fn normalize_endpoint(endpoint: &str) -> String {
    let trimmed = endpoint.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        default_backend_endpoint().to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn oauth_credential_id(agent_did: &str, provider: &str) -> String {
    format!("{provider}:{agent_did}")
}

pub fn oauth_credential_query(agent_did: &str, provider: &str) -> String {
    let agent_did = crate::graphql::escape_graphql_string(agent_did);
    let provider = crate::graphql::escape_graphql_string(provider);
    format!(
        r#"query {{
            OAuthCredential(
                filter: {{
                    agent_did: {{ _eq: "{agent_did}" }},
                    provider: {{ _eq: "{provider}" }},
                    enabled: {{ _eq: true }}
                }},
                limit: 1
            ) {{
                {OAUTH_CREDENTIAL_FIELDS}
            }}
        }}"#
    )
}

pub fn oauth_credential_by_id_query(credential_id: &str) -> String {
    let credential_id = crate::graphql::escape_graphql_string(credential_id);
    format!(
        r#"query {{
            OAuthCredential(
                filter: {{ credential_id: {{ _eq: "{credential_id}" }} }},
                limit: 1
            ) {{
                {OAUTH_CREDENTIAL_FIELDS}
            }}
        }}"#
    )
}

pub fn oauth_credential_upsert_mutation(credential: &OAuthCredential) -> String {
    let input = oauth_credential_input(credential);
    let credential_id = crate::graphql::escape_graphql_string(&credential.credential_id);
    format!(
        r#"mutation {{
            upsert_OAuthCredential(
                filter: {{ credential_id: {{ _eq: "{credential_id}" }} }},
                add: {{
                    credential_id: "{credential_id}",
                    {input}
                }},
                update: {{
                    {input}
                }}
            ) {{ _docID }}
        }}"#
    )
}

pub async fn lookup_oauth_credential(
    node: &EmbeddedNode,
    agent_did: &str,
    provider: &str,
) -> Result<Option<OAuthCredential>> {
    let response = node
        .execute(&oauth_credential_query(agent_did, provider))
        .await;
    if response.has_errors() {
        anyhow::bail!(
            "querying OAuthCredential returned errors: {:?}",
            response.errors
        );
    }
    let response = json!({ "data": response.data.unwrap_or(Value::Null) });
    oauth_credentials_from_response(&response)
        .into_iter()
        .next()
        .transpose()
}

pub async fn lookup_oauth_credential_by_id(
    node: &EmbeddedNode,
    credential_id: &str,
) -> Result<Option<OAuthCredential>> {
    let response = node
        .execute(&oauth_credential_by_id_query(credential_id))
        .await;
    if response.has_errors() {
        anyhow::bail!(
            "querying OAuthCredential returned errors: {:?}",
            response.errors
        );
    }
    let response = json!({ "data": response.data.unwrap_or(Value::Null) });
    oauth_credentials_from_response(&response)
        .into_iter()
        .next()
        .transpose()
}

pub async fn upsert_oauth_credential(
    node: &EmbeddedNode,
    credential: &OAuthCredential,
) -> Result<String> {
    let response = node
        .execute(&oauth_credential_upsert_mutation(credential))
        .await;
    if response.has_errors() {
        anyhow::bail!(
            "upserting OAuthCredential returned errors: {:?}",
            response.errors
        );
    }
    let response = json!({ "data": response.data.unwrap_or(Value::Null) });
    defra_agent_protocol::graphql::extract_mutation_doc_id(&response, "OAuthCredential")
}

pub fn oauth_credentials_from_response(response: &Value) -> Vec<Result<OAuthCredential>> {
    defra_agent_protocol::graphql::graphql_rows_from_response(response, "OAuthCredential")
        .into_iter()
        .map(oauth_credential_from_value)
        .collect()
}

pub fn build_chatgpt_codex_headers(
    account_id: Option<&str>,
    is_fedramp: bool,
) -> Result<HeaderMap> {
    let mut headers = default_headers();
    if let Some(account_id) = account_id.map(str::trim).filter(|value| !value.is_empty()) {
        let account_id = HeaderValue::from_str(account_id)
            .context("ChatGPT account id could not be encoded as an HTTP header")?;
        headers.insert("ChatGPT-Account-ID", account_id);
    }
    if is_fedramp {
        headers.insert("X-OpenAI-Fedramp", HeaderValue::from_static("true"));
    }
    headers.insert(
        "version",
        HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
    );
    Ok(headers)
}

fn oauth_credential_input(credential: &OAuthCredential) -> String {
    let field = |name: &str, value: &str| {
        format!(
            r#"{name}: "{}""#,
            crate::graphql::escape_graphql_string(value)
        )
    };
    let datetime_field = |name: &str, value: Option<DateTime<Utc>>| {
        value
            .map(|value| {
                format!(
                    r#"{name}: "{}""#,
                    value.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                )
            })
            .unwrap_or_else(|| format!("{name}: null"))
    };
    [
        field("agent_did", &credential.agent_did),
        field("provider", &credential.provider),
        field("access_token", &credential.access_token),
        field("refresh_token", &credential.refresh_token),
        defra_agent_protocol::graphql::nullable_string_field(
            "id_token",
            credential.id_token.as_deref(),
        ),
        defra_agent_protocol::graphql::nullable_string_field(
            "account_id",
            credential.account_id.as_deref(),
        ),
        defra_agent_protocol::graphql::nullable_string_field(
            "chatgpt_plan_type",
            credential.chatgpt_plan_type.as_deref(),
        ),
        format!(
            "is_fedramp: {}",
            defra_agent_protocol::graphql::graphql_bool_literal(credential.is_fedramp)
        ),
        datetime_field(
            "access_token_expires_at",
            Some(credential.access_token_expires_at),
        ),
        datetime_field("last_refresh", credential.last_refresh),
        format!(
            "enabled: {}",
            defra_agent_protocol::graphql::graphql_bool_literal(credential.enabled)
        ),
    ]
    .join(",\n                    ")
}

fn oauth_credential_from_value(value: Value) -> Result<OAuthCredential> {
    let row: OAuthCredentialRow =
        serde_json::from_value(value).context("decoding OAuthCredential row")?;
    let access_token = required(row.access_token, "access_token")?;
    let refresh_token = required(row.refresh_token, "refresh_token")?;
    Ok(OAuthCredential {
        doc_id: row.doc_id,
        credential_id: row.credential_id,
        agent_did: required(row.agent_did, "agent_did")?,
        provider: required(row.provider, "provider")?,
        access_token,
        refresh_token,
        id_token: clean_optional(row.id_token),
        account_id: clean_optional(row.account_id),
        chatgpt_plan_type: clean_optional(row.chatgpt_plan_type),
        is_fedramp: row.is_fedramp.unwrap_or(false),
        access_token_expires_at: parse_required_datetime(
            row.access_token_expires_at,
            "access_token_expires_at",
        )?,
        last_refresh: parse_optional_datetime(row.last_refresh, "last_refresh")?,
        enabled: row.enabled.unwrap_or(true),
    })
}

fn required(value: Option<String>, field: &str) -> Result<String> {
    clean_optional(value).ok_or_else(|| anyhow::anyhow!("OAuthCredential missing {field}"))
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(value)
    })
}

fn parse_required_datetime(value: Option<String>, field: &str) -> Result<DateTime<Utc>> {
    parse_optional_datetime(value, field)?
        .ok_or_else(|| anyhow::anyhow!("OAuthCredential missing {field}"))
}

fn parse_optional_datetime(value: Option<String>, field: &str) -> Result<Option<DateTime<Utc>>> {
    value
        .map(|value| {
            DateTime::parse_from_rfc3339(&value)
                .map(|value| value.with_timezone(&Utc))
                .with_context(|| format!("parsing OAuthCredential {field} timestamp {value}"))
        })
        .transpose()
}

fn token_is_fresh(expires_at: DateTime<Utc>) -> bool {
    Utc::now() + REFRESH_SKEW < expires_at
}

/// Supplies a current OAuth bearer, refreshing it as needed.
pub trait BearerSource: Send + Sync {
    fn current_bearer(&self) -> impl Future<Output = Result<String>> + Send;
}

pub struct DbCredentialBearer {
    node: Arc<EmbeddedNode>,
    agent_did: String,
    provider: String,
    credential_id: String,
    http: reqwest::Client,
    /// In-memory authoritative credential. Holds any refresh token rotated at the provider but
    /// not yet persisted, so it is the source of truth for a subsequent in-process refresh.
    cache: Mutex<Option<OAuthCredential>>,
    refresh_lock: Mutex<()>,
    is_owner: bool,
}

impl DbCredentialBearer {
    pub fn new(
        node: Arc<EmbeddedNode>,
        agent_did: impl Into<String>,
        provider: impl Into<String>,
        credential_id: impl Into<String>,
        is_owner: bool,
    ) -> Self {
        Self {
            node,
            agent_did: agent_did.into(),
            provider: provider.into(),
            credential_id: credential_id.into(),
            http: reqwest::Client::new(),
            cache: Mutex::new(None),
            refresh_lock: Mutex::new(()),
            is_owner,
        }
    }

    async fn load_credential(&self) -> Result<OAuthCredential> {
        let credential = lookup_oauth_credential_by_id(self.node.as_ref(), &self.credential_id)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(classify_chatgpt_auth_error(
                    &self.agent_did,
                    &self.provider,
                    &ChatGptAuthProblem::Missing,
                ))
            })?;
        if !credential.enabled {
            anyhow::bail!(
                "{}",
                classify_chatgpt_auth_error(
                    &self.agent_did,
                    &self.provider,
                    &ChatGptAuthProblem::WrongMode {
                        found_mode: "disabled".to_string(),
                    },
                )
            );
        }
        Ok(credential)
    }

    async fn cached(&self) -> Option<OAuthCredential> {
        self.cache.lock().await.clone()
    }

    async fn cache_credential(&self, credential: &OAuthCredential) {
        *self.cache.lock().await = Some(credential.clone());
    }

    /// Persist the credential with bounded retry. Called only after a successful provider
    /// refresh, where the in-memory cache already holds the rotated token — so a persist failure
    /// degrades to "serve from memory + log", never to stranding a consumed refresh token.
    async fn persist_with_retry(&self, credential: &OAuthCredential) -> Result<()> {
        let mut last_error = None;
        let mut delay_ms = 200u64;
        for attempt in 0..3u32 {
            match upsert_oauth_credential(self.node.as_ref(), credential).await {
                Ok(_) => return Ok(()),
                Err(error) => {
                    last_error = Some(error);
                    if attempt + 1 < 3 {
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        delay_ms *= 2;
                    }
                }
            }
        }
        Err(last_error.expect("persist_with_retry ran at least one failing attempt"))
    }

    fn auth_error(&self, problem: &ChatGptAuthProblem) -> anyhow::Error {
        anyhow::anyhow!(classify_chatgpt_auth_error(
            &self.agent_did,
            &self.provider,
            problem,
        ))
    }
}

/// Copy refreshed token material onto the credential, preserving prior optional fields when the
/// refresh response omits them.
fn apply_refreshed_tokens(
    credential: &mut OAuthCredential,
    refreshed: crate::chatgpt_oauth_refresh::RefreshedTokens,
) {
    credential.access_token = refreshed.access_token;
    credential.refresh_token = refreshed.refresh_token;
    if refreshed.id_token.is_some() {
        credential.id_token = refreshed.id_token;
    }
    if refreshed.account_id.is_some() {
        credential.account_id = refreshed.account_id;
    }
    if refreshed.plan_type.is_some() {
        credential.chatgpt_plan_type = refreshed.plan_type;
    }
    credential.is_fedramp = refreshed.is_fedramp || credential.is_fedramp;
    credential.access_token_expires_at = refreshed.access_token_expires_at;
    credential.last_refresh = Some(Utc::now());
}

impl BearerSource for DbCredentialBearer {
    async fn current_bearer(&self) -> Result<String> {
        if let Some(cred) = self.cached().await {
            if token_is_fresh(cred.access_token_expires_at) {
                return Ok(cred.access_token);
            }
        }

        let _guard = self.refresh_lock.lock().await;

        // Re-check under the lock: another turn may have refreshed while we waited.
        if let Some(cred) = self.cached().await {
            if token_is_fresh(cred.access_token_expires_at) {
                return Ok(cred.access_token);
            }
        }

        // Authoritative base: the in-memory credential (which may hold a rotated-but-not-yet-
        // persisted refresh token) wins over the DB; fall back to the DB on a cold cache.
        let mut credential = match self.cached().await {
            Some(cred) => cred,
            None => {
                let cred = self.load_credential().await?;
                self.cache_credential(&cred).await;
                cred
            }
        };
        if token_is_fresh(credential.access_token_expires_at) {
            return Ok(credential.access_token);
        }

        if !self.is_owner {
            // Non-owner nodes never refresh: a second writer racing the owner with the same
            // refresh token triggers provider reuse-detection and revokes the credential. They
            // serve the latest replicated token and rely on the owner's write-back.
            return Ok(credential.access_token);
        }

        // Owner is about to mint a new token. Re-load from DefraDB first so a credential disabled
        // after the cache warmed stops serving (load_credential bails when `enabled` is false),
        // and so an externally-refreshed token is adopted. Take the DB copy when it is at least as
        // fresh; keep the in-memory copy only when it is strictly ahead — i.e. a prior persist
        // failed and the cache still holds the not-yet-written rotated refresh token.
        let db_credential = self.load_credential().await?;
        if db_credential.access_token_expires_at >= credential.access_token_expires_at {
            credential = db_credential;
            if token_is_fresh(credential.access_token_expires_at) {
                self.cache_credential(&credential).await;
                return Ok(credential.access_token);
            }
        }

        // Owner refresh. The provider ROTATES the refresh token here: once this returns 200 the
        // old refresh token is consumed and `refreshed` is the ONLY source of truth for it.
        let refreshed = crate::chatgpt_oauth_refresh::refresh_chatgpt_token(
            &credential.refresh_token,
            &self.http,
        )
        .await
        .map_err(|problem| self.auth_error(&problem))?;
        apply_refreshed_tokens(&mut credential, refreshed);

        // Make the rotated credential the in-memory source of truth BEFORE persisting, so a
        // write failure can never strand the consumed refresh token and force a reuse-revoking
        // re-refresh on the next request.
        self.cache_credential(&credential).await;
        if let Err(error) = self.persist_with_retry(&credential).await {
            tracing::error!(
                agent_did = %self.agent_did,
                credential_id = %self.credential_id,
                %error,
                "failed to persist rotated ChatGPT OAuth token to DefraDB after retries; serving \
                 the rotated token from memory. It must be re-persisted before this process exits \
                 or the rotated refresh token will be lost."
            );
        }
        Ok(credential.access_token)
    }
}

pub struct ChatGptCodexHttpClient<S: BearerSource> {
    inner: ReqwestClient,
    bearer: Option<Arc<S>>,
}

impl<S: BearerSource> Clone for ChatGptCodexHttpClient<S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            bearer: self.bearer.clone(),
        }
    }
}

impl<S: BearerSource> fmt::Debug for ChatGptCodexHttpClient<S> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatGptCodexHttpClient")
            .field("inner", &self.inner)
            .field("bearer_configured", &self.bearer.is_some())
            .finish()
    }
}

impl<S: BearerSource> Default for ChatGptCodexHttpClient<S> {
    fn default() -> Self {
        Self {
            inner: ReqwestClient::default(),
            bearer: None,
        }
    }
}

impl<S: BearerSource> ChatGptCodexHttpClient<S> {
    pub fn new(bearer: Arc<S>) -> Self {
        Self {
            inner: ReqwestClient::default(),
            bearer: Some(bearer),
        }
    }

    async fn fresh_auth_header(&self) -> http_client::Result<HeaderValue> {
        let bearer = self.bearer.as_ref().ok_or_else(|| {
            http_client::Error::Instance(
                anyhow::anyhow!("ChatGptCodexHttpClient used without a configured BearerSource")
                    .into(),
            )
        })?;
        let token = bearer
            .current_bearer()
            .await
            .map_err(|error| http_client::Error::Instance(error.into()))?;
        HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|error| http_client::Error::Instance(anyhow::Error::from(error).into()))
    }

    async fn prepare(&self, req: Request<Bytes>) -> http_client::Result<Request<Bytes>> {
        let req = Self::inject_required_instructions(req);
        let value = self.fresh_auth_header().await?;
        let (mut parts, body) = req.into_parts();
        parts.headers.insert("authorization", value);
        Ok(Request::from_parts(parts, body))
    }

    fn inject_required_instructions(req: Request<Bytes>) -> Request<Bytes> {
        let (parts, body) = req.into_parts();
        let mut body = body;
        if parts.uri.path().ends_with("/responses") {
            if let Some(patched) = patch_instructions_body(&body) {
                body = patched;
            }
        }
        Request::from_parts(parts, body)
    }

    #[cfg(test)]
    pub async fn prepare_for_test(
        &self,
        req: Request<Bytes>,
    ) -> http_client::Result<Request<Bytes>> {
        self.prepare(req).await
    }
}

impl<S: BearerSource + 'static> HttpClientExt for ChatGptCodexHttpClient<S> {
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes>,
        U: WasmCompatSend + 'static,
    {
        let inner = self.inner.clone();
        let this = self.clone();
        let (parts, body) = req.into_parts();
        let req = Request::from_parts(parts, body.into());
        async move {
            let req = this.prepare(req).await?;
            send_reqwest(inner, req).await
        }
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes>,
        U: WasmCompatSend + 'static,
    {
        let inner = self.inner.clone();
        let this = self.clone();
        async move {
            let value = this.fresh_auth_header().await?;
            let (mut parts, body) = req.into_parts();
            parts.headers.insert("authorization", value);
            let req = Request::from_parts(parts, body);
            HttpClientExt::send_multipart(&inner, req).await
        }
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes>,
    {
        let inner = self.inner.clone();
        let this = self.clone();
        let (parts, body) = req.into_parts();
        let req = Request::from_parts(parts, body.into());
        async move {
            let req = this.prepare(req).await?;
            HttpClientExt::send_streaming(&inner, req).await
        }
    }
}

async fn send_reqwest<U>(
    inner: ReqwestClient,
    req: Request<Bytes>,
) -> http_client::Result<Response<LazyBody<U>>>
where
    U: From<Bytes>,
    U: WasmCompatSend + 'static,
{
    let is_responses_request = req.uri().path().ends_with("/responses");
    let request_body = req.body().clone();
    let (parts, body) = req.into_parts();
    let response = inner
        .request(parts.method, parts.uri.to_string())
        .headers(parts.headers)
        .body(body)
        .send()
        .await
        .map_err(|error| http_client::Error::Instance(error.into()))?;

    let status = response.status();
    let headers = response.headers().clone();
    if !status.is_success() {
        return Err(http_client::Error::InvalidStatusCodeWithMessage(
            status,
            response.text().await.unwrap_or_default(),
        ));
    }

    let body = if is_responses_request {
        let text = response
            .text()
            .await
            .map_err(|error| http_client::Error::Instance(error.into()))?;
        synthesize_completion_response(&request_body, &text)
    } else {
        response
            .bytes()
            .await
            .map_err(|error| http_client::Error::Instance(error.into()))?
    };

    let mut response_builder = Response::builder().status(status);
    if let Some(response_headers) = response_builder.headers_mut() {
        *response_headers = headers;
    }
    let body: LazyBody<U> = Box::pin(async move { Ok(U::from(body)) });
    response_builder
        .body(body)
        .map_err(http_client::Error::Protocol)
}

fn patch_instructions_body(body: &[u8]) -> Option<Bytes> {
    let mut value = serde_json::from_slice::<Value>(body).ok()?;
    let mut changed = false;

    if value.get("instructions").is_none() {
        let instructions = first_system_text(value.get("input")?)?;
        value["instructions"] = Value::String(instructions);
        if let Some(input) = value.get_mut("input") {
            strip_system_items(input);
        }
        changed = true;
    }
    if value.get("store").is_none() {
        value["store"] = Value::Bool(false);
        changed = true;
    }
    if value.get("stream").is_none() {
        value["stream"] = Value::Bool(true);
        changed = true;
    }
    if !changed {
        return None;
    }
    serde_json::to_vec(&value).ok().map(Bytes::from)
}

fn first_system_text(input: &Value) -> Option<String> {
    match input {
        Value::Array(items) => items.iter().find_map(system_item_text),
        Value::Object(_) => system_item_text(input),
        _ => None,
    }
}

fn system_item_text(item: &Value) -> Option<String> {
    if item.get("role").and_then(Value::as_str) != Some("system") {
        return None;
    }
    content_text(item.get("content")?)
}

fn strip_system_items(input: &mut Value) {
    match input {
        Value::Array(items) => {
            items.retain(|item| item.get("role").and_then(Value::as_str) != Some("system"));
        }
        Value::Object(item) if item.get("role").and_then(Value::as_str) == Some("system") => {
            item.clear();
        }
        Value::Object(_) => {}
        _ => {}
    }
}

fn synthesize_completion_response(request_body: &[u8], sse_body: &str) -> Bytes {
    if let Some(response) = completed_response(sse_body) {
        if let Ok(body) = serde_json::to_vec(&response) {
            return Bytes::from(body);
        }
    }

    let model = serde_json::from_slice::<Value>(request_body)
        .ok()
        .and_then(|request| {
            request
                .get("model")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "gpt-5.2".to_string());
    let text = streamed_output_text(sse_body);
    let response = json!({
        "id": "defra-chatgpt-codex-response",
        "object": "response",
        "created_at": chrono::Utc::now().timestamp().max(0) as u64,
        "status": "completed",
        "error": null,
        "incomplete_details": null,
        "instructions": null,
        "max_output_tokens": null,
        "model": model,
        "usage": null,
        "output": [
            {
                "type": "message",
                "id": "defra-chatgpt-codex-message",
                "role": "assistant",
                "status": "completed",
                "content": [
                    {
                        "type": "output_text",
                        "text": text
                    }
                ]
            }
        ]
    });
    Bytes::from(serde_json::to_vec(&response).unwrap_or_else(|_| b"{}".to_vec()))
}

fn completed_response(sse_body: &str) -> Option<Value> {
    sse_events(sse_body).into_iter().find_map(|event| {
        if event.get("type").and_then(Value::as_str) == Some("response.completed") {
            event
                .get("response")
                .filter(|response| response.get("output").is_some())
                .cloned()
        } else {
            None
        }
    })
}

fn streamed_output_text(sse_body: &str) -> String {
    let mut deltas = String::new();
    let mut done_text = None;
    for event in sse_events(sse_body) {
        match event.get("type").and_then(Value::as_str) {
            Some("response.output_text.delta") => {
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    deltas.push_str(delta);
                }
            }
            Some("response.output_text.done") => {
                done_text = event
                    .get("text")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
            }
            _ => {}
        }
    }
    if deltas.is_empty() {
        done_text.unwrap_or_default()
    } else {
        deltas
    }
}

fn sse_events(sse_body: &str) -> Vec<Value> {
    sse_body
        .split("\n\n")
        .filter_map(|event| {
            let data = event
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim_start)
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() || data == "[DONE]" {
                return None;
            }
            serde_json::from_str::<Value>(&data).ok()
        })
        .collect()
}

fn content_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let text = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            (!text.trim().is_empty()).then_some(text)
        }
        Value::Object(part) => part
            .get("text")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        _ => None,
    }
}

pub async fn build_responses_client(
    node: Arc<EmbeddedNode>,
    agent_did: &str,
    endpoint: &str,
) -> Result<rig::providers::openai::Client<ChatGptCodexHttpClient<DbCredentialBearer>>> {
    let provider = CHATGPT_CODEX_PROVIDER;
    let credential = lookup_oauth_credential(node.as_ref(), agent_did, provider)
        .await
        .with_context(|| format!("loading OAuthCredential for agent {agent_did}"))?
        .ok_or_else(|| {
            anyhow::anyhow!(classify_chatgpt_auth_error(
                agent_did,
                provider,
                &ChatGptAuthProblem::Missing,
            ))
        })?;
    let headers =
        build_chatgpt_codex_headers(credential.account_id.as_deref(), credential.is_fedramp)?;
    let endpoint = normalize_endpoint(endpoint);
    let bearer = DbCredentialBearer::new(
        node,
        agent_did,
        provider,
        credential.credential_id.clone(),
        /*is_owner*/ true,
    );
    let http = ChatGptCodexHttpClient::new(Arc::new(bearer));
    crate::inference_http::build_openai_responses_client(
        "chatgpt-oauth-managed",
        &endpoint,
        http,
        headers,
    )
    .context("building ChatGPT Codex Responses client")
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingBearer {
        token: String,
        calls: AtomicUsize,
    }

    impl BearerSource for CountingBearer {
        async fn current_bearer(&self) -> Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.token.clone())
        }
    }

    #[test]
    fn classifies_missing_auth_with_login_guidance() {
        let msg = classify_chatgpt_auth_error(
            "did:key:zAgent",
            CHATGPT_CODEX_PROVIDER,
            &ChatGptAuthProblem::Missing,
        );

        assert!(msg.contains("did:key:zAgent"), "names the agent DID: {msg}");
        assert!(
            msg.contains("defra-agent codex-login"),
            "tells the user how to fix it: {msg}"
        );
    }

    #[test]
    fn classifies_wrong_mode_naming_found_mode() {
        let msg = classify_chatgpt_auth_error(
            "did:key:zAgent",
            CHATGPT_CODEX_PROVIDER,
            &ChatGptAuthProblem::WrongMode {
                found_mode: "disabled".to_string(),
            },
        );

        assert!(msg.contains("ChatGPT"), "asks for ChatGPT OAuth: {msg}");
        assert!(msg.contains("disabled"), "names what was found: {msg}");
    }

    #[test]
    fn classifies_expired_with_reauth_guidance() {
        let msg = classify_chatgpt_auth_error(
            "did:key:zAgent",
            CHATGPT_CODEX_PROVIDER,
            &ChatGptAuthProblem::Expired,
        );

        assert!(msg.to_lowercase().contains("expired"), "{msg}");
        assert!(msg.contains("defra-agent codex-login"), "{msg}");
    }

    #[tokio::test]
    async fn injects_fresh_bearer_on_each_request() {
        let bearer = Arc::new(CountingBearer {
            token: "tok-123".to_string(),
            calls: AtomicUsize::new(0),
        });
        let client = ChatGptCodexHttpClient::new(bearer.clone());

        let req = Request::builder()
            .method("POST")
            .uri("https://example.com/v1/responses")
            .header("authorization", "Bearer STALE")
            .body(Bytes::from_static(b"{}"))
            .unwrap();

        let prepared = client.prepare_for_test(req).await.unwrap();
        let auth = prepared
            .headers()
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap();

        assert_eq!(auth, "Bearer tok-123", "stale bearer was replaced");
        assert_eq!(
            bearer.calls.load(Ordering::SeqCst),
            1,
            "refreshed once per request"
        );
    }

    #[test]
    fn patches_rig_responses_body_for_chatgpt_codex() {
        let body = json!({
            "model": "gpt-5.2",
            "input": [
                {
                    "type": "message",
                    "role": "system",
                    "content": [
                        { "type": "input_text", "text": "Use terse answers." }
                    ]
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [
                        { "type": "input_text", "text": "Say pong." }
                    ]
                }
            ]
        });

        let patched = patch_instructions_body(&serde_json::to_vec(&body).unwrap()).unwrap();
        let patched: Value = serde_json::from_slice(&patched).unwrap();

        assert_eq!(
            patched.get("instructions").and_then(Value::as_str),
            Some("Use terse answers.")
        );
        assert_eq!(patched.get("store").and_then(Value::as_bool), Some(false));
        assert_eq!(patched.get("stream").and_then(Value::as_bool), Some(true));
        assert!(patched
            .get("input")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .all(|item| item.get("role").and_then(Value::as_str) != Some("system")));
    }

    #[test]
    fn streamed_output_prefers_deltas_over_done_text() {
        let sse = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"pong\"}\n",
            "\n",
            "event: response.output_text.done\n",
            "data: {\"type\":\"response.output_text.done\",\"text\":\"pong\"}\n",
            "\n"
        );

        assert_eq!(streamed_output_text(sse), "pong");
    }
}
