use std::fmt;
use std::sync::Arc;

use rig::client::CompletionClient;
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};
use rig::streaming::StreamingCompletionResponse;

use super::http::Result;
use crate::provider_call::{ForCall, ProviderCall};

type Build<C> = Arc<dyn Fn(&ProviderCall) -> Result<C> + Send + Sync>;

/// A Rig provider client kept as what builds it. Rig's client cannot change
/// its HTTP client once built, so each call gets a client built over a
/// transport that holds that call; the client built for no call names the
/// provider's routes.
pub struct CallClient<C> {
    build: Build<C>,
    client: C,
}

impl<C> CallClient<C> {
    /// `build` is fallible only in what does not depend on the call, so a
    /// build that succeeds for no call succeeds for every call.
    pub fn new<F>(build: F) -> Result<Self>
    where
        F: Fn(&ProviderCall) -> Result<C> + Send + Sync + 'static,
    {
        let client = build(&ProviderCall::default())?;
        Ok(Self {
            build: Arc::new(build),
            client,
        })
    }

    /// The client built for no call.
    pub fn client(&self) -> &C {
        &self.client
    }

    fn for_call(&self, call: &ProviderCall) -> C {
        (self.build)(call).expect("a provider client that built for no call builds for every call")
    }
}

impl<C> Clone for CallClient<C>
where
    C: Clone,
{
    fn clone(&self) -> Self {
        Self {
            build: Arc::clone(&self.build),
            client: self.client.clone(),
        }
    }
}

impl<C> fmt::Debug for CallClient<C>
where
    C: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.client.fmt(f)
    }
}

impl<C> CompletionClient for CallClient<C>
where
    C: CompletionClient + Clone + Send + Sync + 'static,
{
    type CompletionModel = CallModel<C>;
}

/// A Rig provider model whose client is rebuilt for each call.
pub struct CallModel<C>
where
    C: CompletionClient,
{
    client: CallClient<C>,
    name: String,
    model: C::CompletionModel,
}

impl<C> Clone for CallModel<C>
where
    C: CompletionClient + Clone,
{
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            name: self.name.clone(),
            model: self.model.clone(),
        }
    }
}

impl<C> ForCall for CallModel<C>
where
    C: CompletionClient + Clone + Send + Sync + 'static,
{
    fn for_call(&self, call: &ProviderCall) -> Self {
        Self {
            client: self.client.clone(),
            name: self.name.clone(),
            model: self
                .client
                .for_call(call)
                .completion_model(self.name.clone()),
        }
    }
}

impl<C> CompletionModel for CallModel<C>
where
    C: CompletionClient + Clone + Send + Sync + 'static,
{
    type Response = <C::CompletionModel as CompletionModel>::Response;
    type StreamingResponse = <C::CompletionModel as CompletionModel>::StreamingResponse;
    type Client = CallClient<C>;

    fn make(client: &Self::Client, model: impl Into<String>) -> Self {
        let name = model.into();
        Self {
            model: client.client.completion_model(name.clone()),
            client: client.clone(),
            name,
        }
    }

    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> std::result::Result<CompletionResponse<Self::Response>, CompletionError> {
        self.model.completion(request).await
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> std::result::Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError>
    {
        self.model.stream(request).await
    }
}
