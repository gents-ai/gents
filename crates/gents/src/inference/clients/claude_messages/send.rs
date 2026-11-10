//! The Messages open path: plans the body, renders the headers, sends through
//! the capture layer and replays a 401 the way Claude Code's retry loop does.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use rapidhash::RapidHashSet;
use rig::completion::{CompletionError, CompletionRequest};
use rig::http_client::{self, HeaderValue, HttpClientExt, Request};
use rig::streaming::RawStreamingChoice;

use super::aws::sign_request;
use super::bedrock;
use super::error::{body_prefix, non_success_error, read_body_prefix};
use super::route::{ClaudeRoute, Credential};
use super::sse::{self, stream_sse_body_observed, MessagesSseState};
use super::transport::{capturing, url_parts, ClaudeHttpClient};
use super::MESSAGES_URI;
use crate::inference::auth::credential::BearerSource;
use crate::inference::clients::claude_subscription::ClaudeStreamResponse;

use gents_claude::bedrock as bedrock_rules;
use gents_claude::headers::{self, AgentHeaders, Auth, HeaderContext};
use gents_claude::profile;
use gents_claude::retry::{
    backoff_ms, BASE_DELAY_MS, DEFAULT_MAX_RETRIES, MAX_DELAY_MS, OAUTH_REFRESH_ATTEMPTS,
};
use gents_claude::vertex as vertex_rules;
use gents_claude::wire;
use gents_loop::claude_messages_body::{
    plan_claude_request_for, ClaudeAccountContext, ClaudeRequestPlan,
};

pub async fn stream_messages<S>(
    model: &str,
    request: &CompletionRequest,
    account: &ClaudeAccountContext,
    surface: RapidHashSet<String>,
    bearer: &S,
    usage: Option<Arc<crate::inference::usage_observation::UsageReporter>>,
) -> Result<
    impl futures::Stream<Item = Result<RawStreamingChoice<ClaudeStreamResponse>, CompletionError>>,
    CompletionError,
>
where
    S: BearerSource,
{
    stream_messages_routed(
        &ClaudeRoute::first_party(MESSAGES_URI),
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

#[cfg(test)]
pub(crate) async fn stream_messages_at<S>(
    uri: &str,
    model: &str,
    request: &CompletionRequest,
    account: &ClaudeAccountContext,
    surface: RapidHashSet<String>,
    bearer: &S,
    usage: Option<Arc<crate::inference::usage_observation::UsageReporter>>,
) -> Result<
    impl futures::Stream<Item = Result<RawStreamingChoice<ClaudeStreamResponse>, CompletionError>>,
    CompletionError,
>
where
    S: BearerSource,
{
    stream_messages_routed(
        &ClaudeRoute::first_party(uri),
        model,
        request,
        account,
        surface,
        bearer,
        usage,
    )
    .await
}

pub(crate) async fn stream_messages_routed<S>(
    route: &ClaudeRoute,
    model: &str,
    request: &CompletionRequest,
    account: &ClaudeAccountContext,
    surface: RapidHashSet<String>,
    bearer: &S,
    usage: Option<Arc<crate::inference::usage_observation::UsageReporter>>,
) -> Result<
    impl futures::Stream<Item = Result<RawStreamingChoice<ClaudeStreamResponse>, CompletionError>>,
    CompletionError,
>
where
    S: BearerSource,
{
    open_messages_at(route, model, request, account, surface, bearer, usage)
        .await
        .map_err(|failure| failure.error)
}

fn invalid_input(message: String) -> CompletionError {
    CompletionError::RequestError(Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        message,
    )))
}

/// The request for one send to `uri`: the planned headers rendered for `token`
/// with a fresh `x-client-request-id`, over the same body bytes every send. A
/// signing route signs everything but `x-client-request-id`, which Claude Code's
/// fetch wrapper adds after the SDK signed.
fn render_request(
    route: &ClaudeRoute,
    plan: &ClaudeRequestPlan,
    account: &ClaudeAccountContext,
    token: &str,
    uri: &str,
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
        auth: match route.credential {
            _ if route.signing.is_some() => Auth::None,
            Credential::Account if account.api_key => Auth::ApiKey(token),
            Credential::Account | Credential::Bearer => Auth::Bearer(token),
            Credential::ApiKey => Auth::ApiKey(token),
        },
        hint_headers: route.hint_headers,
        client_request_id_header: route.client_request_id_header,
        workspace_id: route.workspace_id.as_deref(),
        agent: plan.agent.as_ref().map(|agent| AgentHeaders {
            agent_did: &agent.agent_did,
            parent_agent_did: agent.parent_agent_did.as_deref(),
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
    let mut rendered = headers::request_headers(&header_context);
    // Claude Code's fetch wrapper adds this after the SDK middleware signed.
    let unsigned = rendered
        .iter()
        .position(|(name, _)| name.eq_ignore_ascii_case("x-client-request-id"))
        .map(|at| rendered.remove(at));
    if let Some(signing) = &route.signing {
        let target = url_parts(uri).map_err(|error| {
            CompletionError::ProviderError(format!("Claude Messages request URL: {error}"))
        })?;
        let plain: Vec<(&str, &str)> = rendered
            .iter()
            .map(|(name, header)| (*name, header.value.as_str()))
            .collect();
        let added = sign_request(signing, "POST", &target, &plain, body).map_err(|error| {
            CompletionError::ProviderError(format!("Claude Messages request signing: {error}"))
        })?;
        rendered.extend(added);
    }
    if let Some(project) = &route.quota_project {
        rendered.push((
            "x-goog-user-project",
            headers::HeaderValue {
                value: project.clone(),
                sensitive: false,
            },
        ));
    }
    rendered.extend(unsigned);
    let mut builder = Request::builder().method("POST").uri(uri);
    for (name, header) in rendered {
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
    route: &ClaudeRoute,
    model: &str,
    request: &CompletionRequest,
    account: &ClaudeAccountContext,
    surface: RapidHashSet<String>,
    bearer: &S,
    usage: Option<Arc<crate::inference::usage_observation::UsageReporter>>,
) -> Result<
    impl futures::Stream<Item = Result<RawStreamingChoice<ClaudeStreamResponse>, CompletionError>>,
    MessagesOpenError,
>
where
    S: BearerSource,
{
    // Boxed: the open path (planning, signing, profile listing, the transport and its
    // replays) is a large state machine that would otherwise sit inline in every
    // caller's future, the agent loop's included.
    Box::pin(open_messages(
        route, model, request, account, surface, bearer, usage,
    ))
    .await
}

async fn open_messages<S>(
    route: &ClaudeRoute,
    model: &str,
    request: &CompletionRequest,
    account: &ClaudeAccountContext,
    surface: RapidHashSet<String>,
    bearer: &S,
    usage: Option<Arc<crate::inference::usage_observation::UsageReporter>>,
) -> Result<
    impl futures::Stream<Item = Result<RawStreamingChoice<ClaudeStreamResponse>, CompletionError>>,
    MessagesOpenError,
>
where
    S: BearerSource,
{
    let bearer_error = |error: anyhow::Error| {
        CompletionError::ProviderError(format!("Claude Messages bearer: {error}"))
    };
    // A signed-in gateway's attribution mark comes from the token it sends (`J6o`).
    let early_token = if route.gateway_mark_from_token {
        Some(bearer.current_bearer().await.map_err(bearer_error)?)
    } else {
        None
    };
    let mut target = route.target;
    if let Some(token) = &early_token {
        target.gateway_mark = Some(gents_claude::attribution::gateway_mark(token));
    }
    let plan = plan_claude_request_for(model, request, account, target)
        .map_err(|error| invalid_input(format!("Claude Messages body: {error:#}")))?;
    let planned = wire::to_bytes(&plan.body)
        .map_err(|error| invalid_input(format!("encode Claude Messages body: {error}")))?;
    let encode = |error: &dyn std::fmt::Display| {
        invalid_input(format!("encode Claude Messages body: {error}"))
    };
    let body = Bytes::from(if route.bedrock.is_some() {
        bedrock_rules::transform_body(&planned, &plan.body_betas, &plan.betas)
            .map_err(|error| encode(&error))?
    } else if route.vertex.is_some() {
        vertex_rules::transform_body(&planned).map_err(|error| encode(&error))?
    } else {
        planned
    });
    // Only a request that would be sent consumes a fixture: a test whose body
    // fails to build must not take a body queued by a concurrent test.
    #[cfg(test)]
    let fixture = super::take_messages_sse_fixture();
    #[cfg(not(test))]
    let fixture: Option<String> = None;

    let mut token = if let Some(token) = early_token {
        token
    } else if fixture.is_none() {
        bearer.current_bearer().await.map_err(bearer_error)?
    } else {
        String::new()
    };
    let uri = match &route.bedrock {
        Some(runtime) => {
            // Boxed: the listing holds a second transport future, which inlined
            // here would double the send state machine.
            let id = Box::pin(bedrock::resolve_model(
                runtime,
                route.signing.as_ref(),
                &plan,
                &token,
                fixture.is_none(),
            ))
            .await;
            bedrock::invoke_uri(&route.uri, &id)
                .map_err(|error| CompletionError::ProviderError(error.to_string()))?
        }
        None => route
            .vertex_uri(&plan.body.model)
            .unwrap_or_else(|| route.uri.clone()),
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
        let http_request = render_request(route, &plan, account, &token, &uri, &body)?;
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
    let response = if route.bedrock.is_some() && status.is_success() {
        sse::bedrock_response(response)?
    } else {
        response
    };
    let request_id = response
        .headers()
        .get("request-id")
        .or_else(|| response.headers().get("x-amzn-requestid"))
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
