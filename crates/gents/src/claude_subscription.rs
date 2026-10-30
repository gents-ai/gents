use rapidhash::RapidHashSet;
use std::fmt;
use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;
use futures::StreamExt;
use rig::client::CompletionClient;
use rig::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, GetTokenUsage, Usage,
};
use rig::http_client::ReqwestClient;
use rig::streaming::{RawStreamingChoice, StreamingCompletionResponse};
use rig::OneOrMany;
use serde::{Deserialize, Serialize};

use crate::inference::auth::claude::{CLAUDE_OAUTH_PRODUCT, CLAUDE_OAUTH_PROVIDER};
use crate::inference::auth::credential::{BearerSource, DbCredentialBearer, OAuthRefreshKind};
use crate::usage_observation::{UsageAccount, UsageReporter};

pub const DEFAULT_BACKEND_ENDPOINT: &str = "claude-cli://subscription";
pub const ANTHROPIC_API_ENDPOINT: &str = "https://api.anthropic.com/v1";
pub const DEFAULT_MODEL_ID: &str = "claude-opus-5-5";

pub fn default_backend_endpoint() -> &'static str {
    DEFAULT_BACKEND_ENDPOINT
}

pub fn default_model_name() -> &'static str {
    DEFAULT_MODEL_ID
}

pub struct ClaudeSubscriptionClient<S = DbCredentialBearer>
where
    S: BearerSource,
{
    pub(crate) bearer: Arc<S>,
    pub(crate) http: ReqwestClient,
    pub(crate) usage: Option<Arc<UsageReporter>>,
}

impl<S> Clone for ClaudeSubscriptionClient<S>
where
    S: BearerSource,
{
    fn clone(&self) -> Self {
        Self {
            bearer: self.bearer.clone(),
            http: self.http.clone(),
            usage: self.usage.clone(),
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
        cache: Arc<crate::usage_observation::UsageCache>,
        node: Arc<EmbeddedNode>,
        node_did: &str,
        account_ref: Option<&str>,
    ) -> Result<Self> {
        let (bearer, credential) = crate::inference::auth::http::bootstrap_oauth_client(
            node.clone(),
            node_did,
            CLAUDE_OAUTH_PROVIDER,
            OAuthRefreshKind::Claude,
            CLAUDE_OAUTH_PRODUCT,
            crate::inference::auth::credential::AccountPick::Reference(account_ref),
        )
        .await?;
        Ok(Self {
            bearer,
            http: ReqwestClient::new(),
            usage: Some(UsageReporter::new(
                cache,
                node,
                UsageAccount::for_credential(&credential),
            )),
        })
    }
}

impl ClaudeSubscriptionClient<ApiKeyBearer> {
    pub fn with_api_key(key: String) -> Self {
        Self {
            bearer: Arc::new(ApiKeyBearer::new(key)),
            http: ReqwestClient::new(),
            usage: None,
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
            bearer,
            http: ReqwestClient::new(),
            usage: None,
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
    bearer: Arc<S>,
    http: ReqwestClient,
    usage: Option<Arc<UsageReporter>>,
}

impl<S> Clone for ClaudeSubscriptionModel<S>
where
    S: BearerSource,
{
    fn clone(&self) -> Self {
        Self {
            model: self.model.clone(),
            bearer: self.bearer.clone(),
            http: self.http.clone(),
            usage: self.usage.clone(),
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

impl<S> CompletionModel for ClaudeSubscriptionModel<S>
where
    S: BearerSource + 'static,
{
    type Response = ClaudeStreamResponse;
    type StreamingResponse = ClaudeStreamResponse;
    type Client = ClaudeSubscriptionClient<S>;

    fn make(client: &Self::Client, model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            bearer: client.bearer.clone(),
            http: client.http.clone(),
            usage: client.usage.clone(),
        }
    }

    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        let surface = surface_of(&request);
        let stream = crate::claude_messages::stream_messages(
            &self.model,
            &request,
            surface,
            self.bearer.as_ref(),
            &self.http,
            self.usage.clone(),
        )
        .await?;
        futures::pin_mut!(stream);
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
        let surface = surface_of(&request);
        let stream = crate::claude_messages::stream_messages(
            &self.model,
            &request,
            surface,
            self.bearer.as_ref(),
            &self.http,
            self.usage.clone(),
        )
        .await?;
        Ok(StreamingCompletionResponse::stream(Box::pin(stream)))
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
