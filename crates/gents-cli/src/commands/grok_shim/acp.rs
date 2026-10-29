use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use futures_util::future::BoxFuture;
use gents::defra_node::EmbeddedNode;
use serde_json::{json, Value};

use super::projection::caused_sessions::{
    SUBAGENT_CANCEL_METHOD, SUBAGENT_GET_METHOD, SUBAGENT_LIST_RUNNING_METHOD,
};
use super::projection::{effective_context_window_tokens, ProjectionEngine, SESSION_UPDATE_METHOD};
use super::protocol::{ClientCapabilities, RegisterMode};
use super::server::{AcpDelegate, AcpOutbound};
use super::turn::sender::PromptSender;
use super::turn::wire::parse_cancel_notification;
use super::turn::TurnManager;

mod errors;
mod lifecycle;
mod prompt;
mod session_document;
mod session_state;
mod subagents;
pub(in crate::commands::grok_shim) mod wire;

use errors::{
    error_code_for, invalid_params, required_session_id, shaped_stub_error, ShapedMethodNotFound,
};
use session_state::AcpSessionState;
use wire::{
    error_line, internal_method, notification_line, response_line, AcpDispatch, AcpRequest,
    RequestOutcome,
};

pub(crate) const JSONRPC_INVALID_REQUEST: i64 = -32600;

pub(crate) const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;

pub(crate) const JSONRPC_INVALID_PARAMS: i64 = -32602;

pub(crate) const JSONRPC_INTERNAL_ERROR: i64 = -32603;

pub(crate) const GENTS_AUTH_METHOD_ID: &str = "gents.runtime";

pub(crate) const ACP_PROTOCOL_VERSION: i64 = 1;

pub(crate) const INITIALIZE_METHOD: &str = "initialize";

pub(crate) const AUTHENTICATE_METHOD: &str = "authenticate";

pub(crate) const SESSION_NEW_METHOD: &str = "session/new";

pub(crate) const SESSION_LOAD_METHOD: &str = "session/load";

pub(crate) const SESSION_SET_MODEL_METHOD: &str = "session/set_model";

pub(crate) const SESSION_SET_MODE_METHOD: &str = "session/set_mode";

pub(crate) const SESSION_PROMPT_METHOD: &str = "session/prompt";

pub(crate) const SESSION_CANCEL_METHOD: &str = "session/cancel";

pub(crate) const INTERJECT_METHOD: &str = "x.ai/interject";

pub(crate) const COMPACT_CONVERSATION_METHOD: &str = "x.ai/compact_conversation";

pub(crate) const MODELS_UPDATE_METHOD: &str = "x.ai/models/update";

pub(crate) const MCP_INITIALIZED_METHOD: &str = "x.ai/mcp_initialized";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundModel {
    pub(crate) model_id: String,
    pub(crate) name: String,
    pub(crate) total_context_tokens: u64,
}

impl BoundModel {
    pub(crate) fn effective_context_window(&self) -> u64 {
        effective_context_window_tokens(self.total_context_tokens)
    }

    fn catalog_entry(&self) -> Value {
        json!({
            "modelId": self.model_id,
            "name": self.name,
            "meta": {
                "totalContextTokens": self.effective_context_window(),
                "acceptsImages": false,
                "inputModalities": ["text"],
                "supportsReasoningEffort": false,
            },
        })
    }
}

impl BoundModel {
    pub(crate) fn models_object(&self) -> Value {
        json!({
            "availableModels": [self.catalog_entry()],
            "currentModelId": self.model_id,
        })
    }
}

#[derive(Clone)]
pub(crate) struct AcpServiceConfig {
    pub(crate) node: Arc<EmbeddedNode>,
    pub(crate) node_did: Arc<str>,
    pub(crate) agent_id: Arc<str>,
    pub(crate) current_model: BoundModel,
    pub(crate) grok_home: Option<std::path::PathBuf>,
    /// Gents home the plugin catalogs of `x.ai/workflows/list` are read under.
    pub(crate) plugin_home: std::path::PathBuf,
}

impl AcpServiceConfig {
    pub(crate) fn models_object(&self) -> Value {
        self.current_model.models_object()
    }
}

pub(crate) struct AcpService {
    config: AcpServiceConfig,
    turns: Arc<TurnManager>,
    projections: Arc<ProjectionEngine>,
    sessions: tokio::sync::Mutex<BTreeMap<String, AcpSessionState>>,
    loading_sessions: std::sync::Mutex<BTreeSet<String>>,
    client_id: Option<u64>,
    registered_mode: Option<RegisterMode>,
    registered_capabilities: Option<ClientCapabilities>,
}

impl AcpService {
    pub(super) fn new(
        config: AcpServiceConfig,
        turns: Arc<TurnManager>,
        projections: Arc<ProjectionEngine>,
    ) -> Self {
        Self {
            config,
            turns,
            projections,
            sessions: tokio::sync::Mutex::new(BTreeMap::new()),
            loading_sessions: std::sync::Mutex::new(BTreeSet::new()),
            client_id: None,
            registered_mode: None,
            registered_capabilities: None,
        }
    }

    pub(super) fn register_client_identity(
        &mut self,
        client_id: u64,
        registration: &crate::commands::grok_shim::server::Registration,
    ) {
        self.client_id = Some(client_id);
        self.registered_mode = Some(registration.mode);
        self.registered_capabilities = Some(registration.capabilities.clone());
    }

    pub(super) async fn dispatch_with_sender(
        &self,
        payload: &str,
        prompt_sender: &PromptSender,
    ) -> AcpDispatch {
        let request = match AcpRequest::from_payload(payload) {
            Ok(request) => request,
            Err(error) => {
                tracing::warn!(%error, "grok shim rejected an undecodable ACP payload");
                return AcpDispatch {
                    notifications: Vec::new(),
                    response: Some(error_line(
                        &Value::Null,
                        JSONRPC_INVALID_REQUEST,
                        &format!("invalid ACP payload: {error}"),
                    )),
                };
            }
        };

        if request.is_notification() {
            tracing::debug!(method = %request.method, "grok shim ACP notification");
            return self.dispatch_notification(request).await;
        }
        tracing::debug!(method = %request.method, "grok shim ACP request");
        self.dispatch_request(request, prompt_sender).await
    }

    async fn dispatch_notification(&self, request: AcpRequest) -> AcpDispatch {
        match request.method.as_str() {
            SESSION_CANCEL_METHOD => {
                let cancel = match parse_cancel_notification(&request.params) {
                    Ok(cancel) => cancel,
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            method = %request.method,
                            "grok shim ignored a malformed session/cancel notification"
                        );
                        return AcpDispatch::default();
                    }
                };
                if let Err(error) = self.turns.handle_cancel(cancel).await {
                    tracing::warn!(
                        %error,
                        method = %request.method,
                        "grok shim failed to dispatch ACP notification"
                    );
                }
            }
            other => {
                tracing::warn!(%other, "grok shim ignored an unknown ACP notification");
            }
        }
        AcpDispatch::default()
    }

    async fn dispatch_request(
        &self,
        request: AcpRequest,
        prompt_sender: &PromptSender,
    ) -> AcpDispatch {
        let id = request.id.clone().unwrap_or(Value::Null);
        let method = request.method.clone();
        match self.handle_request(&request, prompt_sender).await {
            Ok(outcome) => {
                let mut notifications = outcome
                    .notifications
                    .iter()
                    .map(|params| {
                        let notification_method = params
                            .get("__method")
                            .and_then(Value::as_str)
                            .unwrap_or(SESSION_UPDATE_METHOD);
                        let mut params = params.clone();
                        if let Some(object) = params.as_object_mut() {
                            object.remove("__method");
                        }
                        notification_line(notification_method, &params)
                    })
                    .collect::<Vec<_>>();
                notifications.extend(prompt_sender.take_lines().await);
                AcpDispatch {
                    notifications,
                    response: Some(response_line(&id, &outcome.result)),
                }
            }
            Err(error) => {
                let code = error_code_for(&error);
                tracing::warn!(%error, %method, code, "grok shim ACP request failed");
                AcpDispatch {
                    notifications: prompt_sender.take_lines().await,
                    response: Some(error_line(&id, code, &error.to_string())),
                }
            }
        }
    }

    async fn handle_request(
        &self,
        request: &AcpRequest,
        prompt_sender: &PromptSender,
    ) -> Result<RequestOutcome> {
        match request.method.as_str() {
            INITIALIZE_METHOD => self.handle_initialize(request).await,
            AUTHENTICATE_METHOD => self.handle_authenticate(request).await,
            "x.ai/session/info" => {
                let session = required_session_id(&request.params)?;
                let result = super::usage::session_info(
                    &self.config.node,
                    &self.config.node_did,
                    &self.config.agent_id,
                    &session,
                    &self.config.current_model.model_id,
                    &self.config.current_model.name,
                    self.config.current_model.effective_context_window(),
                )
                .await?;
                Ok(RequestOutcome {
                    notifications: Vec::new(),
                    result,
                })
            }
            "x.ai/billing" => Ok(RequestOutcome {
                notifications: Vec::new(),
                result: json!({"result":{"config":null,"on_demand_enabled":false}}),
            }),
            "x.ai/session/usage" => {
                let session = required_session_id(&request.params)?;
                let result = super::usage::session_usage(
                    &self.config.node,
                    &self.config.node_did,
                    &self.config.agent_id,
                    &session,
                )
                .await?;
                Ok(RequestOutcome {
                    notifications: Vec::new(),
                    result,
                })
            }
            "x.ai/workflows/list" => Ok(RequestOutcome {
                notifications: Vec::new(),
                result: super::workflow_catalog::list(
                    &self.config.node,
                    &self.config.node_did,
                    &self.config.agent_id,
                    &self.config.plugin_home,
                )
                .await?,
            }),
            "x.ai/sessions/list" => Ok(RequestOutcome {
                notifications: Vec::new(),
                result: super::sessions::roster(
                    &self.config.node,
                    &self.config.node_did,
                    &self.config.agent_id,
                )
                .await?,
            }),
            "x.ai/session/list" => {
                let params: super::sessions::ListParams =
                    serde_json::from_value(request.params.clone()).map_err(|error| {
                        invalid_params(format!("invalid session list: {error}"))
                    })?;
                params
                    .validate()
                    .map_err(|error| invalid_params(error.to_string()))?;
                let result = super::sessions::list(
                    &self.config.node,
                    &self.config.node_did,
                    &self.config.agent_id,
                    params,
                )
                .await?;
                Ok(RequestOutcome {
                    notifications: Vec::new(),
                    result,
                })
            }
            SESSION_NEW_METHOD => {
                let outcome = self.handle_session_new(request).await?;
                if let Some(session_id) = outcome.result.get("sessionId").and_then(Value::as_str) {
                    self.turns
                        .observe_session(
                            session_id,
                            prompt_sender.clone(),
                            self.projections.clone(),
                        )
                        .await;
                }
                Ok(outcome)
            }
            SESSION_SET_MODEL_METHOD => self.handle_session_set_model(request).await,
            SESSION_SET_MODE_METHOD => self.handle_session_set_mode(request, prompt_sender).await,
            SESSION_PROMPT_METHOD => self.handle_session_prompt(request, prompt_sender).await,
            SESSION_LOAD_METHOD => self.handle_session_load(request, prompt_sender).await,
            INTERJECT_METHOD | COMPACT_CONVERSATION_METHOD => {
                Err(shaped_stub_error(&request.method, &request.params))
            }
            SUBAGENT_GET_METHOD | SUBAGENT_LIST_RUNNING_METHOD | SUBAGENT_CANCEL_METHOD => {
                self.handle_subagent_ext_request(request.method.as_str(), request)
                    .await
            }
            "x.ai/task/kill" => {
                let params: super::task_control::KillTaskRequest =
                    serde_json::from_value(request.params.clone())
                        .map_err(|error| invalid_params(format!("invalid task kill: {error}")))?;
                if params.session_id.trim().is_empty() || params.task_id.trim().is_empty() {
                    return Err(invalid_params(
                        "task kill requires non-empty sessionId and taskId",
                    ));
                }
                let sessions = self
                    .sessions
                    .lock()
                    .await
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>();
                let result = super::task_control::kill(
                    self.config.node.clone(),
                    &self.projections.background_executions,
                    &self.config.node_did,
                    &sessions,
                    params,
                )
                .await?;
                Ok(RequestOutcome {
                    notifications: Vec::new(),
                    result: json!({"result":result}),
                })
            }
            other if other.starts_with("terminal/") => {
                let error = super::projection::tools::handle_terminal_client_method(other)
                    .expect_err("terminal client methods are never supported");
                Err(anyhow::Error::new(ShapedMethodNotFound {
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                }))
            }
            other => Err(anyhow::Error::new(ShapedMethodNotFound {
                message: format!("method {other:?} is not supported by the Gents Grok shim"),
            })),
        }
    }
}

impl AcpDelegate for AcpService {
    fn handle_acp<'a>(
        &'a self,
        payload: &'a str,
        outbound: AcpOutbound,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            // The foreground transport turn ends only after its response is
            // enqueued. Keep autonomous session output behind that boundary,
            // including the gap after the durable request terminalizes.
            let envelope = serde_json::from_str::<Value>(payload).ok();
            let _foreground_delivery = if let Some(session_id) = envelope
                .as_ref()
                .filter(|value| {
                    value.get("method").and_then(Value::as_str) == Some(SESSION_PROMPT_METHOD)
                })
                .and_then(|value| value.pointer("/params/sessionId"))
                .and_then(Value::as_str)
            {
                Some(self.turns.begin_foreground_delivery(session_id).await)
            } else {
                None
            };
            let sender = PromptSender::Live {
                outbound: outbound.clone(),
            };
            let dispatch = self.dispatch_with_sender(payload, &sender).await;
            // The MCP completion must follow session/new's response so the
            // pager has attached the session's routing.
            let (after_response, before_response): (Vec<_>, Vec<_>) = dispatch
                .notifications
                .into_iter()
                .partition(|notification| {
                    serde_json::from_str::<Value>(notification)
                        .ok()
                        .and_then(|value| {
                            value
                                .get("method")
                                .and_then(Value::as_str)
                                .map(|method| internal_method(method).to_owned())
                        })
                        .as_deref()
                        == Some(MCP_INITIALIZED_METHOD)
                });
            for notification in before_response {
                outbound.send(notification).await?;
            }
            if let Some(response) = dispatch.response {
                outbound.send(response).await?;
            }
            for notification in after_response {
                outbound.send(notification).await?;
            }
            self.activate_loaded_session(payload, &sender).await;
            Ok(())
        })
    }

    fn on_disconnect(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if let Err(error) = self.turns.handle_disconnect().await {
                tracing::warn!(
                    %error,
                    "grok shim failed to drain pending turns on disconnect"
                );
            }
        })
    }
}

#[cfg(test)]
impl AcpService {
    pub(super) async fn handle_acp_payload(&self, payload: &str) -> AcpDispatch {
        let buffer = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let sender = PromptSender::Buffer {
            buffer: buffer.clone(),
        };
        let dispatch = self.dispatch_with_sender(payload, &sender).await;
        self.activate_loaded_session(payload, &sender).await;
        dispatch
    }
}

#[cfg(test)]
mod tests;
