//! Where one Claude Messages request is sent and how it authenticates: the
//! first-party API for the subscription and the API key, or the base URL, the
//! credential form and the header rules of a Claude platform backend (a
//! gateway, Foundry, Claude Platform on AWS, Mantle) as Claude Code 2.1.311
//! sends them there. The body, the header builder, the transport and the SSE
//! parser are the first-party owners; only this route differs.

use gents_claude::attribution;
use gents_claude::betas::Provider;
use gents_loop::backend_provider::BackendProviderKind;
use gents_loop::claude_messages_body::ClaudeTarget;
use gents_loop::claude_platform::{messages_uri, ClaudePlatform};

/// Why a backend has no Messages route.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClaudeRouteError {
    /// The platform's request signing or token exchange is not part of this build.
    #[error("{provider} backends are not available yet: {reason}")]
    Unavailable {
        provider: &'static str,
        reason: &'static str,
    },
    /// The kind is not a Claude platform.
    #[error("{kind} is not a Claude platform")]
    NotAPlatform { kind: BackendProviderKind },
    /// The backend has no usable settings for the platform.
    #[error(transparent)]
    Settings(#[from] gents_loop::claude_platform::ClaudePlatformError),
}

/// How the request's credential is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Credential {
    /// The account decides: a subscription token is a bearer, an API key is `X-Api-Key`.
    Account,
    /// `Authorization: Bearer`.
    Bearer,
    /// `X-Api-Key`.
    ApiKey,
}

/// The destination and header rules of one backend's requests.
#[derive(Debug, Clone)]
pub(crate) struct ClaudeRoute {
    /// The platform provider kind; `None` for the first-party API.
    pub(crate) platform_kind: Option<BackendProviderKind>,
    pub(crate) uri: String,
    pub(crate) target: ClaudeTarget,
    pub(crate) credential: Credential,
    pub(crate) workspace_id: Option<String>,
    pub(crate) hint_headers: bool,
    pub(crate) client_request_id_header: bool,
}

impl ClaudeRoute {
    /// The first-party API at `uri`.
    pub(crate) fn first_party(uri: &str) -> Self {
        Self {
            platform_kind: None,
            uri: uri.to_owned(),
            target: ClaudeTarget::FIRST_PARTY,
            credential: Credential::Account,
            workspace_id: None,
            hint_headers: true,
            client_request_id_header: true,
        }
    }

    /// The route of a Claude platform backend whose static credential is
    /// `token`. `endpoint` is the platform's base URL. Platforms never send
    /// the hint headers (`z7n`); only Claude Platform on AWS sends
    /// `x-client-request-id`, and only on its default base URL.
    pub(crate) fn platform(
        kind: BackendProviderKind,
        endpoint: &str,
        settings: Option<&ClaudePlatform>,
        token: &str,
    ) -> Result<Self, ClaudeRouteError> {
        ClaudePlatform::validate(kind, settings)?;
        let uri = messages_uri(endpoint);
        let route = |provider, gateway_mark, credential| Self {
            platform_kind: Some(kind),
            uri: uri.clone(),
            target: ClaudeTarget::platform(provider, gateway_mark),
            credential,
            workspace_id: None,
            hint_headers: false,
            client_request_id_header: false,
        };
        Ok(match kind {
            BackendProviderKind::ClaudeGateway => route(
                Provider::Gateway,
                Some(attribution::gateway_mark(token)),
                Credential::Bearer,
            ),
            BackendProviderKind::ClaudeFoundry => route(
                Provider::Foundry,
                None,
                if settings.is_some_and(|s| s.bearer_token == Some(true)) {
                    Credential::Bearer
                } else {
                    Credential::ApiKey
                },
            ),
            BackendProviderKind::ClaudeAnthropicAws => Self {
                workspace_id: settings.and_then(|s| s.workspace_id.clone()),
                client_request_id_header: ClaudePlatform::is_default_endpoint(
                    kind, settings, endpoint,
                ),
                ..route(Provider::AnthropicAws, None, Credential::ApiKey)
            },
            BackendProviderKind::ClaudeMantle => route(Provider::Mantle, None, Credential::Bearer),
            BackendProviderKind::ClaudeBedrock => {
                return Err(ClaudeRouteError::Unavailable {
                    provider: "Amazon Bedrock",
                    reason: "its request signing and event-stream decoding are not built",
                })
            }
            BackendProviderKind::ClaudeVertex => {
                return Err(ClaudeRouteError::Unavailable {
                    provider: "Google Vertex AI",
                    reason: "its Google credentials and request rewrite are not built",
                })
            }
            BackendProviderKind::ClaudeGoogleCloud => {
                return Err(ClaudeRouteError::Unavailable {
                    provider: "Claude Platform on Google Cloud",
                    reason: "its Google credentials are not built",
                })
            }
            BackendProviderKind::OpenAiCompatible
            | BackendProviderKind::OpenRouter
            | BackendProviderKind::ChatGptCodex
            | BackendProviderKind::XaiGrokOAuth
            | BackendProviderKind::ClaudeCliSubscription
            | BackendProviderKind::AnthropicApiKey => {
                return Err(ClaudeRouteError::NotAPlatform { kind })
            }
        })
    }
}
