//! The Messages open path: plans the body, renders the headers, sends through
//! the capture layer and replays a 401 the way Claude Code's retry loop does.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use rapidhash::RapidHashSet;
use rig::completion::{CompletionError, CompletionRequest};
use rig::http_client::{self, HeaderValue, HttpClientExt, Request};
use rig::streaming::RawStreamingChoice;

use super::error::{body_prefix, non_success_error, read_body_prefix};
use super::sse::{stream_sse_body_observed, MessagesSseState};
use super::transport::{capturing, ClaudeHttpClient};
use super::MESSAGES_URI;
use crate::claude_subscription::ClaudeStreamResponse;
use crate::oauth_credential::BearerSource;

use gents_claude::headers::{self, AgentHeaders, Auth, HeaderContext};
use gents_claude::profile;
use gents_claude::retry::{
    backoff_ms, BASE_DELAY_MS, DEFAULT_MAX_RETRIES, MAX_DELAY_MS, OAUTH_REFRESH_ATTEMPTS,
};
use gents_claude::wire;
use gents_loop::claude_messages_body::{
    plan_claude_request, ClaudeAccountContext, ClaudeRequestPlan,
};

pub async fn stream_messages<S>(
    model: &str,
    request: &CompletionRequest,
    account: &ClaudeAccountContext,
    surface: RapidHashSet<String>,
    bearer: &S,
    usage: Option<Arc<crate::usage_observation::UsageReporter>>,
) -> Result<
    impl futures::Stream<Item = Result<RawStreamingChoice<ClaudeStreamResponse>, CompletionError>>,
    CompletionError,
>
where
    S: BearerSource,
{
    stream_messages_at(
        MESSAGES_URI,
        model,
        request,
        account,
        surface,
        bearer,
        usage,
    )
    .await
}

/// A failed open of a Messages stream; `rejected` is set when the provider answered with a non-success status.
pub(crate) struct MessagesOpenError {
    pub(crate) error: CompletionError,
    pub(crate) rejected: bool,
}

impl From<CompletionError> for MessagesOpenError {
    fn from(error: CompletionError) -> Self {
        Self {
            error,
            rejected: false,
        }
    }
}

pub(crate) async fn stream_messages_at<S>(
    uri: &str,
    model: &str,
    request: &CompletionRequest,
    account: &ClaudeAccountContext,
    surface: RapidHashSet<String>,
    bearer: &S,
    usage: Option<Arc<crate::usage_observation::UsageReporter>>,
) -> Result<
    impl futures::Stream<Item = Result<RawStreamingChoice<ClaudeStreamResponse>, CompletionError>>,
    CompletionError,
>
where
    S: BearerSource,
{
    open_messages_at(uri, model, request, account, surface, bearer, usage)
        .await
        .map_err(|failure| failure.error)
}

fn invalid_input(message: String) -> CompletionError {
    CompletionError::RequestError(Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        message,
    )))
}

/// The request for one send: the planned headers rendered for `token` with a fresh
/// `x-client-request-id`, over the same body bytes every send.
fn render_request(
    uri: &str,
    plan: &ClaudeRequestPlan,
    account: &ClaudeAccountContext,
    token: &str,
    body: &Bytes,
) -> Result<Request<Bytes>, CompletionError> {
    let (platform, arch) = headers::host_platform();
    let (os, arch) = (headers::platform_os(platform), headers::platform_arch(arch));
    let client_request_id = uuid::Uuid::new_v4().to_string();
    let header_context = HeaderContext {
        entrypoint: plan.mode.entrypoint,
        agent_sdk_version: None,
        client_app: None,
        workload: plan.mode.workload,
        background: plan.mode.background,
        session_id: &plan.session_id,
        os: &os,
        arch: &arch,
        auth: if account.api_key {
            Auth::ApiKey(token)
        } else {
            Auth::Bearer(token)
        },
        first_party_base_url: true,
        agent: plan.agent.as_ref().map(|agent| AgentHeaders {
            agent_id: &agent.agent_id,
            parent_agent_id: agent.parent_agent_id.as_deref(),
        }),
        // Every Gents agent is operator-defined, which Claude Code types as a custom agent.
        agent_type: plan.agent.as_ref().map(|_| headers::CUSTOM_AGENT_TYPE),
        request_class: plan.mode.request_class,
        prompt_id: Some(&plan.prompt_id),
        betas: &plan.betas,
        client_request_id: &client_request_id,
        retry_count: 0,
        timeout_ms: profile::REQUEST_TIMEOUT_MS,
    };
    let mut builder = Request::builder().method("POST").uri(uri);
    for (name, header) in headers::request_headers(&header_context) {
        let mut value = HeaderValue::from_str(&header.value).map_err(|error| {
            CompletionError::ProviderError(format!("Claude Messages header {name}: {error}"))
        })?;
        value.set_sensitive(header.sensitive);
        builder = builder.header(name, value);
    }
    builder.body(body.clone()).map_err(|error| {
        CompletionError::ProviderError(format!("Claude Messages request: {error}"))
    })
}

/// Claude Code's 401 replay (`pve`): the OAuth refresh is forced, and a refresh that returns
/// the rejected access token [`OAUTH_REFRESH_ATTEMPTS`] times in a row ends the request. One
/// request never exceeds [`DEFAULT_MAX_RETRIES`] + 1 sends.
#[derive(Debug, Default)]
pub(super) struct Replay {
    pub(super) sends: u32,
    unchanged: u32,
}

impl Replay {
    pub(super) fn sent(&mut self) {
        self.sends += 1;
    }

    pub(super) fn may_resend(&self) -> bool {
        self.sends <= DEFAULT_MAX_RETRIES
    }

    /// The wait before the resend, or `None` when the refresh returned the rejected token too often.
    pub(super) fn refreshed(&mut self, changed: bool, random: f64) -> Option<Duration> {
        if changed {
            self.unchanged = 0;
        } else {
            self.unchanged += 1;
            if self.unchanged >= OAUTH_REFRESH_ATTEMPTS {
                return None;
            }
        }
        Some(Duration::from_millis(backoff_ms(
            self.sends,
            None,
            MAX_DELAY_MS,
            BASE_DELAY_MS,
            random,
        )))
    }
}

/// Sends the planned request as Claude Code 2.1.311 would: the typed body bytes, the header
/// set in Claude Code's insertion order, and the [`ClaudeHttpClient`] that writes them with
/// Bun's casing, order, defaults and gzip.
pub(crate) async fn open_messages_at<S>(
    uri: &str,
    model: &str,
    request: &CompletionRequest,
    account: &ClaudeAccountContext,
    surface: RapidHashSet<String>,
    bearer: &S,
    usage: Option<Arc<crate::usage_observation::UsageReporter>>,
) -> Result<
    impl futures::Stream<Item = Result<RawStreamingChoice<ClaudeStreamResponse>, CompletionError>>,
    MessagesOpenError,
>
where
    S: BearerSource,
{
    let plan = plan_claude_request(model, request, account)
        .map_err(|error| invalid_input(format!("Claude Messages body: {error:#}")))?;
    let body = Bytes::from(
        wire::to_bytes(&plan.body)
            .map_err(|error| invalid_input(format!("encode Claude Messages body: {error}")))?,
    );
    // Only a request that would be sent consumes a fixture: a test whose body
    // fails to build must not take a body queued by a concurrent test.
    #[cfg(test)]
    let fixture = super::take_messages_sse_fixture();
    #[cfg(not(test))]
    let fixture: Option<String> = None;

    let bearer_error = |error: anyhow::Error| {
        CompletionError::ProviderError(format!("Claude Messages bearer: {error}"))
    };
    let mut token = if fixture.is_none() {
        bearer.current_bearer().await.map_err(bearer_error)?
    } else {
        String::new()
    };
    if fixture.is_none() {
        tracing::info!(
            model = %model,
            oauth = account.subscriber,
            "live Claude Messages HTTP send"
        );
    }
    let replayable = !account.api_key && bearer.is_oauth();
    let mut replay = Replay::default();
    let response = loop {
        let http_request = render_request(uri, &plan, account, &token, &body)?;
        let client = capturing(fixture.clone(), ClaudeHttpClient::new(usage.clone()));
        replay.sent();
        let failure = match client.send_streaming(http_request).await {
            Ok(response) => break response,
            Err(http_client::Error::InvalidStatusCodeWithMessage(status, message)) => {
                let (text, marker) = gents_loop::provider_limit::split_annotated_body(&message);
                let failure = MessagesOpenError {
                    error: non_success_error(
                        status,
                        None,
                        &format!("{}{marker}", body_prefix(text.as_bytes())),
                    ),
                    rejected: true,
                };
                if status.as_u16() != 401 {
                    return Err(failure);
                }
                failure
            }
            Err(error) => return Err(CompletionError::from(error).into()),
        };
        bearer.invalidate().await;
        if !replayable || !replay.may_resend() {
            return Err(failure);
        }
        let fresh = bearer.current_bearer().await.map_err(bearer_error)?;
        let Some(wait) = replay.refreshed(fresh != token, rand::random()) else {
            return Err(failure);
        };
        tokio::time::sleep(wait).await;
        token = fresh;
    };
    let status = response.status();
    let request_id = response
        .headers()
        .get("request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if !status.is_success() {
        let prefix = read_body_prefix(response.into_body()).await;
        return Err(MessagesOpenError {
            error: non_success_error(status, request_id.as_deref(), &prefix),
            rejected: true,
        });
    }
    let audit_sender = gents_loop::rendered_request::scope::current_audit_sender();
    if gents_loop::rendered_request::scope::current_scope().is_some() && audit_sender.is_none() {
        return Err(CompletionError::ProviderError(
            "Claude Messages response has no exact provider audit attempt".into(),
        )
        .into());
    }
    Ok(stream_sse_body_observed(
        response.into_body(),
        MessagesSseState::new(surface).with_request_id(request_id),
        audit_sender,
    ))
}
