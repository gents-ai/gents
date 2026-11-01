//! Where one Claude Messages request is sent and how it authenticates: the
//! first-party API for the subscription and the API key, or the base URL, the
//! credential form and the header rules of a Claude platform backend (a
//! gateway, Foundry, Claude Platform on AWS, Mantle) as Claude Code 2.1.311
//! sends them there. The body, the header builder, the transport and the SSE
//! parser are the first-party owners; only this route differs. A route with
//! [`AwsSigning`] signs the request with AWS Signature Version 4 after the
//! headers are built; a [`BedrockRoute`] also rewrites the URL, the body and
//! the response for the Bedrock runtime API.

use std::sync::Arc;

use gents_claude::attribution;
use gents_claude::bedrock as bedrock_rules;
use gents_claude::betas::Provider;
use gents_claude::sigv4::Signer;
use gents_claude::vertex as vertex_rules;
use gents_loop::backend_provider::BackendProviderKind;
use gents_loop::claude_messages_body::ClaudeTarget;
use gents_loop::claude_platform::{messages_uri, ClaudePlatform, ClaudePlatformError};

/// Why a backend has no Messages route.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClaudeRouteError {
    /// The kind is not a Claude platform.
    #[error("{kind} is not a Claude platform")]
    NotAPlatform { kind: BackendProviderKind },
    /// The kind cannot be signed with AWS credentials.
    #[error("{kind} cannot use AWS credentials")]
    NotAnAwsPlatform { kind: BackendProviderKind },
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

/// AWS Signature Version 4 signing of a route's requests.
#[derive(Debug, Clone)]
pub(crate) struct AwsSigning {
    pub(crate) signer: Arc<Signer>,
    pub(crate) region: String,
    /// The signing service name of the platform.
    pub(crate) service: &'static str,
}

/// The Amazon Bedrock runtime API of a region: the model is in the URL path,
/// not the body, and the answer is an event stream.
#[derive(Debug, Clone)]
pub(crate) struct BedrockRoute {
    pub(crate) region: String,
    /// Where `ListInferenceProfiles` is asked: the control plane of the region,
    /// or the custom endpoint when the runtime has one.
    pub(crate) control_url: String,
    /// The backend whose profile list is cached; empty when unbound.
    pub(crate) backend_id: String,
}

/// The Google Vertex AI predict API of a project and region: the model is in
/// the URL path, not the body.
#[derive(Debug, Clone)]
pub(crate) struct VertexRoute {
    pub(crate) project: String,
    pub(crate) region: String,
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
    /// Sign requests with AWS Signature Version 4 instead of sending a key or token.
    pub(crate) signing: Option<AwsSigning>,
    /// Present for Amazon Bedrock; `uri` is then the runtime base URL.
    pub(crate) bedrock: Option<BedrockRoute>,
    /// Present for Google Vertex AI; `uri` is then the `/v1` base URL.
    pub(crate) vertex: Option<VertexRoute>,
    /// `x-goog-user-project`, the quota project of a Vertex route's Google credentials.
    pub(crate) quota_project: Option<String>,
    /// A signed-in gateway: its `cc_gateway` mark follows the current JWT, so the
    /// token is read before the request is planned.
    pub(crate) gateway_mark_from_token: bool,
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
            signing: None,
            bedrock: None,
            vertex: None,
            quota_project: None,
            gateway_mark_from_token: false,
        }
    }

    /// The URL the replay issuer is derived from: the Messages URL, or for Bedrock the
    /// invoke URL of a placeholder model (the issuer masks the model segment).
    pub(crate) fn issuer_uri(&self) -> Result<String, bedrock_rules::BedrockError> {
        let base = self.uri.strip_suffix('/').unwrap_or(&self.uri);
        if self.bedrock.is_some() {
            return Ok(format!("{base}{}", bedrock_rules::invoke_path("model")?));
        }
        Ok(match &self.vertex {
            Some(vertex) => format!(
                "{base}{}",
                vertex_rules::stream_path(&vertex.project, &vertex.region, "model")
            ),
            None => self.uri.clone(),
        })
    }

    /// The URL of a request for `model` along a Vertex route, or `None` for
    /// any other route.
    pub(crate) fn vertex_uri(&self, model: &str) -> Option<String> {
        self.vertex.as_ref().map(|vertex| {
            format!(
                "{}{}",
                self.uri.strip_suffix('/').unwrap_or(&self.uri),
                vertex_rules::stream_path(&vertex.project, &vertex.region, model)
            )
        })
    }

    /// The same gateway route signed in with a node OAuth account, whose mark is
    /// read from each request's token.
    #[must_use]
    pub(crate) fn with_signed_in_gateway(mut self) -> Self {
        self.gateway_mark_from_token =
            self.platform_kind == Some(BackendProviderKind::ClaudeGateway);
        self
    }

    /// The same route sending `quota_project` as `x-goog-user-project` (Vertex only).
    #[must_use]
    pub(crate) fn with_quota_project(mut self, quota_project: Option<&str>) -> Self {
        if self.vertex.is_some() {
            self.quota_project = quota_project.map(str::to_owned);
        }
        self
    }

    /// The same route with the backend whose Bedrock profile list it caches.
    #[must_use]
    pub(crate) fn with_backend_id(mut self, backend_id: &str) -> Self {
        if let Some(bedrock) = &mut self.bedrock {
            backend_id.clone_into(&mut bedrock.backend_id);
        }
        self
    }

    /// The route of a Claude platform backend whose static credential is
    /// `token`. `endpoint` is the platform's base URL. Platforms never send
    /// the hint headers (`z7n`); only Claude Platform on AWS sends
    /// `x-client-request-id`, and only on its default base URL. On Bedrock and
    /// Mantle a token is `AWS_BEARER_TOKEN_BEDROCK`'s bearer.
    pub(crate) fn platform(
        kind: BackendProviderKind,
        endpoint: &str,
        settings: Option<&ClaudePlatform>,
        token: &str,
    ) -> Result<Self, ClaudeRouteError> {
        Self::build(kind, endpoint, settings, token, None)
    }

    /// The route of an AWS platform backend (Bedrock, Mantle, Claude Platform
    /// on AWS) that signs each request with AWS Signature Version 4.
    pub(crate) fn platform_signed(
        kind: BackendProviderKind,
        endpoint: &str,
        settings: Option<&ClaudePlatform>,
        signer: Arc<Signer>,
    ) -> Result<Self, ClaudeRouteError> {
        Self::build(kind, endpoint, settings, "", Some(signer))
    }

    fn build(
        kind: BackendProviderKind,
        endpoint: &str,
        settings: Option<&ClaudePlatform>,
        token: &str,
        signer: Option<Arc<Signer>>,
    ) -> Result<Self, ClaudeRouteError> {
        ClaudePlatform::validate(kind, settings)?;
        let uri = messages_uri(endpoint);
        let region = || {
            settings
                .and_then(|s| s.region.clone())
                .ok_or(ClaudeRouteError::Settings(ClaudePlatformError::Missing {
                    kind,
                    field: "region",
                }))
        };
        let signing = |service| -> Result<Option<AwsSigning>, ClaudeRouteError> {
            signer
                .as_ref()
                .map(|signer| {
                    Ok(AwsSigning {
                        signer: signer.clone(),
                        region: region()?,
                        service,
                    })
                })
                .transpose()
        };
        let route = |provider, gateway_mark, credential| Self {
            platform_kind: Some(kind),
            uri: uri.clone(),
            target: ClaudeTarget::platform(provider, gateway_mark),
            credential,
            workspace_id: None,
            hint_headers: false,
            client_request_id_header: false,
            signing: None,
            bedrock: None,
            vertex: None,
            quota_project: None,
            gateway_mark_from_token: false,
        };
        if signer.is_some()
            && !matches!(
                kind,
                BackendProviderKind::ClaudeBedrock
                    | BackendProviderKind::ClaudeMantle
                    | BackendProviderKind::ClaudeAnthropicAws
            )
        {
            return Err(ClaudeRouteError::NotAnAwsPlatform { kind });
        }
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
                signing: signing("aws-external-anthropic")?,
                ..route(Provider::AnthropicAws, None, Credential::ApiKey)
            },
            BackendProviderKind::ClaudeMantle => Self {
                signing: signing("bedrock-mantle")?,
                ..route(Provider::Mantle, None, Credential::Bearer)
            },
            BackendProviderKind::ClaudeBedrock => {
                let region = region()?;
                let default = ClaudePlatform::is_default_endpoint(kind, settings, endpoint);
                let control_url = if default {
                    format!("https://bedrock.{region}.amazonaws.com")
                } else {
                    endpoint.to_owned()
                };
                Self {
                    uri: endpoint.to_owned(),
                    target: ClaudeTarget::platform(Provider::Bedrock, None)
                        .at_default_endpoint(bedrock_rules::is_own_runtime_endpoint(endpoint)),
                    signing: signing("bedrock")?,
                    bedrock: Some(BedrockRoute {
                        region,
                        control_url,
                        backend_id: String::new(),
                    }),
                    ..route(Provider::Bedrock, None, Credential::Bearer)
                }
            }
            BackendProviderKind::ClaudeVertex => {
                let project =
                    settings
                        .and_then(|s| s.project.clone())
                        .ok_or(ClaudeRouteError::Settings(ClaudePlatformError::Missing {
                            kind,
                            field: "project",
                        }))?;
                Self {
                    uri: endpoint.to_owned(),
                    target: ClaudeTarget::platform(Provider::Vertex, None).at_default_endpoint(
                        ClaudePlatform::is_default_endpoint(kind, settings, endpoint),
                    ),
                    vertex: Some(VertexRoute {
                        project,
                        region: settings
                            .and_then(|s| s.location.clone())
                            .unwrap_or_else(|| vertex_rules::DEFAULT_REGION.to_owned()),
                    }),
                    ..route(Provider::Vertex, None, Credential::Bearer)
                }
            }
            BackendProviderKind::ClaudeGoogleCloud => {
                route(Provider::AnthropicGoogleCloud, None, Credential::Bearer)
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
