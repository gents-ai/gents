use rapidhash::RapidHashSet;
use std::fmt;
use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;
use futures::StreamExt;
use reqwest::StatusCode;
use rig::client::CompletionClient;
use rig::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, GetTokenUsage, Usage,
};
use rig::http_client::ReqwestClient;
use rig::streaming::{RawStreamingChoice, StreamingCompletionResponse};
use rig::OneOrMany;
use serde::{Deserialize, Serialize};

use chrono::DateTime;
use gents_claude::rotation::Family;
use gents_loop::claude_messages_body::SESSION_KEY_PARAM;
use gents_loop::provider_limit::{mark_account_rotated, ProviderLimitHeaders};

use crate::claude_messages::{non_success_message, open_messages_at};
use crate::claude_oauth::{CLAUDE_OAUTH_PRODUCT, CLAUDE_OAUTH_PROVIDER};
use crate::oauth_credential::{BearerSource, DbCredentialBearer, OAuthRefreshKind};
use crate::usage_observation::now_unix_secs;
use crate::usage_observation::{RotationFeed, UsageAccount, UsageCache, UsageReporter};

pub const DEFAULT_BACKEND_ENDPOINT: &str = "claude-cli://subscription";
pub const ANTHROPIC_API_ENDPOINT: &str = "https://api.anthropic.com/v1";
pub const DEFAULT_MODEL_ID: &str = "claude-opus-5-5";

pub fn default_backend_endpoint() -> &'static str {
    DEFAULT_BACKEND_ENDPOINT
}

pub fn default_model_name() -> &'static str {
    DEFAULT_MODEL_ID
}

/// One Claude account a client can send with: its `OAuthCredential` id, bearer and usage reporter.
pub(crate) struct ClaudeAccount<S>
where
    S: BearerSource,
{
    pub(crate) credential_id: String,
    pub(crate) bearer: Arc<S>,
    pub(crate) usage: Option<Arc<UsageReporter>>,
}

/// Rotation over a pool backend's accounts, shared through the runtime's usage cache.
#[derive(Clone)]
pub(crate) struct Rotation {
    pub(crate) policy: gents_claude::rotation::Policy,
    pub(crate) cache: Arc<UsageCache>,
    pub(crate) credential_ids: Arc<[String]>,
}

pub struct ClaudeSubscriptionClient<S = DbCredentialBearer>
where
    S: BearerSource,
{
    /// Never empty.
    pub(crate) accounts: Arc<[ClaudeAccount<S>]>,
    /// `None` is a single account with no rotation.
    pub(crate) rotation: Option<Rotation>,
    pub(crate) http: ReqwestClient,
}

impl<S> Clone for ClaudeSubscriptionClient<S>
where
    S: BearerSource,
{
    fn clone(&self) -> Self {
        Self {
            accounts: self.accounts.clone(),
            rotation: self.rotation.clone(),
            http: self.http.clone(),
        }
    }
}

impl<S> fmt::Debug for ClaudeSubscriptionClient<S>
where
    S: BearerSource,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeSubscriptionClient")
            .finish_non_exhaustive()
    }
}

impl ClaudeSubscriptionClient<DbCredentialBearer> {
    pub async fn build(
        cache: Arc<UsageCache>,
        node: Arc<EmbeddedNode>,
        node_did: &str,
        account_ref: Option<&str>,
    ) -> Result<Self> {
        let account = Self::bootstrap_account(&cache, &node, node_did, account_ref).await?;
        Ok(Self {
            accounts: Arc::from([account]),
            rotation: None,
            http: ReqwestClient::new(),
        })
    }

    /// A client that rotates over every enabled Claude account of the node.
    pub async fn build_pool(
        cache: Arc<UsageCache>,
        node: Arc<EmbeddedNode>,
        node_did: &str,
        allow_overage: bool,
    ) -> Result<Self> {
        let access = crate::config_client::ConfigAccess::local(node.clone());
        let rows =
            crate::oauth_credential::resolve_oauth_pool(&access, node_did, CLAUDE_OAUTH_PROVIDER)
                .await?;
        if rows.is_empty() {
            anyhow::bail!(
                "no enabled Claude account on this node; sign in with `gents claude-login`"
            );
        }
        // One concurrent bootstrap per account; an account whose sign-in cannot be
        // loaded or refreshed leaves the pool instead of taking the others down.
        let booted = futures::future::join_all(rows.iter().map(|row| {
            Self::bootstrap_account(&cache, &node, node_did, row.account_ref.as_deref())
        }))
        .await;
        let mut accounts = Vec::with_capacity(rows.len());
        let mut first_error = None;
        for (row, result) in rows.iter().zip(booted) {
            match result {
                Ok(account) => accounts.push(account),
                Err(error) => {
                    tracing::warn!(
                        credential_id = %row.credential_id,
                        error = %crate::failure_text::BoundedErrorChain(error.as_ref()),
                        "Claude account left the rotation pool: its sign-in could not be loaded"
                    );
                    first_error.get_or_insert(error);
                }
            }
        }
        if accounts.is_empty() {
            return Err(first_error
                .unwrap_or_else(|| anyhow::anyhow!("no Claude account could be loaded"))
                .context("no Claude account of this node could be loaded; sign in again with `gents claude-login`"));
        }
        let credential_ids: Arc<[String]> =
            accounts.iter().map(|a| a.credential_id.clone()).collect();
        Ok(Self {
            accounts: accounts.into(),
            rotation: Some(Rotation {
                policy: gents_claude::rotation::Policy { allow_overage },
                cache,
                credential_ids,
            }),
            http: ReqwestClient::new(),
        })
    }

    async fn bootstrap_account(
        cache: &Arc<UsageCache>,
        node: &Arc<EmbeddedNode>,
        node_did: &str,
        account_ref: Option<&str>,
    ) -> Result<ClaudeAccount<DbCredentialBearer>> {
        let (bearer, credential) = crate::oauth_http::bootstrap_oauth_client(
            node.clone(),
            node_did,
            CLAUDE_OAUTH_PROVIDER,
            OAuthRefreshKind::Claude,
            CLAUDE_OAUTH_PRODUCT,
            crate::oauth_credential::AccountPick::Reference(account_ref),
        )
        .await?;
        Ok(ClaudeAccount {
            credential_id: credential.credential_id.clone(),
            bearer,
            usage: Some(UsageReporter::new(
                cache.clone(),
                node.clone(),
                UsageAccount::for_credential(&credential),
            )),
        })
    }
}

impl ClaudeSubscriptionClient<ApiKeyBearer> {
    pub fn with_api_key(key: String, usage: Option<Arc<UsageReporter>>) -> Self {
        Self {
            accounts: Arc::from([ClaudeAccount {
                credential_id: String::new(),
                bearer: Arc::new(ApiKeyBearer::new(key)),
                usage,
            }]),
            rotation: None,
            http: ReqwestClient::new(),
        }
    }
}

impl<S> ClaudeSubscriptionClient<S>
where
    S: BearerSource,
{
    #[cfg(test)]
    pub(crate) fn with_bearer(bearer: Arc<S>) -> Self {
        Self {
            accounts: Arc::from([ClaudeAccount {
                credential_id: String::new(),
                bearer,
                usage: None,
            }]),
            rotation: None,
            http: ReqwestClient::new(),
        }
    }
}

impl<S> CompletionClient for ClaudeSubscriptionClient<S>
where
    S: BearerSource + 'static,
{
    type CompletionModel = ClaudeSubscriptionModel<S>;
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ClaudeStreamResponse {
    pub usage: Option<Usage>,
    #[serde(default)]
    pub token_usage: gents_protocol::usage::TokenUsage,
}

impl GetTokenUsage for ClaudeStreamResponse {
    fn token_usage(&self) -> Option<Usage> {
        self.usage
    }
}

pub struct ClaudeSubscriptionModel<S = DbCredentialBearer>
where
    S: BearerSource,
{
    model: String,
    /// Never empty; with rotation each reporter also feeds this model's family.
    accounts: Arc<[ClaudeAccount<S>]>,
    rotation: Option<Rotation>,
    http: ReqwestClient,
}

impl<S> Clone for ClaudeSubscriptionModel<S>
where
    S: BearerSource,
{
    fn clone(&self) -> Self {
        Self {
            model: self.model.clone(),
            accounts: self.accounts.clone(),
            rotation: self.rotation.clone(),
            http: self.http.clone(),
        }
    }
}

impl<S> fmt::Debug for ClaudeSubscriptionModel<S>
where
    S: BearerSource,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeSubscriptionModel")
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

fn surface_of(request: &CompletionRequest) -> RapidHashSet<String> {
    request.tools.iter().map(|tool| tool.name.clone()).collect()
}

impl<S> ClaudeSubscriptionModel<S>
where
    S: BearerSource + 'static,
{
    /// Open the response stream: on the one account, or on the pool's pick with its rotation bookkeeping.
    async fn open(
        &self,
        request: &CompletionRequest,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            Result<RawStreamingChoice<ClaudeStreamResponse>, CompletionError>,
        >,
        CompletionError,
    > {
        let surface = surface_of(request);
        match &self.rotation {
            Some(rotation) => open_rotated(
                crate::claude_messages::MESSAGES_URI,
                &self.model,
                request,
                surface,
                &self.accounts,
                rotation,
                &self.http,
            )
            .await
            .map(StreamExt::boxed),
            None => {
                let account = self.accounts.first().ok_or_else(|| {
                    CompletionError::ProviderError("Claude client has no account".to_string())
                })?;
                crate::claude_messages::stream_messages(
                    &self.model,
                    request,
                    surface,
                    account.bearer.as_ref(),
                    &self.http,
                    account.usage.clone(),
                )
                .await
                .map(StreamExt::boxed)
            }
        }
    }
}

impl<S> CompletionModel for ClaudeSubscriptionModel<S>
where
    S: BearerSource + 'static,
{
    type Response = ClaudeStreamResponse;
    type StreamingResponse = ClaudeStreamResponse;
    type Client = ClaudeSubscriptionClient<S>;

    fn make(client: &Self::Client, model: impl Into<String>) -> Self {
        let model = model.into();
        let family = gents_claude::rotation::Family::of_model(&model);
        let accounts = match client.rotation {
            None => client.accounts.clone(),
            Some(_) => client
                .accounts
                .iter()
                .map(|account| ClaudeAccount {
                    credential_id: account.credential_id.clone(),
                    bearer: account.bearer.clone(),
                    usage: account.usage.as_ref().map(|usage| {
                        usage.with_rotation(RotationFeed {
                            credential_id: account.credential_id.clone(),
                            family,
                        })
                    }),
                })
                .collect(),
        };
        Self {
            model,
            accounts,
            rotation: client.rotation.clone(),
            http: client.http.clone(),
        }
    }

    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        let mut stream = self.open(&request).await?;
        let mut text = String::new();
        let mut usage = Usage::new();
        let mut raw_response = ClaudeStreamResponse::default();
        while let Some(item) = stream.next().await {
            match item? {
                RawStreamingChoice::Message(chunk) => text.push_str(&chunk),
                RawStreamingChoice::FinalResponse(raw) => {
                    if let Some(reported) = raw.token_usage() {
                        usage = reported;
                    }
                    raw_response = raw;
                    break;
                }
                _ => {}
            }
        }
        if text.trim().is_empty() {
            return Err(CompletionError::ProviderError(
                "Claude Messages returned empty assistant text".to_string(),
            ));
        }
        Ok(CompletionResponse {
            choice: OneOrMany::one(rig::completion::AssistantContent::text(text)),
            usage,
            raw_response,
            message_id: None,
        })
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        let stream = self.open(&request).await?;
        Ok(StreamingCompletionResponse::stream(stream))
    }
}

// Per-call account choice for a pool backend. A failure is annotated
// `account-rotated` only when the pool now yields a different eligible account.

/// The session a request rotates under; requests without one share the empty key.
fn session_key(request: &CompletionRequest) -> &str {
    request
        .additional_params
        .as_ref()
        .and_then(|params| params.get(SESSION_KEY_PARAM))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

/// The failure when every account is exhausted for the family: a provider usage
/// limit, so the loop's usage-limit path reads its reset time.
fn pool_exhausted(reset_at: Option<u64>) -> CompletionError {
    let message = non_success_message(
        StatusCode::TOO_MANY_REQUESTS,
        None,
        "every Claude account on this node has reached its usage limit",
    );
    let limits = ProviderLimitHeaders {
        reset_at: reset_at
            .and_then(|at| i64::try_from(at).ok())
            .and_then(|at| DateTime::from_timestamp(at, 0)),
        usage_exhausted: true,
        ..Default::default()
    };
    CompletionError::ProviderError(limits.annotate(&message))
}

async fn open_rotated<S>(
    uri: &str,
    model: &str,
    request: &CompletionRequest,
    surface: RapidHashSet<String>,
    accounts: &Arc<[ClaudeAccount<S>]>,
    rotation: &Rotation,
    http: &ReqwestClient,
) -> Result<
    impl futures::Stream<Item = Result<RawStreamingChoice<ClaudeStreamResponse>, CompletionError>>,
    CompletionError,
>
where
    S: BearerSource,
{
    let pool = rotation.cache.claude_accounts();
    let (ids, policy) = (&rotation.credential_ids[..], &rotation.policy);
    let (family, session) = (Family::of_model(model), session_key(request));
    let now = now_unix_secs();
    let Some(choice) = pool.choose(ids, session, family, now, policy) else {
        return Err(pool_exhausted(
            pool.earliest_reset(ids, family, now, policy),
        ));
    };
    let account = accounts
        .get(choice.index)
        .ok_or_else(|| CompletionError::ProviderError("Claude pool lost an account".to_string()))?;
    match open_messages_at(
        uri,
        model,
        request,
        surface,
        account.bearer.as_ref(),
        http,
        account.usage.clone(),
    )
    .await
    {
        Ok(stream) => Ok(stream),
        Err(failure) if failure.rejected => {
            // The rejected response was observed into the pool before this error returned.
            let next = pool.choose(ids, session, family, now_unix_secs(), policy);
            Err(match (failure.error, next) {
                (CompletionError::ProviderError(text), Some(next))
                    if next.index != choice.index =>
                {
                    CompletionError::ProviderError(mark_account_rotated(&text))
                }
                (error, _) => error,
            })
        }
        Err(failure) => Err(failure.error),
    }
}

/// An Anthropic API key as the bearer. No `Debug`, so the key cannot reach logs.
pub struct ApiKeyBearer(String);

impl ApiKeyBearer {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }
}

impl BearerSource for ApiKeyBearer {
    async fn current_bearer(&self) -> Result<String> {
        Ok(self.0.clone())
    }

    fn is_oauth(&self) -> bool {
        false
    }
}

#[cfg(test)]
pub(crate) struct StaticBearer {
    token: std::result::Result<String, String>,
    calls: std::sync::atomic::AtomicUsize,
    invalidations: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl StaticBearer {
    pub(crate) fn new(token: &str) -> Self {
        Self {
            token: Ok(token.to_string()),
            calls: Default::default(),
            invalidations: Default::default(),
        }
    }

    pub(crate) fn failing(message: &str) -> Self {
        Self {
            token: Err(message.to_string()),
            calls: Default::default(),
            invalidations: Default::default(),
        }
    }

    pub(crate) fn bearer_calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn invalidations(&self) -> usize {
        self.invalidations.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
impl BearerSource for StaticBearer {
    async fn current_bearer(&self) -> Result<String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.token
            .clone()
            .map_err(|message| anyhow::anyhow!(message))
    }

    async fn invalidate(&self) {
        self.invalidations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
#[path = "claude_subscription/tests.rs"]
mod tests;
