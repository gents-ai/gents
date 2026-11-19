use std::future::Future;

use futures::stream::BoxStream;
use rig::client::CompletionClient;
use rig::completion::{
    AssistantContent, CompletionError, CompletionModel, CompletionRequest, CompletionResponse,
    GetTokenUsage, Usage,
};
use rig::streaming::{RawStreamingChoice, StreamingCompletionResponse};
use rig::OneOrMany;
use serde::{Deserialize, Serialize};

use crate::provider_call::{ForCall, ProviderCall};

/// What a [`Model`]'s response ends with: the usage it charges, and what the
/// loop reads from a final provider response by field name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamEnd {
    pub usage: Option<Usage>,
    #[serde(default)]
    pub token_usage: gents_protocol::usage::TokenUsage,
    /// The provider's response request id, under
    /// [`RESPONSE_REQUEST_ID_FIELD`](super::RESPONSE_REQUEST_ID_FIELD).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl GetTokenUsage for StreamEnd {
    fn token_usage(&self) -> Option<Usage> {
        self.usage
    }
}

pub type ModelStream = BoxStream<'static, Result<RawStreamingChoice<StreamEnd>, CompletionError>>;

/// A completion model this workspace implements itself rather than taking
/// from Rig. [`LiftedModel`] and [`LiftedClient`] are the one place it becomes
/// Rig's model and client.
pub trait Model: Clone + Send + Sync + 'static {
    type Client: Send + Sync;

    fn make(client: &Self::Client, model: String) -> Self;

    /// This model for one call: what it sends carries `call`.
    fn for_call(&self, call: &ProviderCall) -> Self;

    /// The whole response as assistant text with its end.
    fn completion(
        &self,
        request: CompletionRequest,
    ) -> impl Future<Output = Result<(String, StreamEnd), CompletionError>> + Send;

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> impl Future<Output = Result<ModelStream, CompletionError>> + Send;
}

/// A [`Model`] as Rig's completion model.
#[derive(Clone, Debug)]
pub struct LiftedModel<M>(pub M);

/// A [`Model`]'s client as Rig's completion client.
pub struct LiftedClient<M>(pub M::Client)
where
    M: Model;

impl<M> Clone for LiftedClient<M>
where
    M: Model,
    M::Client: Clone,
{
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<M> std::fmt::Debug for LiftedClient<M>
where
    M: Model,
    M::Client: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<M> CompletionClient for LiftedClient<M>
where
    M: Model,
{
    type CompletionModel = LiftedModel<M>;
}

impl<M> ForCall for LiftedModel<M>
where
    M: Model,
{
    fn for_call(&self, call: &ProviderCall) -> Self {
        Self(self.0.for_call(call))
    }
}

impl<M> CompletionModel for LiftedModel<M>
where
    M: Model,
{
    type Response = StreamEnd;
    type StreamingResponse = StreamEnd;
    type Client = LiftedClient<M>;

    fn make(client: &Self::Client, model: impl Into<String>) -> Self {
        Self(M::make(&client.0, model.into()))
    }

    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        let (text, end) = self.0.completion(request).await?;
        Ok(CompletionResponse {
            choice: OneOrMany::one(AssistantContent::text(text)),
            usage: end.usage.unwrap_or_else(Usage::new),
            raw_response: end,
            message_id: None,
        })
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        Ok(StreamingCompletionResponse::stream(
            self.0.stream(request).await?,
        ))
    }
}
