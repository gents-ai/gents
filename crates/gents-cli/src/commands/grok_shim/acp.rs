use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use gents::config_client::ConfigAccess;
use gents::defra_node::EmbeddedNode;
use gents::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use serde_json::{json, Value};

use super::projection::caused_sessions::{
    SUBAGENT_CANCEL_METHOD, SUBAGENT_GET_METHOD, SUBAGENT_LIST_RUNNING_METHOD,
};
use super::projection::{
    effective_context_window_tokens, stamp_update_meta, AsyncCommit, ProjectionEngine,
    UpdateTimestamps, SESSION_UPDATE_METHOD,
};
use super::protocol::{ClientCapabilities, RegisterMode};
use super::server::{AcpDelegate, AcpOutbound};
use super::turn::{
    parse_cancel_notification, parse_prompt_request, PromptSender, PromptSenderLine, TurnManager,
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

#[derive(Debug, Clone)]
struct AcpSessionState {
    mode_id: String,
    resume_from: Option<String>,
}

impl AcpSessionState {
    fn new() -> Self {
        Self {
            mode_id: "default".to_string(),
            resume_from: None,
        }
    }
}

/// Connection-local reservation only. No mutex is held across replay I/O;
/// cancellation releases the reservation synchronously through Drop.
struct SessionLoadReservation<'a> {
    loading: &'a std::sync::Mutex<BTreeSet<String>>,
    session_id: String,
}

impl<'a> SessionLoadReservation<'a> {
    fn reserve(loading: &'a std::sync::Mutex<BTreeSet<String>>, session_id: &str) -> Result<Self> {
        if !loading
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(session_id.to_owned())
        {
            return Err(invalid_params(
                "session is already loading on this connection",
            ));
        }
        Ok(Self {
            loading,
            session_id: session_id.to_owned(),
        })
    }
}

impl Drop for SessionLoadReservation<'_> {
    fn drop(&mut self) {
        self.loading
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.session_id);
    }
}

struct ModeStateCommit<'a> {
    sessions: &'a tokio::sync::Mutex<BTreeMap<String, AcpSessionState>>,
    session_id: String,
    mode_id: String,
}

impl AsyncCommit for ModeStateCommit<'_> {
    async fn commit(&self) {
        let mut sessions = self.sessions.lock().await;
        if let Some(state) = sessions.get_mut(&self.session_id) {
            state.mode_id = self.mode_id.clone();
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AcpRequest {
    pub(crate) id: Option<Value>,
    pub(crate) method: String,
    pub(crate) params: Value,
}

impl AcpRequest {
    pub(crate) fn from_payload(payload: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(payload)
            .with_context(|| "decoding Grok shim ACP payload".to_string())?;
        let object = value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("ACP payload is not a JSON object"))?;
        let method = object
            .get("method")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("ACP payload has no method"))?
            .to_string();
        let method = internal_method(&method).to_owned();
        let id = match object.get("id") {
            Some(Value::Null) | None => None,
            Some(id) => Some(id.clone()),
        };
        let params = object.get("params").cloned().unwrap_or_else(|| json!({}));
        Ok(Self { id, method, params })
    }

    fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AcpDispatch {
    pub(crate) notifications: Vec<String>,
    pub(crate) response: Option<String>,
}

struct RequestOutcome {
    notifications: Vec<Value>,
    result: Value,
}

fn response_line(id: &Value, result: &Value) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }))
    .expect("JSON-RPC response envelope is serializable")
}

fn error_line(id: &Value, code: i64, message: &str) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    }))
    .expect("JSON-RPC error envelope is serializable")
}

pub(super) fn wire_method(method: &str) -> std::borrow::Cow<'_, str> {
    if method.starts_with("x.ai/") {
        std::borrow::Cow::Owned(format!("_{method}"))
    } else {
        std::borrow::Cow::Borrowed(method)
    }
}

pub(super) fn internal_method(method: &str) -> &str {
    if method.starts_with("_x.ai/") {
        &method[1..]
    } else {
        method
    }
}

fn notification_line(method: &str, params: &Value) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "method": wire_method(method),
        "params": params,
    }))
    .expect("JSON-RPC notification envelope is serializable")
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
        let id = request.id.clone().unwrap_or_else(|| Value::Null);
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
                        .map_err(|error| invalid_params(&format!("invalid task kill: {error}")))?;
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

    async fn handle_initialize(&self, request: &AcpRequest) -> Result<RequestOutcome> {
        tracing::debug!(
            request_id = ?request.id,
            protocol_version = ACP_PROTOCOL_VERSION,
            "grok shim initialize"
        );
        Ok(RequestOutcome {
            notifications: Vec::new(),
            result: json!({
                "protocolVersion": ACP_PROTOCOL_VERSION,
                "agentCapabilities": {
                    "loadSession": true,
                    "prompt": true,
                    "cancel": true,
                    "setMode": true,
                    "setModel": true,
                },
                "authMethods": [{
                    "id": GENTS_AUTH_METHOD_ID,
                    "name": "Gents runtime",
                    "description": "Gents runtime identity",
                }],
            }),
        })
    }

    async fn handle_authenticate(&self, request: &AcpRequest) -> Result<RequestOutcome> {
        let method_id = request
            .params
            .get("methodId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if method_id != GENTS_AUTH_METHOD_ID {
            return Err(invalid_params(&format!(
                "unsupported auth method {method_id:?}; the Grok shim advertises only \
                 {GENTS_AUTH_METHOD_ID:?}"
            )));
        }
        Ok(RequestOutcome {
            notifications: Vec::new(),
            result: json!({
                "_meta": { "provider": "gents" },
            }),
        })
    }

    async fn handle_session_new(&self, request: &AcpRequest) -> Result<RequestOutcome> {
        let meta = request
            .params
            .get("_meta")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let preferred = meta
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let requested_model = meta.get("modelId").and_then(Value::as_str);

        let registered = self.registered_capabilities.as_ref();
        let yolo_mode = meta
            .get("yoloMode")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| registered.is_some_and(|caps| caps.yolo_mode));
        let auto_mode = meta
            .get("autoMode")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| registered.is_some_and(|caps| caps.auto_mode));
        let client_terminal = meta
            .get("clientTerminal")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| registered.is_some_and(|caps| caps.terminal));

        if let Some(requested) = requested_model.map(str::trim).filter(|v| !v.is_empty()) {
            if requested != self.config.current_model.model_id {
                return Err(invalid_params(&format!(
                    "model {requested:?} is not in the bound catalog; the Grok shim serves \
                     {:?} from the bound Agent/InferenceProfile",
                    self.config.current_model.model_id
                )));
            }
        }

        let session_id = preferred.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        ensure_session_document(&self.config, &session_id).await?;
        self.write_pager_session_record(request, &session_id);

        {
            let mut sessions = self.sessions.lock().await;
            if self
                .loading_sessions
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .contains(&session_id)
            {
                return Err(invalid_params("session is loading on this connection"));
            }
            sessions.insert(session_id.clone(), AcpSessionState::new());
        }
        tracing::info!(%session_id, "grok shim session/new");

        Ok(RequestOutcome {
            notifications: vec![json!({
                "__method": MCP_INITIALIZED_METHOD,
                "sessionId": session_id,
                "mcpToolCount": 0_u32,
                "elapsedMs": 0_u64,
            })],
            result: json!({
                "sessionId": session_id,
                "models": self.config.models_object(),
                "_meta": {
                    "yoloMode": yolo_mode,
                    "autoMode": auto_mode,
                    "clientTerminal": client_terminal,
                    "modelId": self.config.current_model.model_id,
                },
            }),
        })
    }

    fn write_pager_session_record(&self, request: &AcpRequest, session_id: &str) {
        let Some(grok_home) = self.config.grok_home.as_deref() else {
            return;
        };
        let cwd = request
            .params
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        match super::session_record::write_record(grok_home, cwd, session_id) {
            Ok(Some(path)) => tracing::debug!(
                %session_id,
                path = %path.display(),
                "grok shim wrote the pager's session record"
            ),
            Ok(None) => {}
            Err(error) => tracing::warn!(
                %error,
                %session_id,
                grok_home = %grok_home.display(),
                "grok shim could not write the pager's session record; grok will list this \
                 session but not resume it"
            ),
        }
    }

    async fn handle_session_load(
        &self,
        request: &AcpRequest,
        sender: &PromptSender,
    ) -> Result<RequestOutcome> {
        let session_id = required_session_id(&request.params)?;
        let _reservation = SessionLoadReservation::reserve(&self.loading_sessions, &session_id)?;
        if self.sessions.lock().await.contains_key(&session_id) {
            return Err(invalid_params(
                "session is already attached on this connection",
            ));
        }
        let attached_at = chrono::Utc::now().to_rfc3339();
        let rows = super::sessions::load(
            &self.config.node,
            &self.config.node_did,
            &self.config.agent_id,
            &session_id,
        )
        .await?;
        let running = self
            .turns
            .replay_session(&session_id, &rows, sender, &self.projections)
            .await?;
        let mut state = AcpSessionState::new();
        state.resume_from = Some(attached_at);
        self.sessions.lock().await.insert(session_id.clone(), state);
        Ok(RequestOutcome {
            notifications: vec![
                json!({"__method":MCP_INITIALIZED_METHOD, "sessionId":session_id, "mcpToolCount":0, "elapsedMs":0}),
            ],
            result: json!({"models":self.config.models_object(), "_meta":{
                "x.ai/runningPromptId":running, "modelId":self.config.current_model.model_id,
                "gents/codeRestored":false
            }}),
        })
    }

    async fn activate_loaded_session(&self, payload: &str, sender: &PromptSender) {
        let Ok(request) = AcpRequest::from_payload(payload) else {
            return;
        };
        if request.method != SESSION_LOAD_METHOD {
            return;
        }
        let Ok(session_id) = required_session_id(&request.params) else {
            return;
        };
        let attached = self
            .sessions
            .lock()
            .await
            .get_mut(&session_id)
            .and_then(|state| state.resume_from.take());
        if let Some(attached) = attached {
            self.turns
                .observe_session_since(
                    &session_id,
                    attached,
                    sender.clone(),
                    self.projections.clone(),
                )
                .await;
        }
    }

    async fn handle_session_set_model(&self, request: &AcpRequest) -> Result<RequestOutcome> {
        let session_id = required_session_id(&request.params)?;
        let requested = request
            .params
            .get("modelId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid_params("session/set_model requires a non-empty modelId"))?;
        if requested != self.config.current_model.model_id {
            return Err(invalid_params(&format!(
                "model {requested:?} is not in the bound catalog; the Grok shim serves \
                 {:?} from the bound Agent/InferenceProfile",
                self.config.current_model.model_id
            )));
        }
        if let Some(effort) = request
            .params
            .get("_meta")
            .and_then(|meta| meta.get("reasoningEffort"))
        {
            return Err(invalid_params(&format!(
                "session/set_model does not support _meta.reasoningEffort {effort:?}: the \
                 advertised catalog is text-only with supportsReasoningEffort=false, and a \
                 selected effort is not plumbed into SubmitRequestOptions or provider \
                 inference, so it is rejected rather than silently stored"
            )));
        }

        {
            let sessions = self.sessions.lock().await;
            if !sessions.contains_key(&session_id) {
                anyhow::bail!("unknown session {session_id:?}");
            }
        }

        tracing::info!(%session_id, %requested, "grok shim session/set_model");
        Ok(RequestOutcome {
            notifications: vec![{
                let mut notification = self.config.models_object();
                notification["__method"] = json!(MODELS_UPDATE_METHOD);
                notification
            }],
            result: json!({}),
        })
    }

    async fn handle_session_set_mode(
        &self,
        request: &AcpRequest,
        prompt_sender: &PromptSender,
    ) -> Result<RequestOutcome> {
        let session_id = required_session_id(&request.params)?;
        let mode_id = request
            .params
            .get("modeId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid_params("session/set_mode requires a non-empty modeId"))?;

        {
            let sessions = self.sessions.lock().await;
            if !sessions.contains_key(&session_id) {
                anyhow::bail!("unknown session {session_id:?}");
            }
        }

        let commit = ModeStateCommit {
            sessions: &self.sessions,
            session_id: session_id.clone(),
            mode_id: mode_id.to_string(),
        };
        self.projections
            .session_updates()
            .send_with_commit(
                &session_id,
                |event_id, total_tokens| {
                    let meta = stamp_update_meta(
                        event_id,
                        total_tokens,
                        None,
                        None,
                        UpdateTimestamps {
                            agent_timestamp_ms: Some(chrono::Utc::now().timestamp_millis()),
                            ..UpdateTimestamps::default()
                        },
                    );
                    Ok(json!({
                        "jsonrpc": "2.0",
                        "method": SESSION_UPDATE_METHOD,
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "current_mode_update",
                                "currentModeId": mode_id,
                            },
                            "_meta": meta,
                        },
                    }))
                },
                PromptSenderLine(prompt_sender),
                commit,
            )
            .await?;
        tracing::info!(%session_id, %mode_id, "grok shim session/set_mode");
        Ok(RequestOutcome {
            notifications: Vec::new(),
            result: json!({}),
        })
    }

    async fn handle_session_prompt(
        &self,
        request: &AcpRequest,
        prompt_sender: &PromptSender,
    ) -> Result<RequestOutcome> {
        let parsed = parse_prompt_request(&request.params, request.id.clone())
            .map_err(|error| invalid_params(error.to_string()))?;
        {
            let sessions = self.sessions.lock().await;
            let session = sessions
                .get(&parsed.session_id)
                .context("session must be created or loaded before prompting")?;
            if session.resume_from.is_some() {
                return Err(invalid_params(
                    "session replay has not handed off to live observation",
                ));
            }
        }
        if let [block] = parsed.prompt.as_slice() {
            if block.kind == "text"
                && block
                    .meta
                    .as_ref()
                    .is_none_or(|meta| meta.as_object().is_some_and(|meta| meta.is_empty()))
            {
                let goal = super::goals::GoalCommand::parse(&block.text)
                    .map_err(|error| invalid_params(error.to_string()))?
                    .filter(|command| !matches!(command, super::goals::GoalCommand::Create { .. }));
                let workflow = super::workflow_command::WorkflowCommand::parse(&block.text);
                if goal.is_some() || workflow.is_some() {
                    super::sessions::load(
                        &self.config.node,
                        &self.config.node_did,
                        &self.config.agent_id,
                        &parsed.session_id,
                    )
                    .await?;
                    let reply = match (goal, workflow) {
                        (Some(command), _) => {
                            command
                                .execute(
                                    &self.config.node,
                                    &self.config.node_did,
                                    &parsed.session_id,
                                )
                                .await?
                        }
                        (None, Some(command)) => {
                            command
                                .execute(
                                    &self.config.node,
                                    &self.config.node_did,
                                    &parsed.session_id,
                                )
                                .await?
                        }
                        (None, None) => unreachable!("a host command matched above"),
                    };
                    for (kind, text) in [
                        ("user_message_chunk", block.text.as_str()),
                        ("agent_message_chunk", reply.as_str()),
                    ] {
                        self.projections.session_updates().send(&parsed.session_id, |event_id, total_tokens| {
                            Ok(super::projection::session_notification_for_method("session/update", &parsed.session_id,
                                json!({"sessionUpdate":kind,"content":{"type":"text","text":text}, "_meta":{"hostTurn":true}}),
                                super::projection::stamp_update_meta(event_id, total_tokens,
                                    parsed.prompt_id.as_deref(), None, super::projection::UpdateTimestamps::default())))
                        }, super::turn::PromptSenderLine(prompt_sender)).await?;
                    }
                    self.turns
                        .observe_session(
                            &parsed.session_id,
                            prompt_sender.clone(),
                            self.projections.clone(),
                        )
                        .await;
                    return Ok(RequestOutcome {
                        notifications: Vec::new(),
                        result: json!({"stopReason":"end_turn"}),
                    });
                }
            }
        }
        let stop_reason = self
            .turns
            .handle_prompt(parsed, prompt_sender, &self.projections)
            .await?;
        Ok(RequestOutcome {
            notifications: Vec::new(),
            result: stop_reason,
        })
    }

    async fn handle_subagent_ext_request(
        &self,
        method: &str,
        request: &AcpRequest,
    ) -> Result<RequestOutcome> {
        let session_id = if method == SUBAGENT_LIST_RUNNING_METHOD
            || request.params.get("sessionId").is_some()
        {
            Some(required_session_id(&request.params)?)
        } else {
            None
        };
        if method == SUBAGENT_GET_METHOD || method == SUBAGENT_CANCEL_METHOD {
            let id = request.params.get("subagentId");
            match id {
                None | Some(Value::Null) => {
                    return Err(invalid_params("subagent request requires a subagentId"));
                }
                Some(value) if !value.is_string() => {
                    return Err(invalid_params(&format!(
                        "subagent request requires a string subagentId, got {value:?}"
                    )));
                }
                Some(value) if value.as_str().is_some_and(|id| id.trim().is_empty()) => {
                    return Err(invalid_params(
                        "subagent request requires a non-empty subagentId",
                    ));
                }
                _ => {}
            }
        }
        let sessions = {
            let known = self.sessions.lock().await;
            match session_id {
                Some(session) => {
                    if !known.contains_key(&session) {
                        return Err(invalid_params(
                            "subagent request names an unknown connection session",
                        ));
                    }
                    vec![session]
                }
                None => known.keys().cloned().collect(),
            }
        };
        let result = super::projection::caused_sessions::handle(
            self.config.node.clone(),
            &self.config.node_did,
            &sessions,
            method,
            &request.params,
            self.config.current_model.effective_context_window(),
        )
        .await?;
        Ok(RequestOutcome {
            notifications: Vec::new(),
            result: json!({"result": result}),
        })
    }
}

#[derive(Debug)]
pub(crate) struct ShapedMethodNotFound {
    pub(crate) message: String,
}

impl std::fmt::Display for ShapedMethodNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ShapedMethodNotFound {}

fn shaped_stub_error(method: &str, params: &Value) -> anyhow::Error {
    let session = optional_session_id(params).unwrap_or_default();
    let message = match method {
        INTERJECT_METHOD => format!(
            "x.ai/interject is not supported by the Gents Grok shim: the owned completion \
             loop has no formally specified injection transition, and writing a detached \
             AgentMessage would not affect provider input. No AgentMessage or \
             AgentRequest document was fabricated for session {session:?}, and no \
             x.ai/session/interjection notification was emitted."
        ),
        COMPACT_CONVERSATION_METHOD => format!(
            "x.ai/compact_conversation is not supported by the Gents Grok shim: \
             CompactionEntry is runtime-owned, its schema has no tokens_before or \
             tokens_after fields, and AgentSession has no usage counters. No \
             CompactionEntry or AgentSession field was fabricated for session \
             {session:?}."
        ),
        other => format!("method {other:?} is not supported by the Gents Grok shim"),
    };
    anyhow::Error::new(ShapedMethodNotFound { message })
}

fn error_code_for(error: &anyhow::Error) -> i64 {
    if error.downcast_ref::<ShapedMethodNotFound>().is_some() {
        JSONRPC_METHOD_NOT_FOUND
    } else if error.downcast_ref::<InvalidParams>().is_some() {
        JSONRPC_INVALID_PARAMS
    } else {
        JSONRPC_INTERNAL_ERROR
    }
}

#[derive(Debug)]
struct InvalidParams {
    message: String,
}

impl std::fmt::Display for InvalidParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for InvalidParams {}

fn invalid_params(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(InvalidParams {
        message: message.into(),
    })
}

fn required_session_id(params: &Value) -> Result<String> {
    params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| invalid_params("request requires a non-empty sessionId"))
}

fn optional_session_id(params: &Value) -> Option<String> {
    params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

async fn ensure_session_document(config: &AcpServiceConfig, session_id: &str) -> Result<()> {
    let escaped_session_id = escape_graphql_string(session_id);
    let lookup = format!(
        r#"{{
            AgentSession(filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}) {{
                session_id
                node_did
                requester_did
                agent_id
            }}
        }}"#
    );
    let response =
        graphql_with_transaction_retry(&config.node, &lookup, "grok shim AgentSession lookup")
            .await?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentSession"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(existing) = rows
        .iter()
        .filter_map(|row| row.get("agent_id").and_then(Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())
    {
        if existing != config.agent_id.as_ref() {
            anyhow::bail!(
                "session {session_id:?} already exists with agent {existing:?}; the \
                 Grok shim is bound to {:?} and will not rewrite session identity",
                config.agent_id
            );
        }
    }
    // The serving node DID is `@immutable` on `AgentSession`: an existing
    // row stamped for a different node is a hard identity mismatch and
    // must never be reactivated under this shim's identity.
    if let Some(existing) = rows
        .iter()
        .filter_map(|row| row.get("node_did").and_then(Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())
    {
        if existing != config.node_did.as_ref() {
            anyhow::bail!(
                "session {session_id:?} already exists with agent {existing:?}; the \
                 Grok shim serves as {:?} and will not reactivate a session bound to \
                 a different immutable node_did",
                config.node_did
            );
        }
    }

    anyhow::ensure!(
        rows.iter()
            .all(|row| row.get("requester_did").and_then(Value::as_str)
                == Some(config.node_did.as_ref())),
        "session belongs to a different immutable requester_did"
    );
    let escaped_node_did = escape_graphql_string(&config.node_did);
    let escaped_agent_id = escape_graphql_string(&config.agent_id);
    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let escaped_created_at = escape_graphql_string(&created_at);
    if rows.is_empty() {
        let create = format!(
            r#"mutation {{
                create_AgentSession(input: {{
                    session_id: "{escaped_session_id}",
                    node_did: "{escaped_node_did}",
                    requester_did: "{escaped_node_did}",
                    agent_id: "{escaped_agent_id}",
                    created_at: "{escaped_created_at}"
                }}) {{ _docID }}
            }}"#
        );
        ConfigAccess::local(config.node.clone())
            .write("grok.session.create", &create)
            .await?;
    } else {
        let update = format!(
            r#"mutation {{
                update_AgentSession(
                    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                    input: {{
                        closed_at: null
                    }}
                ) {{ _docID }}
            }}"#
        );
        ConfigAccess::local(config.node.clone())
            .write("grok.session.reactivate", &update)
            .await?;
    }
    Ok(())
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
mod tests {
    use super::*;
    use crate::commands::grok_shim::projection::tools::PAGER_WAIT_FOR_EXIT_MESSAGE;
    use crate::commands::grok_shim::test_fixtures::seed_canonical_assistant_message;
    use crate::commands::grok_shim::test_fixtures::{
        configure_runtime_agent, seed_job_row,
        streaming_backend::{MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse},
    };
    use crate::commands::grok_shim::turn::PromptBlock;
    use gents::graphql::ensure_no_errors;
    use tokio::sync::Mutex;

    fn bound_model() -> BoundModel {
        BoundModel {
            model_id: "GLM-5.3-NVFP4".to_string(),
            name: "GLM 5.3 NVFP4".to_string(),
            total_context_tokens: 262_144,
        }
    }

    async fn config() -> (tempfile::TempDir, AcpServiceConfig) {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let identity = gents::KeyIdentity::load_or_create(tempdir.path().join("actor.key"), None)
            .expect("test signing identity");
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(gents::NodeIdentity::did(&identity))
                .data_path(tempdir.path().join("node"))
                .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
                .build()
                .await
                .expect("embedded node"),
        );
        let plugin_home = tempdir.path().join("home");
        (
            tempdir,
            AcpServiceConfig {
                node,
                node_did: Arc::from("did:test:grok-shim"),
                agent_id: Arc::from("did:test:grok-shim:default"),
                current_model: bound_model(),
                grok_home: None,
                plugin_home,
            },
        )
    }

    async fn test_service() -> (tempfile::TempDir, AcpService) {
        service_over(false).await
    }

    /// `signing_node`: the node DID is the registered test signing identity,
    /// for owners that author documents as the node (job control's kill).
    async fn service_over(signing_node: bool) -> (tempfile::TempDir, AcpService) {
        let (staging, mut config) = config().await;
        let identity = gents::KeyIdentity::load_or_create(staging.path().join("actor.key"), None)
            .expect("test signing identity");
        let actor = ::identity::Did::new(gents::NodeIdentity::did(&identity).to_owned())
            .expect("fixture creator DID");
        if signing_node {
            config.node_did = Arc::from(gents::NodeIdentity::did(&identity));
        }
        gents::schema::ensure_runtime_schemas(config.node.as_ref())
            .await
            .expect("runtime schemas");
        let turns = Arc::new(TurnManager::new(
            config.node.clone(),
            super::super::turn::TurnManagerConfig {
                actor,
                node_did: config.node_did.to_string(),
                agent_id: config.agent_id.to_string(),
                graphql: "http://127.0.0.1:8000/api/v0/graphql".to_string(),
            },
        ));
        let projections = Arc::new(ProjectionEngine::new(
            config.node.clone(),
            super::super::projection::BoundModelContext::new(
                config.current_model.model_id.clone(),
                config.current_model.name.clone(),
                config.current_model.total_context_tokens,
            ),
        ));
        (staging, AcpService::new(config, turns, projections))
    }

    #[tokio::test]
    async fn goal_slash_controls_use_runtime_owner_without_submitting_inference() {
        let (_dir, service) = test_service().await;
        let new = service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({"_meta":{"sessionId":"goal-controls"}}),
            ))
            .await;
        assert!(parse_response(new.response.as_deref().unwrap())
            .get("error")
            .is_none());
        let node = &service.config.node;
        gents::goal::set_goal(
            node,
            &service.config.node_did,
            "goal-controls",
            Some("Finish the feature"),
            None,
            Some(Some(1000)),
        )
        .await
        .unwrap();
        for (command, expected, expected_reply) in [
            ("status", Some("active"), None),
            ("pause", Some("paused"), None),
            (
                "resume",
                Some("paused"),
                Some("gents goal resume-request --session SESSION --from REQUEST_ID"),
            ),
            ("clear", None, None),
        ] {
            let dispatch = service.handle_acp_payload(&request_payload("session/prompt", json!({
                "sessionId":"goal-controls", "prompt":[{"type":"text", "text":format!("/goal {command}"), "meta":{}}]
            }))).await;
            let response = parse_response(dispatch.response.as_deref().unwrap());
            assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
            assert!(dispatch.notifications.iter().any(|line| {
                let update = parse_response(line);
                update["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
                    && update["params"]["update"]["_meta"]["hostTurn"] == true
            }));
            if let Some(expected_reply) = expected_reply {
                assert!(dispatch.notifications.iter().any(|line| {
                    let update = parse_response(line);
                    update["params"]["update"]["content"]["text"]
                        .as_str()
                        .is_some_and(|text| text.contains(expected_reply))
                }));
            }
            let goal =
                gents::goal::load_canonical_goal(node, &service.config.node_did, "goal-controls")
                    .await
                    .unwrap();
            assert_eq!(goal.as_ref().map(|goal| goal.status.as_str()), expected);
        }
        let requests = node.execute("{AgentRequest {_docID}}").await;
        ensure_no_errors(&requests, "goal controls do not infer").unwrap();
        assert!(requests.data.as_ref().unwrap()["AgentRequest"]
            .as_array()
            .unwrap()
            .is_empty());
        let denied = service
            .handle_acp_payload(&request_payload(
                "session/prompt",
                json!({
                    "sessionId":"not-attached", "prompt":[{"type":"text", "text":"/goal clear"}]
                }),
            ))
            .await;
        assert!(parse_response(denied.response.as_deref().unwrap())
            .get("error")
            .is_some());
        service.turns.handle_disconnect().await.unwrap();
    }

    async fn workflow_session(session: &str) -> (tempfile::TempDir, AcpService) {
        let (dir, service) = service_over(true).await;
        let new = service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({"_meta":{"sessionId":session}}),
            ))
            .await;
        assert!(parse_response(new.response.as_deref().unwrap())
            .get("error")
            .is_none());
        (dir, service)
    }

    async fn host_reply(service: &AcpService, session: &str, text: &str) -> String {
        let dispatch = service
            .handle_acp_payload(&request_payload(
                "session/prompt",
                json!({"sessionId":session, "prompt":[{"type":"text", "text":text, "meta":{}}]}),
            ))
            .await;
        let response = parse_response(dispatch.response.as_deref().unwrap());
        assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
        dispatch
            .notifications
            .iter()
            .map(|line| parse_response(line))
            .find(|update| {
                update["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
                    && update["params"]["update"]["_meta"]["hostTurn"] == true
            })
            .map(|update| {
                update["params"]["update"]["content"]["text"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .unwrap_or_else(|| panic!("{text} produced no host turn"))
    }

    async fn no_requests_were_submitted(service: &AcpService) {
        let requests = service
            .config
            .node
            .execute(r#"{AgentRequest(filter: {request_id: {_nlike: "request-call-%"}}) {_docID}}"#)
            .await;
        ensure_no_errors(&requests, "workflow controls do not infer").unwrap();
        assert!(requests.data.as_ref().unwrap()["AgentRequest"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn workflow_runs_overview() {
        use gents_protocol::job::JobState;
        let (_dir, service) = workflow_session("wf-runs").await;
        let node = &service.config.node;
        let did = &*service.config.node_did;
        seed_job_row(
            node,
            did,
            "wf-runs",
            "call-a",
            "alpha",
            "running",
            JobState::Running,
        )
        .await;
        seed_job_row(
            node,
            did,
            "wf-runs",
            "call-b",
            "beta",
            "completed",
            JobState::Completed,
        )
        .await;
        for text in ["/workflow", "/workflow runs"] {
            let reply = host_reply(&service, "wf-runs", text).await;
            assert!(reply.contains("alpha"), "{reply}");
            assert!(reply.contains("wf-runs:call-a"), "{reply}");
            assert!(reply.contains("running"), "{reply}");
            assert!(!reply.contains("beta"), "{reply}");
        }
        no_requests_were_submitted(&service).await;
        service.turns.handle_disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn stop_or_pause_without_a_handle_lists_the_handles() {
        use gents_protocol::job::JobState;
        let (_dir, service) = workflow_session("wf-list").await;
        let node = &service.config.node;
        let did = &*service.config.node_did;
        seed_job_row(
            node,
            did,
            "wf-list",
            "call-a",
            "alpha",
            "running",
            JobState::Running,
        )
        .await;
        for text in ["/workflow stop", "/workflow pause"] {
            let reply = host_reply(&service, "wf-list", text).await;
            assert!(reply.contains("alpha"), "{reply}");
            assert!(reply.contains("wf-list:call-a"), "{reply}");
        }
        no_requests_were_submitted(&service).await;
        service.turns.handle_disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn unknown_handle_replies_c9_code() {
        use gents_protocol::job::JobState;
        let (_dir, service) = workflow_session("wf-unknown").await;
        let node = &service.config.node;
        let did = &*service.config.node_did;
        seed_job_row(
            node,
            did,
            "wf-other",
            "call-x",
            "elsewhere",
            "running",
            JobState::Running,
        )
        .await;
        for text in [
            "/workflow stop nope",
            "/workflow pause nope",
            "/workflow stop elsewhere",
            "/workflow stop wf-other:call-x",
        ] {
            let reply = host_reply(&service, "wf-unknown", text).await;
            assert!(
                reply.contains("workflow_control_unknown_run"),
                "{text}: {reply}"
            );
            assert!(reply.contains("/workflow runs"), "{text}: {reply}");
        }
        no_requests_were_submitted(&service).await;
        service.turns.handle_disconnect().await.unwrap();
    }

    async fn job_state(
        service: &AcpService,
        session: &str,
        call: &str,
    ) -> gents_protocol::job::JobState {
        gents::plugin_job::view::load_job_view(
            &ConfigAccess::local(service.config.node.clone()),
            None,
            gents::plugin_job::view::JobKey::Address {
                session_id: session.into(),
                tool_call_id: call.into(),
            },
        )
        .await
        .unwrap()
        .state
    }

    #[tokio::test]
    async fn ended_row_replies_not_applicable() {
        use gents_protocol::job::JobState;
        let (_dir, service) = workflow_session("wf-ended").await;
        seed_job_row(
            &service.config.node,
            &service.config.node_did,
            "wf-ended",
            "call-b",
            "beta",
            "completed",
            JobState::Completed,
        )
        .await;
        for text in ["/workflow stop beta", "/workflow pause beta"] {
            let reply = host_reply(&service, "wf-ended", text).await;
            assert!(
                reply.contains("workflow_control_not_applicable"),
                "{text}: {reply}"
            );
            assert!(
                reply.contains("gents job show wf-ended:call-b"),
                "{text}: {reply}"
            );
        }
        assert_eq!(
            job_state(&service, "wf-ended", "call-b").await,
            JobState::Completed
        );
        service.turns.handle_disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn workflow_stop_replies_host_turn() {
        use gents_protocol::job::JobState;
        let (_dir, service) = workflow_session("wf-stop").await;
        seed_job_row(
            &service.config.node,
            &service.config.node_did,
            "wf-stop",
            "call-a",
            "alpha",
            "running",
            JobState::Running,
        )
        .await;
        let reply = host_reply(&service, "wf-stop", "/workflow stop alpha").await;
        assert!(reply.contains("alpha"), "{reply}");
        assert!(reply.contains('0'), "{reply}");
        assert_eq!(
            job_state(&service, "wf-stop", "call-a").await,
            JobState::Cancelled
        );
        service.turns.handle_disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn workflow_pause_replies_host_turn() {
        use gents_protocol::job::JobState;
        let (_dir, service) = workflow_session("wf-pause").await;
        seed_job_row(
            &service.config.node,
            &service.config.node_did,
            "wf-pause",
            "call-a",
            "alpha",
            "running",
            JobState::Running,
        )
        .await;
        let reply = host_reply(&service, "wf-pause", "/workflow pause alpha").await;
        assert!(reply.contains("alpha"), "{reply}");
        assert!(reply.contains("/workflow resume alpha"), "{reply}");
        assert_eq!(
            job_state(&service, "wf-pause", "call-a").await,
            JobState::Paused
        );
        service.turns.handle_disconnect().await.unwrap();
    }

    #[test]
    fn forwarded_verbs_reach_prompt_text() {
        use super::super::workflow_command::WorkflowCommand;
        for text in [
            "/workflow research the topic",
            "/workflow resume alpha",
            "/workflow answer alpha yes",
            "/workflow save alpha",
        ] {
            assert_eq!(WorkflowCommand::parse(text), None, "{text}");
            let request = crate::commands::grok_shim::turn::PromptRequest {
                session_id: "s".to_owned(),
                prompt: vec![PromptBlock {
                    kind: "text".to_owned(),
                    text: text.to_owned(),
                    meta: Some(json!({})),
                }],
                prompt_id: None,
                screen_mode: None,
                send_now: false,
                id: None,
            };
            assert_eq!(super::super::turn::prompt_text(&request), text);
        }
    }

    #[tokio::test]
    async fn stalled_replay_reserves_only_its_session_and_abort_releases_it() {
        let (_dir, service) = test_service().await;
        let service = Arc::new(service);
        ensure_session_document(&service.config, "stalled-history")
            .await
            .unwrap();
        let seeded = service.config.node.execute(r#"mutation {create_AgentRequest(input:{purpose: "normal", 
            request_id:"stalled-request", session_id:"stalled-history", node_did:"did:test:grok-shim",
            requester_did:"did:test:grok-shim", content:"Original prompt", lifecycle_state:"completed",
            created_at:"2026-09-01T12:00:00Z"
        }) {_docID}}"#).await;
        ensure_no_errors(&seeded, "seed stalled replay").unwrap();
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let hold_output = buffer.lock().await;
        let sender = PromptSender::Buffer {
            buffer: buffer.clone(),
        };
        let request = AcpRequest::from_payload(&request_payload(
            "session/load",
            json!({"sessionId":"stalled-history"}),
        ))
        .unwrap();
        let task = tokio::spawn({
            let service = service.clone();
            let sender = sender.clone();
            let request = request.clone();
            async move { service.handle_session_load(&request, &sender).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !service
                .loading_sessions
                .lock()
                .unwrap()
                .contains("stalled-history")
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("load reservation");
        let unrelated = AcpRequest::from_payload(&request_payload(
            "session/new",
            json!({"_meta":{"sessionId":"unrelated"}}),
        ))
        .unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            service.handle_session_new(&unrelated),
        )
        .await
        .expect("stalled replay must not block another session")
        .unwrap();
        assert!(service
            .handle_session_load(&request, &sender)
            .await
            .is_err());
        let conflict = AcpRequest::from_payload(&request_payload(
            "session/new",
            json!({"_meta":{"sessionId":"stalled-history"}}),
        ))
        .unwrap();
        assert!(service.handle_session_new(&conflict).await.is_err());
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        assert!(service.loading_sessions.lock().unwrap().is_empty());
        drop(hold_output);
        service
            .handle_session_load(&request, &sender)
            .await
            .unwrap();
        assert!(service.loading_sessions.lock().unwrap().is_empty());
        assert!(service
            .sessions
            .lock()
            .await
            .contains_key("stalled-history"));
    }

    #[tokio::test]
    async fn session_load_replays_all_message_pages_without_creating_requests() {
        let (_dir, service) = test_service().await;
        ensure_session_document(&service.config, "resume-history")
            .await
            .unwrap();
        let node = &service.config.node;
        let result = node.execute(r#"mutation { create_AgentRequest(input: {purpose: "normal", 
            request_id: "resume-request", session_id: "resume-history", node_did: "did:test:grok-shim",
            requester_did: "did:test:grok-shim", content: "Original human prompt",
            lifecycle_state: "completed", created_at: "2026-09-01T12:00:00Z"
        }) { _docID } }"#).await;
        ensure_no_errors(&result, "seed resume request").unwrap();
        let request = node
            .execute(r#"{ AgentRequest(filter: {request_id: {_eq: "resume-request"}}, limit: 2) {_docID request_id node_did requester_did session_id} }"#)
            .await;
        ensure_no_errors(&request, "lookup resume request").unwrap();
        let rows = request.data.as_ref().unwrap()["AgentRequest"]
            .as_array()
            .unwrap();
        assert_eq!(rows.len(), 1, "resume request identity must be exact");
        let request: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(rows[0].clone()).unwrap();
        for sequence in 1_i64..=70 {
            let message_key = gents::session::sequence_message_key(
                "did:test:grok-shim",
                "resume-history",
                Some("did:test:grok-shim"),
                u32::try_from(sequence).expect("fixture sequence fits u32"),
            );
            seed_canonical_assistant_message(
                node,
                &request,
                &message_key,
                sequence,
                "",
                &format!("REPLAY_{sequence:03}\n"),
            )
            .await;
        }
        let dispatch = service
            .handle_acp_payload(&request_payload(
                "session/load",
                json!({"sessionId":"resume-history", "cwd":"/tmp", "mcpServers":[]}),
            ))
            .await;
        let response = parse_response(dispatch.response.as_deref().unwrap());
        assert!(response.get("error").is_none(), "{response}");
        assert!(response["result"]["_meta"]["x.ai/runningPromptId"].is_null());
        let events: Vec<_> = dispatch
            .notifications
            .iter()
            .map(|line| parse_response(line))
            .filter(|line| line["method"] == "session/update")
            .collect();
        assert!(events
            .iter()
            .all(|event| event["params"]["_meta"]["isReplay"] == true));
        let texts: Vec<_> = events
            .iter()
            .filter_map(|event| {
                event
                    .pointer("/params/update/content/text")
                    .and_then(Value::as_str)
            })
            .collect();
        assert_eq!(
            texts
                .iter()
                .filter(|text| **text == "Original human prompt")
                .count(),
            1
        );
        let assistant = texts
            .iter()
            .filter(|text| text.starts_with("REPLAY_"))
            .copied()
            .collect::<String>();
        assert_eq!(
            assistant,
            (1..=70)
                .map(|sequence| format!("REPLAY_{sequence:03}\n"))
                .collect::<String>()
        );
        let result = node
            .execute(r#"{ AgentRequest { request_id } AgentSession { session_id requester_did } }"#)
            .await;
        ensure_no_errors(&result, "resume write check").unwrap();
        assert_eq!(
            result.data.as_ref().unwrap()["AgentRequest"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            result.data.as_ref().unwrap()["AgentSession"][0]["requester_did"],
            "did:test:grok-shim"
        );
        let repeated = service
            .handle_acp_payload(&request_payload(
                "session/load",
                json!({"sessionId":"resume-history"}),
            ))
            .await;
        assert_eq!(
            parse_response(repeated.response.as_deref().unwrap())["error"]["code"],
            -32602
        );
        service.turns.handle_disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn history_picker_scopes_searches_paginates_and_preserves_legacy_sessions() {
        let (_dir, service) = test_service().await;
        for (id, requester, provenance) in [
            ("older", "null", ""),
            ("newer", "\"did:test:grok-shim\"", ""),
            ("foreign-session", "\"did:test:foreign\"", ""),
            (
                "child",
                "\"did:test:grok-shim\"",
                r#"provenance: {parent_request_doc_id: "doc-a"},"#,
            ),
        ] {
            let result = service
                .config
                .node
                .execute(&format!(
                    r#"mutation {{ create_AgentSession(input: {{
                session_id: "{id}", node_did: "did:test:grok-shim", requester_did: {requester},
                {provenance} agent_id: "did:test:grok-shim:default", created_at: "2026-09-01T00:00:00Z"
            }}) {{ _docID }} }}"#
                ))
                .await;
            ensure_no_errors(&result, "seed history owner").unwrap();
        }
        for (id, session, requester, content, day, parent) in [
            (
                "a",
                "older",
                "did:test:grok-shim",
                "First older prompt",
                1,
                "null",
            ),
            (
                "b",
                "newer",
                "did:test:grok-shim",
                "First newer prompt",
                2,
                "null",
            ),
            (
                "c",
                "foreign-session",
                "did:test:grok-shim",
                "SECRET session",
                4,
                "null",
            ),
            (
                "d",
                "older",
                "did:test:foreign",
                "SECRET request",
                5,
                "null",
            ),
            (
                "e",
                "child",
                "did:test:grok-shim",
                "Child instructions",
                6,
                "\"a\"",
            ),
            (
                "f",
                "older",
                "did:test:grok-shim",
                "Later searchable NEEDLE",
                3,
                "null",
            ),
        ] {
            let result = service
                .config
                .node
                .execute(&format!(
                    r#"mutation {{ create_AgentRequest(input: {{purpose: "normal", 
                request_id: "{id}", session_id: "{session}", node_did: "did:test:grok-shim",
                requester_did: "{requester}", content: "{content}", lifecycle_state: "completed",
                caused_by_parent_request_id: {parent}, created_at: "2026-09-0{day}T12:00:00Z"
            }}) {{ _docID }} }}"#
                ))
                .await;
            ensure_no_errors(&result, "seed history request").unwrap();
        }
        let dispatch = service
            .handle_acp_payload(&request_payload(
                "_x.ai/session/list",
                json!({"limit":1, "cwd":"/not-historical"}),
            ))
            .await;
        let first = parse_response(dispatch.response.as_deref().unwrap());
        assert!(first.get("error").is_none(), "{first}");
        let page = &first["result"];
        assert_eq!(page["sessions"][0]["sessionId"], "older");
        assert_eq!(page["sessions"][0]["summary"], "First older prompt");
        assert!(page["sessions"][0].get("cwd").is_none());
        assert!(page["sessions"][0].get("modelId").is_none());
        assert_eq!(page["_meta"]["x.ai/listScope"], "all");
        let dispatch = service
            .handle_acp_payload(&request_payload(
                "x.ai/session/list",
                json!({"limit":1, "cursor":page["nextCursor"]}),
            ))
            .await;
        let second = parse_response(dispatch.response.as_deref().unwrap());
        assert_eq!(second["result"]["sessions"][0]["sessionId"], "newer");
        assert!(second["result"]["nextCursor"].is_null());
        for (query, count) in [("needle", 1), ("secret", 0)] {
            let dispatch = service
                .handle_acp_payload(&request_payload(
                    "x.ai/session/list",
                    json!({"query":query}),
                ))
                .await;
            let response = parse_response(dispatch.response.as_deref().unwrap());
            assert_eq!(
                response["result"]["sessions"].as_array().unwrap().len(),
                count
            );
        }
        let rows = super::super::sessions::load(
            &service.config.node,
            &service.config.node_did,
            &service.config.agent_id,
            "older",
        )
        .await
        .unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| row.request_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "f"]
        );
        assert!(super::super::sessions::load(
            &service.config.node,
            &service.config.node_did,
            &service.config.agent_id,
            "foreign-session"
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn native_task_kill_is_scoped_idempotent_and_uses_stock_envelope() {
        let prompt = "start task process";
        let fixture = runtime_control_fixture(
            "task-kill",
            vec![StreamPlan::current_authored_user(
                prompt,
                vec![
                    StreamResponse::streams(
                        prompt,
                        vec![StreamChunk::tool_call(
                            "task-meta",
                            "spawn_process",
                            r#"{"tool_name":"bash","args":{"command":"sleep","args":["60"]}}"#,
                        )],
                    ),
                    StreamResponse::completes(prompt, ["task started"]),
                ],
            )],
        )
        .await;
        let service = &fixture.service;
        service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({"_meta":{"sessionId":"task-session"}}),
            ))
            .await;
        let completed = service
            .handle_acp_payload(&request_payload(
                "session/prompt",
                json!({"sessionId":"task-session","prompt":[{"type":"text","text":prompt}]}),
            ))
            .await;
        let completed_response = parse_response(completed.response.as_deref().unwrap());
        assert!(
            completed_response.get("error").is_none(),
            "runtime task prompt failed: {completed_response}"
        );
        let task_id =
            wait_for_running_bash_handle(service.config.node.as_ref(), "task-session").await;
        for params in [
            json!({"sessionId":"task-session"}),
            json!({"sessionId":"task-session","taskId":""}),
            json!({"sessionId":"task-session","taskId":task_id,"source":"invalid"}),
        ] {
            let response = service
                .handle_acp_payload(&request_payload("x.ai/task/kill", params))
                .await;
            assert_eq!(
                parse_response(response.response.as_deref().unwrap())["error"]["code"],
                -32602
            );
        }
        for (session, expected) in [
            ("foreign", "not_found"),
            ("task-session", "killed"),
            ("task-session", "already_exited"),
        ] {
            let response = service
                .handle_acp_payload(&request_payload(
                    "x.ai/task/kill",
                    json!({"sessionId":session, "taskId":task_id, "source":"clientUi"}),
                ))
                .await;
            let value = parse_response(response.response.as_deref().unwrap());
            assert_eq!(
                value["result"]["result"],
                json!({"taskId":task_id,"outcome":expected}),
                "{value}"
            );
        }
        let response = service.config.node.execute(&format!(r#"{{ AgentToolCall(filter: {{tool_call_id: {{_eq: "{}"}}}}) {{lifecycle_state}} }}"#, escape_graphql_string(&task_id))).await;
        assert_eq!(
            response.data.unwrap()["AgentToolCall"][0]["lifecycle_state"],
            "cancelled"
        );
        fixture.shutdown().await;
    }

    #[tokio::test]
    async fn native_child_controls_and_usage_follow_physical_lineage() {
        let parent_prompt = "spawn child worker";
        let child_prompt = "run child process";
        let unlinked_prompt = "run unlinked process";
        let child_args = serde_json::json!({
            "agent": "__BEHAVIOR__",
            "prompt": child_prompt
        });
        let mut fixture = runtime_control_fixture_with_plan_builder("child-controls", |agent_id| {
            vec![
                StreamPlan::current_authored_user(
                    parent_prompt,
                    vec![
                        StreamResponse::streams(
                            parent_prompt,
                            vec![StreamChunk::tool_call(
                                "spawn-child-meta",
                                "agent_new",
                                child_args.to_string().replace("__BEHAVIOR__", agent_id),
                            )],
                        ),
                        StreamResponse::completes(parent_prompt, ["child started"]),
                    ],
                ),
                StreamPlan::current_authored_user(
                    child_prompt,
                    vec![
                        StreamResponse::streams(
                            child_prompt,
                            vec![StreamChunk::tool_call(
                                "child-process-meta",
                                "spawn_process",
                                r#"{"tool_name":"bash","args":{"command":"sleep","args":["60"]}}"#,
                            )],
                        ),
                        StreamResponse::completes(child_prompt, ["child process started"]),
                    ],
                ),
                StreamPlan::current_authored_user(
                    unlinked_prompt,
                    vec![
                        StreamResponse::streams(
                            unlinked_prompt,
                            vec![StreamChunk::tool_call(
                                "unlinked-process-meta",
                                "spawn_process",
                                r#"{"tool_name":"bash","args":{"command":"sleep","args":["60"]}}"#,
                            )],
                        ),
                        StreamResponse::completes(unlinked_prompt, ["unlinked process started"]),
                    ],
                ),
            ]
        })
        .await;
        let service = &fixture.service;
        for (session, prompt) in [
            ("parent-session", parent_prompt),
            ("unlinked-session", unlinked_prompt),
        ] {
            service
                .handle_acp_payload(&request_payload(
                    "session/new",
                    json!({"_meta":{"sessionId":session}}),
                ))
                .await;
            let completed = service
                .handle_acp_payload(&request_payload(
                    "session/prompt",
                    json!({"sessionId":session,"prompt":[{"type":"text","text":prompt}]}),
                ))
                .await;
            let completed_response = parse_response(completed.response.as_deref().unwrap());
            assert!(
                completed_response.get("error").is_none(),
                "runtime control prompt failed for {session}: {completed_response}"
            );
        }
        let node = &service.config.node;
        let did = gents::graphql::escape_graphql_string(&service.config.node_did);
        let child = wait_for_child_request(node).await;
        let child_session = child.session_id.as_deref().unwrap();
        let child_process = wait_for_running_bash_handle(node.as_ref(), child_session).await;
        for (session, outcome) in [
            ("unlinked-session", "not_found"),
            ("parent-session", "killed"),
            (child_session, "already_exited"),
        ] {
            let response = service
                .handle_acp_payload(&request_payload(
                    "x.ai/task/kill",
                    json!({
                        "sessionId": session, "taskId": child_process, "source": "teardown"
                    }),
                ))
                .await;
            let response = parse_response(response.response.as_deref().unwrap());
            assert_eq!(
                response["result"]["result"]["outcome"], outcome,
                "{response}"
            );
        }
        let parent = exact_request_by_doc_id(
            node.as_ref(),
            child.caused_by_parent_request_doc_id.as_deref().unwrap(),
        )
        .await;
        RuntimeControlFixture::stop_runtime_parts(&fixture.shutdown, &mut fixture.runtime).await;
        let baseline = service
            .handle_acp_payload(&request_payload(
                "x.ai/session/usage",
                json!({"sessionId":"parent-session"}),
            ))
            .await;
        let baseline = parse_response(baseline.response.as_deref().unwrap());
        assert!(baseline.get("error").is_none(), "{baseline}");
        let baseline = &baseline["result"]["usage"];
        let baseline_input = baseline["inputTokens"].as_u64().unwrap();
        let baseline_output = baseline["outputTokens"].as_u64().unwrap();
        let baseline_cached = baseline["cachedReadTokens"].as_u64().unwrap();
        let baseline_calls = baseline["modelCalls"].as_u64().unwrap();
        let foreign = node.execute(&format!(r#"mutation {{ create_AgentRequest(input: {{purpose: "normal", 
            request_id: "foreign-usage", session_id: "{}", node_did: "{did}", requester_did: "did:test:foreign", lifecycle_state: "completed"
        }}) {{_docID}} }}"#, escape_graphql_string(child_session))).await;
        ensure_no_errors(&foreign, "foreign usage fixture").unwrap();
        for (request, input, output, cached) in [
            (parent.request_id.as_str(), 100, 10, 50),
            (child.request_id.as_str(), 200, 20, 100),
            ("foreign-usage", 9999, 999, 0),
        ] {
            let owner = node
                .execute(&format!(
                    r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{request}"}}}}) {{_docID}} }}"#
                ))
                .await;
            ensure_no_errors(&owner, "usage physical identity").unwrap();
            let doc = owner.data.as_ref().unwrap()["AgentRequest"][0]["_docID"]
                .as_str()
                .unwrap();
            let call = node.execute(&format!(r#"mutation {{ create_InferenceCall(input: {{
                call_id: "usage-{request}", request_id: "parent", request_doc_id: "{}", node_did: "{did}",
                call_seq: 1, call_kind: "inference", prompt_tokens: {input}, completion_tokens: {output}, cached_input_tokens: {cached}
            }}) {{_docID}} }}"#, escape_graphql_string(doc))).await;
            ensure_no_errors(&call, "usage call fixture").unwrap();
        }
        let usage = service
            .handle_acp_payload(&request_payload(
                "x.ai/session/usage",
                json!({"sessionId":"parent-session"}),
            ))
            .await;
        let response = parse_response(usage.response.as_deref().unwrap());
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(
            response["result"]["usage"]["inputTokens"],
            baseline_input + 300
        );
        assert_eq!(
            response["result"]["usage"]["outputTokens"],
            baseline_output + 30
        );
        assert_eq!(
            response["result"]["usage"]["cachedReadTokens"],
            baseline_cached + 150
        );
        assert_eq!(
            response["result"]["usage"]["modelCalls"],
            baseline_calls + 2
        );
        assert_eq!(response["result"]["usage"]["usageIsIncomplete"], true);
        assert!(response["result"]["usage"].get("costUsdTicks").is_none());
        fixture.shutdown().await;
    }

    fn request_payload(method: &str, params: Value) -> String {
        serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": wire_method(method),
            "params": params,
        }))
        .expect("request payload")
    }

    fn notification_payload(method: &str, params: Value) -> String {
        serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "method": wire_method(method),
            "params": params,
        }))
        .expect("notification payload")
    }

    fn parse_response(line: &str) -> Value {
        serde_json::from_str(line).expect("response line is JSON")
    }

    struct RuntimeControlFixture {
        _dir: tempfile::TempDir,
        service: AcpService,
        _backend: MockStreamingBackend,
        shutdown: tokio::sync::watch::Sender<bool>,
        runtime: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
    }

    impl RuntimeControlFixture {
        async fn stop_runtime_parts(
            shutdown: &tokio::sync::watch::Sender<bool>,
            runtime: &mut Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
        ) {
            let _ = shutdown.send(true);
            if let Some(runtime) = runtime.take() {
                runtime.await.unwrap().unwrap();
            }
        }

        async fn shutdown(mut self) {
            Self::stop_runtime_parts(&self.shutdown, &mut self.runtime).await;
        }
    }

    async fn runtime_control_fixture(name: &str, plans: Vec<StreamPlan>) -> RuntimeControlFixture {
        runtime_control_fixture_with_plan_builder(name, |_| plans).await
    }

    async fn runtime_control_fixture_with_plan_builder(
        name: &str,
        plans: impl FnOnce(&str) -> Vec<StreamPlan>,
    ) -> RuntimeControlFixture {
        let dir = tempfile::tempdir().unwrap();
        let identity = Arc::new(
            gents::KeyIdentity::load_or_create(dir.path().join("agent.key"), None).unwrap(),
        );
        let node_did = gents::NodeIdentity::did(identity.as_ref()).to_string();
        let agent_id = gents::default_agent_id_for_node(&node_did);
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(dir.path().join("node"))
                .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
                .with_node_identity_did(&node_did)
                .build()
                .await
                .unwrap(),
        );
        gents::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let backend =
            MockStreamingBackend::start_with_plans("grok-control-model", plans(&agent_id)).unwrap();
        configure_runtime_agent(
            &node,
            &node_did,
            &agent_id,
            &format!("grok-control-{name}"),
            backend.endpoint(),
            "grok-control-model",
            true,
        )
        .await;
        let graphql = spawn_mock_graphql(node.clone()).await;
        let config = AcpServiceConfig {
            node: node.clone(),
            node_did: Arc::from(node_did.as_str()),
            agent_id: Arc::from(agent_id.as_str()),
            current_model: bound_model(),
            grok_home: None,
            plugin_home: std::path::PathBuf::new(),
        };
        let turns = Arc::new(TurnManager::new(
            node.clone(),
            super::super::turn::TurnManagerConfig {
                actor: identity::Did::new(node_did.clone()).expect("fixture creator DID"),
                node_did: node_did.clone(),
                agent_id: agent_id.clone(),
                graphql,
            },
        ));
        let runtime_identity: Arc<dyn gents::NodeIdentity> = identity;
        let agent = gents::Gents::from_default_agent_documents(
            node.clone(),
            runtime_identity,
            gents::DocumentRuntimeOptions {
                tool_ceiling: gents::ToolCeiling::readonly(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let projections = Arc::new(
            ProjectionEngine::new(
                node.clone(),
                super::super::projection::BoundModelContext::new(
                    "grok-control-model".into(),
                    "Grok control model".into(),
                    262_144,
                ),
            )
            .with_background_executions(agent.background_execution_registry()),
        );
        let service = AcpService::new(config, turns, projections);
        let (shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
        let runtime = tokio::spawn(agent.run(shutdown_rx));
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            loop {
                let did = escape_graphql_string(&node_did);
                let response = node.execute(&format!(r#"{{ NodeRuntime(filter: {{node_did: {{_eq: "{did}"}}}}, limit: 2) {{reconcile_phase}} NodeReadiness(filter: {{node_did: {{_eq: "{did}"}}}}, limit: 2) {{node_did snapshot_json updated_at}} }}"#)).await;
                ensure_no_errors(&response, "runtime control readiness").unwrap();
                let data = response.data.as_ref().unwrap();
                let status = data["NodeRuntime"].as_array().unwrap();
                let readiness = data["NodeReadiness"].as_array().unwrap();
                let snapshot = readiness.first().and_then(|row| {
                    let row = serde_json::from_value::<gents_protocol::row::NodeReadinessRow>(row.clone()).ok()?;
                    gents_protocol::row::decode_node_readiness_snapshot(&row, &node_did).ok()
                });
                if status.len() == 1
                    && status[0]["reconcile_phase"] == "idle"
                    && readiness.len() == 1
                    && snapshot.as_ref().is_some_and(|snapshot| {
                        snapshot.process_state.accepts_work()
                            && snapshot.active_generation >= 1
                            && snapshot.agents.iter().any(|agent| {
                                agent.agent_id == agent_id
                                    && agent.state == gents_protocol::row::AgentReadinessState::Ready
                            })
                    })
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("runtime control fixture readiness timeout");
        RuntimeControlFixture {
            _dir: dir,
            service,
            _backend: backend,
            shutdown,
            runtime: Some(runtime),
        }
    }

    async fn wait_for_running_bash_handle(node: &EmbeddedNode, session_id: &str) -> String {
        let session_id = escape_graphql_string(session_id);
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            loop {
                let response = node.execute(&format!(r#"{{ AgentToolCall(filter: {{session_id: {{_eq: "{session_id}"}}, tool_name: {{_eq: "bash"}}, lifecycle_state: {{_eq: "running"}}}}, limit: 2) {{tool_call_id}} }}"#)).await;
                ensure_no_errors(&response, "running bash handle").unwrap();
                let rows = response.data.as_ref().unwrap()["AgentToolCall"].as_array().unwrap();
                if rows.len() == 1 {
                    return rows[0]["tool_call_id"].as_str().unwrap().to_owned();
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }).await.expect("running bash handle timeout")
    }

    async fn wait_for_child_request(
        node: &Arc<EmbeddedNode>,
    ) -> gents_protocol::row::AgentRequestRow {
        let observed = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            loop {
                let response = node.execute(r#"{ AgentRequest {_docID request_id node_did requester_did session_id caused_by_parent_request_id caused_by_parent_request_doc_id caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id} }"#).await;
                ensure_no_errors(&response, "child request").unwrap();
                let rows = response.data.as_ref().unwrap()["AgentRequest"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|row| {
                        row["caused_by_parent_request_doc_id"]
                            .as_str()
                            .is_some_and(|id| !id.trim().is_empty())
                            && row["caused_by_parent_tool_call_doc_id"]
                                .as_str()
                                .is_some_and(|id| !id.trim().is_empty())
                    })
                    .collect::<Vec<_>>();
                assert!(rows.len() <= 1, "ambiguous physical child requests: {rows:?}");
                if rows.len() == 1 {
                    return serde_json::from_value((*rows[0]).clone()).unwrap();
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
        match observed {
            Ok(child) => child,
            Err(_) => {
                let diagnostic = node
                    .execute(r#"{ AgentToolCall(filter: {tool_name: {_eq: "agent_new"}}) {_docID request_id request_doc_id tool_call_id lifecycle_state tool_failure_class await_mode} }"#)
                    .await;
                let presentation = diagnostic
                    .data
                    .as_ref()
                    .and_then(|data| data["AgentToolCall"].as_array())
                    .and_then(|rows| rows.first())
                    .and_then(|row| {
                        Some((row["_docID"].as_str()?, row["request_doc_id"].as_str()?))
                    });
                let presentation = if let Some((tool_doc_id, request_doc_id)) = presentation {
                    let request = node
                        .execute(&format!(
                            r#"{{ AgentRequest(filter: {{_docID: {{_eq: "{}"}}}}, limit: 2) {{node_did requester_did session_id}} }}"#,
                            escape_graphql_string(request_doc_id)
                        ))
                        .await;
                    let scope = request
                        .data
                        .as_ref()
                        .and_then(|data| data["AgentRequest"].as_array())
                        .and_then(|rows| rows.first());
                    if let Some(scope) = scope {
                        gents::tool_call_lifecycle::load_tool_call_presentation(
                            &gents::ConfigAccess::local(node.clone()),
                            tool_doc_id,
                            scope["node_did"].as_str().unwrap_or_default(),
                            scope["session_id"].as_str().unwrap_or_default(),
                            scope["requester_did"].as_str(),
                        )
                        .await
                        .map(|value| format!("{value:?}"))
                        .unwrap_or_else(|error| format!("presentation error: {error:#}"))
                    } else {
                        "parent request scope unavailable".into()
                    }
                } else {
                    "agent_new call unavailable".into()
                };
                panic!(
                    "child request timeout; durable agent_new observations: data={:?} errors={:?}; canonical presentation={presentation}",
                    diagnostic.data, diagnostic.errors,
                );
            }
        }
    }

    async fn exact_request_by_doc_id(
        node: &EmbeddedNode,
        doc_id: &str,
    ) -> gents_protocol::row::AgentRequestRow {
        let doc_id = escape_graphql_string(doc_id);
        let response = node.execute(&format!(r#"{{ AgentRequest(filter: {{_docID: {{_eq: "{doc_id}"}}}}, limit: 2) {{_docID request_id node_did requester_did session_id}} }}"#)).await;
        ensure_no_errors(&response, "physical parent request").unwrap();
        let rows = response.data.as_ref().unwrap()["AgentRequest"]
            .as_array()
            .unwrap();
        assert_eq!(rows.len(), 1, "physical parent request must be exact");
        serde_json::from_value(rows[0].clone()).unwrap()
    }

    async fn test_service_with_graphql(node: Arc<EmbeddedNode>, graphql: String) -> AcpService {
        let identity_dir = tempfile::tempdir().expect("identity tempdir");
        let identity =
            gents::KeyIdentity::load_or_create(identity_dir.path().join("agent.key"), None)
                .expect("test signing identity");
        let node_did = gents::NodeIdentity::did(&identity).to_string();
        let actor = ::identity::Did::new(node_did.clone()).expect("fixture creator DID");
        let agent_id = gents::default_agent_id_for_node(&node_did);
        let config = AcpServiceConfig {
            node,
            node_did: Arc::from(node_did.as_str()),
            agent_id: Arc::from(agent_id.as_str()),
            current_model: bound_model(),
            grok_home: None,
            plugin_home: std::path::PathBuf::new(),
        };
        gents::schema::ensure_runtime_schemas(config.node.as_ref())
            .await
            .expect("runtime schemas");
        super::super::seed_test_agent_configuration(
            &config.node,
            &node_did,
            &agent_id,
            &agent_id,
            &config.current_model.model_id,
            true,
        )
        .await;
        let turns = Arc::new(TurnManager::new(
            config.node.clone(),
            super::super::turn::TurnManagerConfig {
                actor,
                node_did: config.node_did.to_string(),
                agent_id: config.agent_id.to_string(),
                graphql,
            },
        ));
        let projections = Arc::new(ProjectionEngine::new(
            config.node.clone(),
            super::super::projection::BoundModelContext::new(
                config.current_model.model_id.clone(),
                config.current_model.name.clone(),
                config.current_model.total_context_tokens,
            ),
        ));
        AcpService::new(config, turns, projections)
    }

    async fn spawn_mock_graphql(node: Arc<EmbeddedNode>) -> String {
        async fn handler(
            axum::extract::State(node): axum::extract::State<Arc<EmbeddedNode>>,
            axum::Json(body): axum::Json<Value>,
        ) -> axum::Json<Value> {
            let query = body
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let response = node.execute(&query).await;
            axum::Json(serde_json::to_value(&response).unwrap_or_default())
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock graphql listener");
        let addr = listener
            .local_addr()
            .expect("mock graphql listener address");
        let router = axum::Router::new()
            .route("/", axum::routing::post(handler))
            .with_state(node);
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        format!("http://{addr}/")
    }

    #[tokio::test]
    async fn delegate_streams_updates_live_before_the_response() {
        let (_staging, config) = config().await;
        gents::schema::ensure_runtime_schemas(config.node.as_ref())
            .await
            .expect("runtime schemas");
        let graphql = spawn_mock_graphql(config.node.clone()).await;
        let node = config.node.clone();
        let service = test_service_with_graphql(node, graphql).await;
        let node_did = service.config.node_did.to_string();

        service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({ "cwd": "/tmp", "mcpServers": [], "_meta": { "sessionId": "s-live" } }),
            ))
            .await;

        let payload = request_payload(
            "session/prompt",
            json!({
                "sessionId": "s-live",
                "prompt": [{"type": "text", "text": "hello"}],
                "_meta": {"promptId": "prompt-live"},
            }),
        );

        let (frames_tx, mut frames_rx) = tokio::sync::mpsc::unbounded_channel::<
            crate::commands::grok_shim::protocol::ServerEnvelope,
        >();
        let outbound = AcpOutbound::for_frames(frames_tx);

        let node_for_seed = config.node.clone();
        let seed_handle = tokio::spawn(async move {
            loop {
                let query = r#"{ AgentRequest(filter: { lifecycle_state: { _eq: "pending" } }, limit: 2) { _docID request_id node_did requester_did session_id } }"#;
                let response = node_for_seed.execute(query).await;
                let rows = response
                    .data
                    .as_ref()
                    .and_then(|data| data.get("AgentRequest"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if let Some(row) = rows.first() {
                    assert_eq!(rows.len(), 1, "live request identity must be exact");
                    let request: gents_protocol::row::AgentRequestRow =
                        serde_json::from_value(row.clone()).unwrap();
                    let escaped_request =
                        gents::graphql::escape_graphql_string(&request.request_id);
                    let message_key = gents::session::sequence_message_key(
                        &node_did,
                        "s-live",
                        Some(&node_did),
                        1,
                    );
                    let message_doc_id = seed_canonical_assistant_message(
                        node_for_seed.as_ref(),
                        &request,
                        &message_key,
                        1,
                        "",
                        "live answer",
                    )
                    .await;
                    let terminal_output = gents_protocol::graphql::graphql_input_literal(
                        &serde_json::to_value(gents_protocol::output::TerminalOutput::Message {
                            message_doc_id,
                        })
                        .unwrap(),
                    )
                    .unwrap();
                    let mutation = format!(
                        r#"mutation {{
                            update_AgentRequest(
                                filter: {{ request_id: {{ _eq: "{escaped_request}" }} }},
                                input: {{ lifecycle_state: "completed", terminal_output: {terminal_output} }}
                            ) {{ _docID }}
                        }}"#
                    );
                    let response = node_for_seed.execute(&mutation).await;
                    gents::graphql::ensure_no_errors(&response, "terminalize")
                        .expect("terminalize");
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        });

        let service = Arc::new(service);
        let delegate_service = service.clone();
        let handle = tokio::spawn(async move {
            delegate_service
                .handle_acp(&payload, outbound)
                .await
                .expect("delegate should succeed")
        });

        let mut frames: Vec<Value> = Vec::new();
        let response_seen = loop {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(30), frames_rx.recv())
                .await
                .expect("frame should arrive within timeout")
                .expect("outbound stays open until the delegate resolves");
            let crate::commands::grok_shim::protocol::ServerEnvelope::Acp { payload } = frame
            else {
                continue;
            };
            let value: Value = serde_json::from_str(&payload).expect("acp payload is JSON");
            if value.get("id").is_some() {
                frames.push(value);
                break true;
            }
            frames.push(value);
        };
        assert!(response_seen, "the deferred response must arrive");
        handle.await.expect("delegate task");
        seed_handle.await.expect("seed task");

        let kinds: Vec<String> = frames
            .iter()
            .filter_map(|frame| {
                frame["params"]["update"]["sessionUpdate"]
                    .as_str()
                    .map(ToOwned::to_owned)
            })
            .collect();
        let echo_index = kinds
            .iter()
            .position(|kind| kind == "user_message_chunk")
            .expect("the user echo must stream live");
        let answer_index = kinds
            .iter()
            .position(|kind| kind == "agent_message_chunk")
            .expect("the assistant answer must stream live");
        assert!(
            echo_index < answer_index,
            "the echo must precede the answer, got {kinds:?}"
        );
        let last = frames.last().expect("response frame");
        assert_eq!(last["id"], 1, "the deferred response closes the stream");
        assert_eq!(last["result"]["stopReason"], "end_turn");
        for frame in &frames[..frames.len() - 1] {
            assert_eq!(frame["params"]["_meta"]["promptId"], "prompt-live");
        }
    }

    #[test]
    fn extension_methods_use_sdk_wire_prefix_and_normalize_at_dispatch() {
        for method in [
            MCP_INITIALIZED_METHOD,
            MODELS_UPDATE_METHOD,
            "x.ai/session_notification",
            SUBAGENT_GET_METHOD,
        ] {
            let encoded = wire_method(method);
            assert_eq!(encoded, format!("_{method}"));
            assert_eq!(
                wire_method(&encoded),
                encoded,
                "encoding must not double-prefix"
            );
            let request = AcpRequest::from_payload(
                &json!({
                    "jsonrpc": "2.0", "id": 1, "method": encoded, "params": {},
                })
                .to_string(),
            )
            .unwrap();
            assert_eq!(request.method, method);
        }
        for method in [
            SESSION_PROMPT_METHOD,
            "_unknown/extension",
            "__x.ai/subagent/get",
        ] {
            assert_eq!(wire_method(method), method);
            let request = AcpRequest::from_payload(
                &json!({
                    "jsonrpc": "2.0", "id": 1, "method": method,
                })
                .to_string(),
            )
            .unwrap();
            assert_eq!(
                request.method, method,
                "only the exact _x.ai/ namespace is normalized"
            );
        }
    }

    #[test]
    fn shaped_stub_messages_name_the_owned_transition() {
        let interject = shaped_stub_error(
            INTERJECT_METHOD,
            &json!({ "sessionId": "s1", "text": "hi", "interjectionId": "i1" }),
        );
        let message = interject.to_string();
        assert!(
            message.contains("no formally specified injection transition"),
            "interject stub must explain the formal-transition gap: {message}"
        );
        assert!(
            message.contains("No AgentMessage or AgentRequest document was fabricated"),
            "interject stub must state that nothing was fabricated: {message}"
        );

        let compact = shaped_stub_error(COMPACT_CONVERSATION_METHOD, &json!({ "sessionId": "s1" }));
        let message = compact.to_string();
        assert!(
            message.contains("tokens_before") && message.contains("tokens_after"),
            "compact stub must name the invented fields it refuses: {message}"
        );
        assert!(
            message.contains("no usage counters"),
            "compact stub must name the missing AgentSession counters: {message}"
        );
    }

    #[test]
    fn shaped_stub_maps_to_method_not_found_code() {
        let error = shaped_stub_error(SESSION_LOAD_METHOD, &json!({ "sessionId": "s1" }));
        assert_eq!(error_code_for(&error), -32601);
        let unknown = anyhow::Error::new(ShapedMethodNotFound {
            message: "unsupported".to_string(),
        });
        assert_eq!(error_code_for(&unknown), -32601);
    }

    #[test]
    fn internal_and_param_errors_map_to_their_codes() {
        let missing_session = required_session_id(&json!({})).expect_err("missing session id");
        assert_eq!(error_code_for(&missing_session), -32602);
        let internal = anyhow::anyhow!("boom");
        assert_eq!(error_code_for(&internal), -32603);
        let internal_looking =
            anyhow::anyhow!("storage failed while reading session requires a non-empty sessionId");
        assert_eq!(error_code_for(&internal_looking), -32603);
        let typed = invalid_params("any message");
        assert_eq!(error_code_for(&typed), -32602);
        let parser_error =
            super::super::turn::parse_prompt_request(&json!({}), None).expect_err("missing params");
        assert!(
            parser_error.to_string().contains("session/prompt"),
            "the source parser message is preserved: {parser_error}"
        );
        let wrapped = invalid_params(parser_error.to_string());
        assert_eq!(error_code_for(&wrapped), -32602);
        assert_eq!(
            wrapped.to_string(),
            parser_error.to_string(),
            "the wrapped error message matches the source verbatim"
        );
    }

    #[tokio::test]
    async fn initialize_advertises_persisted_session_loading_and_gents_auth() {
        let (_staging, service) = test_service().await;
        let dispatch = service
            .handle_acp_payload(&request_payload(
                "initialize",
                json!({ "protocolVersion": 1, "clientInfo": { "name": "probe" } }),
            ))
            .await;
        assert!(dispatch.notifications.is_empty());
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["agentCapabilities"]["loadSession"], true);
        assert_eq!(response["result"]["authMethods"][0]["id"], "gents.runtime");
        assert_eq!(
            response["result"]["authMethods"][0]["name"],
            "Gents runtime"
        );
    }

    #[tokio::test]
    async fn local_billing_has_no_cloud_balance_or_subscription() {
        let (_staging, service) = test_service().await;
        let dispatch = service
            .handle_acp_payload(&request_payload("x.ai/billing", json!({})))
            .await;
        assert!(dispatch.notifications.is_empty());
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(
            response["result"]["result"],
            json!({"config":null,"on_demand_enabled":false})
        );
    }

    #[tokio::test]
    async fn workflows_list_is_routed() {
        let (_staging, service) = service_over(true).await;
        let config = &service.config;
        super::super::seed_test_agent_configuration(
            &config.node,
            &config.node_did,
            &config.agent_id,
            &config.agent_id,
            "grok-workflows-model",
            true,
        )
        .await;
        let dispatch = service
            .handle_acp_payload(&request_payload("x.ai/workflows/list", json!({})))
            .await;
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(response["result"], json!({"result": {"workflows": []}}));
    }

    #[tokio::test]
    async fn workflows_list_failure_is_a_shaped_internal_error() {
        let (_staging, service) = test_service().await;
        let dispatch = service
            .handle_acp_payload(&request_payload("x.ai/workflows/list", json!({})))
            .await;
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert_eq!(response["error"]["code"], -32603, "{response}");
        assert!(!response["error"]["message"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn authenticate_accepts_gents_runtime_and_rejects_others() {
        let (_staging, service) = test_service().await;
        let ok = service
            .handle_acp_payload(&request_payload(
                "authenticate",
                json!({ "methodId": "gents.runtime" }),
            ))
            .await;
        let response = parse_response(ok.response.as_deref().expect("response line"));
        assert_eq!(response["result"]["_meta"]["provider"], "gents");

        let rejected = service
            .handle_acp_payload(&request_payload(
                "authenticate",
                json!({ "methodId": "oauth" }),
            ))
            .await;
        let response = parse_response(rejected.response.as_deref().expect("response line"));
        assert_eq!(
            response["error"]["code"], -32602,
            "a caller-supplied auth method outside the advertised list is invalid params"
        );
        assert!(
            response["error"]["message"]
                .as_str()
                .expect("message")
                .contains("gents.runtime"),
            "rejection must name the only supported method"
        );
    }

    #[tokio::test]
    async fn session_new_honors_preferred_id_and_nests_models() {
        let (_staging, service) = test_service().await;
        let dispatch = service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({
                    "cwd": "/tmp",
                    "mcpServers": [],
                    "_meta": {
                        "sessionId": "grok-edge-preferred",
                        "modelId": "GLM-5.3-NVFP4",
                        "yoloMode": true,
                        "autoMode": false,
                    },
                }),
            ))
            .await;
        assert_eq!(dispatch.notifications.len(), 1);
        let mcp_initialized = parse_response(&dispatch.notifications[0]);
        assert_eq!(
            mcp_initialized,
            json!({
                "jsonrpc": "2.0",
                "method": "_x.ai/mcp_initialized",
                "params": {
                    "sessionId": "grok-edge-preferred",
                    "mcpToolCount": 0,
                    "elapsedMs": 0,
                },
            }),
            "session/new must emit the stock pager's exact MCP completion envelope"
        );
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        let result = &response["result"];
        assert_eq!(result["sessionId"], "grok-edge-preferred");
        assert_eq!(
            result["models"]["currentModelId"], "GLM-5.3-NVFP4",
            "session/new must return models.currentModelId, not a top-level key"
        );
        assert_eq!(
            result["models"]["availableModels"][0]["modelId"],
            "GLM-5.3-NVFP4"
        );
        assert_eq!(
            result["models"]["availableModels"][0]["meta"]["totalContextTokens"],
            262_144
        );
        assert!(result.get("availableModels").is_none());
        assert!(result.get("currentModelId").is_none());
        assert_eq!(result["_meta"]["yoloMode"], true);
        assert_eq!(result["_meta"]["autoMode"], false);
        assert_eq!(result["_meta"]["modelId"], "GLM-5.3-NVFP4");
    }

    #[tokio::test]
    async fn session_new_derives_mode_capabilities_from_registration() {
        let (_staging, mut service) = test_service().await;
        service.register_client_identity(
            7,
            &crate::commands::grok_shim::server::Registration {
                client_type: "grok-pager".to_string(),
                mode: crate::commands::grok_shim::protocol::RegisterMode::Stdio,
                capabilities: crate::commands::grok_shim::protocol::ClientCapabilities {
                    yolo_mode: true,
                    auto_mode: false,
                    terminal: false,
                    ..Default::default()
                },
            },
        );

        let dispatch = service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({
                    "cwd": "/tmp",
                    "mcpServers": [],
                    "_meta": { "sessionId": "grok-registered-session" },
                }),
            ))
            .await;
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        let result = &response["result"];
        assert_eq!(result["sessionId"], "grok-registered-session");
        assert_eq!(
            result["_meta"]["yoloMode"], true,
            "yoloMode must come from the registered capability, not the request"
        );
        assert_eq!(result["_meta"]["autoMode"], false);
        assert_eq!(
            result["_meta"]["clientTerminal"], false,
            "the reference pager registers terminal=false; the key must be present"
        );
    }

    #[tokio::test]
    async fn session_new_meta_overrides_registration_but_defaults_from_it() {
        let (_staging, mut service) = test_service().await;
        service.register_client_identity(
            9,
            &crate::commands::grok_shim::server::Registration {
                client_type: "grok-pager".to_string(),
                mode: crate::commands::grok_shim::protocol::RegisterMode::Stdio,
                capabilities: crate::commands::grok_shim::protocol::ClientCapabilities {
                    yolo_mode: false,
                    auto_mode: true,
                    terminal: true,
                    ..Default::default()
                },
            },
        );
        let dispatch = service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({
                    "cwd": "/tmp",
                    "_meta": {
                        "sessionId": "grok-override-session",
                        "yoloMode": true,
                    },
                }),
            ))
            .await;
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        let result = &response["result"];
        assert_eq!(
            result["_meta"]["yoloMode"], true,
            "the explicit request override must win"
        );
        assert_eq!(
            result["_meta"]["autoMode"], true,
            "an absent request key falls back to the registered capability"
        );
        assert_eq!(
            result["_meta"]["clientTerminal"], true,
            "clientTerminal must reflect the registered terminal capability"
        );
    }

    #[tokio::test]
    async fn session_new_creates_exactly_one_session_and_zero_requests() {
        let (_staging, service) = test_service().await;
        service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({
                    "cwd": "/tmp",
                    "mcpServers": [],
                    "_meta": { "sessionId": "grok-edge-docs" },
                }),
            ))
            .await;

        let node = service.config.node.clone();
        let query = r#"{
            AgentSession(filter: { session_id: { _eq: "grok-edge-docs" } }) {
                session_id agent_id node_did requester_did created_at closed_at
            }
            AgentRequest(filter: { session_id: { _eq: "grok-edge-docs" } }) { request_id }
        }"#
        .to_string();
        let response = node.execute(&query).await;
        ensure_no_errors(&response, "session document check").expect("query ok");
        let sessions = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentSession"))
            .and_then(Value::as_array)
            .expect("AgentSession array");
        assert_eq!(sessions.len(), 1, "exactly one AgentSession document");
        assert_eq!(sessions[0]["agent_id"], "did:test:grok-shim:default");
        assert_eq!(sessions[0]["node_did"], "did:test:grok-shim");
        assert_eq!(sessions[0]["requester_did"], "did:test:grok-shim");
        assert!(sessions[0]["created_at"].as_str().is_some());
        assert!(sessions[0]["closed_at"].is_null());
        let requests = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .expect("AgentRequest array");
        assert!(
            requests.is_empty(),
            "session/new must not fabricate AgentRequest rows"
        );
    }

    #[tokio::test]
    async fn the_fixture_staging_tempdir_owns_and_cleans_up_the_node_storage() {
        let (staging, service) = test_service().await;
        let staging_root = staging.path().to_path_buf();
        let node_data = staging_root.join("node");
        assert!(
            node_data.exists(),
            "the node's storage directory must exist while the staging guard is held"
        );

        service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({
                    "cwd": "/tmp",
                    "mcpServers": [],
                    "_meta": { "sessionId": "s-staging-cleanup" },
                }),
            ))
            .await;
        drop(service);
        drop(staging);
        assert!(
            !node_data.exists(),
            "dropping the staging guard must delete the node's storage directory"
        );
        assert!(
            !staging_root.exists(),
            "the whole staging directory must be cleaned up, not abandoned"
        );
    }

    #[tokio::test]
    async fn session_new_writes_the_grok_record_the_resume_picker_needs() {
        let (_staging, mut service) = test_service().await;
        let home = tempfile::tempdir().expect("grok home");
        service.config.grok_home = Some(home.path().to_path_buf());
        let dispatch = service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({
                    "cwd": "/Users/edjroz/Repos/gents",
                    "mcpServers": [],
                    "_meta": { "sessionId": "grok-resume-record" },
                }),
            ))
            .await;
        let response = parse_response(dispatch.response.as_deref().expect("response"));
        assert_eq!(response["result"]["sessionId"], "grok-resume-record");

        let path = home
            .path()
            .join("sessions/%2FUsers%2Fedjroz%2FRepos%2Fgents/grok-resume-record/summary.json");
        let record: Value = serde_json::from_slice(
            &std::fs::read(&path).expect("session/new writes the pager's session record"),
        )
        .expect("record is json");
        assert_eq!(record["info"]["id"], "grok-resume-record");
        assert_eq!(record["info"]["cwd"], "/Users/edjroz/Repos/gents");
        assert_eq!(record["chat_format_version"], 1);
    }

    #[tokio::test]
    async fn session_new_is_idempotent_for_the_same_session_id() {
        let (_staging, service) = test_service().await;
        let params = json!({
            "cwd": "/tmp",
            "mcpServers": [],
            "_meta": { "sessionId": "grok-edge-idempotent" },
        });
        let first = service
            .handle_acp_payload(&request_payload("session/new", params.clone()))
            .await;
        let second = service
            .handle_acp_payload(&request_payload("session/new", params))
            .await;
        let first = parse_response(first.response.as_deref().expect("response"));
        let second = parse_response(second.response.as_deref().expect("response"));
        assert_eq!(first["result"]["sessionId"], second["result"]["sessionId"]);

        let node = service.config.node.clone();
        let query = r#"{
            AgentSession(filter: { session_id: { _eq: "grok-edge-idempotent" } }) { session_id }
        }"#
        .to_string();
        let response = node.execute(&query).await;
        ensure_no_errors(&response, "idempotency check").expect("query ok");
        let sessions = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentSession"))
            .and_then(Value::as_array)
            .expect("AgentSession array");
        assert_eq!(
            sessions.len(),
            1,
            "repeat session/new must not duplicate rows"
        );
    }

    #[tokio::test]
    async fn session_new_rejects_an_existing_session_bound_to_a_different_agent() {
        let (_staging, service) = test_service().await;
        let node = service.config.node.clone();
        let seed = r#"mutation {
            create_AgentSession(input: {
                session_id: "grok-edge-foreign-agent",
                node_did: "did:test:foreign-agent",
                requester_did: "did:test:foreign-agent",
                agent_id: "did:test:grok-shim:default",
                created_at: "2026-08-31T22:46:45Z",
                closed_at: "2026-08-31T22:46:46Z"
            }) { _docID }
        }"#
        .to_string();
        let response = node.execute(&seed).await;
        ensure_no_errors(&response, "foreign session seed").expect("seed ok");

        let dispatch = service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({
                    "cwd": "/tmp",
                    "mcpServers": [],
                    "_meta": { "sessionId": "grok-edge-foreign-agent" },
                }),
            ))
            .await;
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert_eq!(response["error"]["code"], -32603);
        let message = response["error"]["message"].as_str().expect("message");
        assert!(
            message.contains("did:test:foreign-agent"),
            "the mismatch must name the session's immutable agent: {message}"
        );
        assert!(
            message.contains("node_did"),
            "the mismatch must name the immutable field: {message}"
        );

        let query = r#"{
            AgentSession(filter: { session_id: { _eq: "grok-edge-foreign-agent" } }) {
                node_did closed_at
            }
        }"#
        .to_string();
        let response = node.execute(&query).await;
        ensure_no_errors(&response, "foreign session check").expect("query ok");
        let sessions = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentSession"))
            .and_then(Value::as_array)
            .expect("AgentSession array");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["node_did"], "did:test:foreign-agent");
        assert_eq!(sessions[0]["closed_at"], "2026-08-31T22:46:46Z");
    }

    #[tokio::test]
    async fn session_new_escapes_session_ids_in_queries() {
        let (_staging, service) = test_service().await;
        let hostile = r#"grok"\<script>-id"#;
        let dispatch = service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({ "cwd": "/tmp", "mcpServers": [], "_meta": { "sessionId": hostile } }),
            ))
            .await;
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert_eq!(response["result"]["sessionId"], hostile);

        let node = service.config.node.clone();
        let query = format!(
            r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ session_id }} }}"#,
            escape_graphql_string(hostile)
        );
        let response = node.execute(&query).await;
        ensure_no_errors(&response, "hostile id check").expect("query ok");
        let sessions = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentSession"))
            .and_then(Value::as_array)
            .expect("AgentSession array");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["session_id"], hostile);
    }

    #[tokio::test]
    async fn session_new_rejects_model_outside_bound_catalog() {
        let (_staging, service) = test_service().await;
        let dispatch = service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({
                    "cwd": "/tmp",
                    "mcpServers": [],
                    "_meta": { "sessionId": "s-model", "modelId": "gpt-9" },
                }),
            ))
            .await;
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert_eq!(
            response["error"]["code"], -32602,
            "a requested model outside the bound catalog is invalid params"
        );
        assert!(
            response["error"]["message"]
                .as_str()
                .expect("message")
                .contains("bound catalog"),
            "rejection must name the bound catalog"
        );
        assert!(
            dispatch.notifications.is_empty(),
            "a rejected session/new must not clear the pager's MCP spinner"
        );
    }

    #[tokio::test]
    async fn set_model_emits_models_update_and_rejects_unknown_model() {
        let (_staging, service) = test_service().await;
        service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({ "cwd": "/tmp", "mcpServers": [], "_meta": { "sessionId": "s-set-model" } }),
            ))
            .await;

        let rejected = service
            .handle_acp_payload(&request_payload(
                "session/set_model",
                json!({ "sessionId": "s-set-model", "modelId": "nope" }),
            ))
            .await;
        let response = parse_response(rejected.response.as_deref().expect("response line"));
        assert_eq!(
            response["error"]["code"], -32602,
            "a model outside the bound catalog is a client invalid-params error"
        );
        assert!(
            response["error"]["message"]
                .as_str()
                .expect("message")
                .contains("bound catalog"),
            "rejection must name the bound catalog"
        );

        let dispatch = service
            .handle_acp_payload(&request_payload(
                "session/set_model",
                json!({
                    "sessionId": "s-set-model",
                    "modelId": "GLM-5.3-NVFP4",
                }),
            ))
            .await;
        assert_eq!(dispatch.notifications.len(), 1);
        let notification = parse_response(&dispatch.notifications[0]);
        assert_eq!(notification["method"], "_x.ai/models/update");
        assert_eq!(notification["params"]["currentModelId"], "GLM-5.3-NVFP4");
        assert!(notification["params"].get("models").is_none());
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert_eq!(response["result"], json!({}), "set_model result is empty");
    }

    #[tokio::test]
    async fn set_model_rejects_a_reasoning_effort_as_invalid_params() {
        let (_staging, service) = test_service().await;
        service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({ "cwd": "/tmp", "mcpServers": [], "_meta": { "sessionId": "s-effort" } }),
            ))
            .await;
        for effort in ["high", "extreme", "none", "low"] {
            let dispatch = service
                .handle_acp_payload(&request_payload(
                    "session/set_model",
                    json!({
                        "sessionId": "s-effort",
                        "modelId": "GLM-5.3-NVFP4",
                        "_meta": { "reasoningEffort": effort },
                    }),
                ))
                .await;
            let response = parse_response(dispatch.response.as_deref().expect("response line"));
            assert_eq!(
                response["error"]["code"], -32602,
                "reasoningEffort {effort:?} must be rejected as invalid params: the \
                     feature is unadvertised and unimplemented"
            );
            assert!(
                response["error"]["message"]
                    .as_str()
                    .expect("message")
                    .contains("reasoningEffort"),
                "the rejection must name the unsupported key: {effort}"
            );
            assert!(
                dispatch.notifications.is_empty(),
                "a rejected set_model must not emit a catalog update"
            );
        }
    }

    #[tokio::test]
    async fn set_mode_emits_current_mode_update() {
        let (_staging, service) = test_service().await;
        service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({ "cwd": "/tmp", "mcpServers": [], "_meta": { "sessionId": "s-set-mode" } }),
            ))
            .await;

        let dispatch = service
            .handle_acp_payload(&request_payload(
                "session/set_mode",
                json!({ "sessionId": "s-set-mode", "modeId": "yolo" }),
            ))
            .await;
        assert_eq!(dispatch.notifications.len(), 1);
        let notification = parse_response(&dispatch.notifications[0]);
        assert_eq!(notification["method"], "session/update");
        assert_eq!(
            notification["params"]["update"]["sessionUpdate"],
            "current_mode_update"
        );
        assert_eq!(notification["params"]["update"]["currentModeId"], "yolo");
        assert_eq!(notification["params"]["sessionId"], "s-set-mode");
        assert_eq!(
            notification["params"]["_meta"]["eventId"], "s-set-mode-1",
            "the MCP completion extension must not consume a session-update event id"
        );
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert_eq!(response["result"], json!({}), "set_mode result is empty");
    }

    #[tokio::test]
    async fn event_ids_are_monotonic_and_session_scoped_through_one_sequencer() {
        let (_staging, service) = test_service().await;
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let sender = PromptSender::Buffer {
            buffer: buffer.clone(),
        };
        for _ in 0..3 {
            sender
                .send_user_message_chunk(
                    service.projections.session_updates(),
                    "s-events",
                    "prompt-1",
                    &PromptBlock {
                        kind: "text".to_string(),
                        text: "hello".to_string(),
                        meta: None,
                    },
                    0,
                )
                .await
                .expect("echo send");
        }
        let lines = buffer.lock().await;
        assert_eq!(lines.len(), 3, "three sends enqueue three lines");
        let event_ids: Vec<String> = lines
            .iter()
            .map(|line| {
                parse_response(line)["params"]["_meta"]["eventId"]
                    .as_str()
                    .expect("eventId")
                    .to_string()
            })
            .collect();
        assert_eq!(event_ids, vec!["s-events-1", "s-events-2", "s-events-3"]);

        let other_lines = {
            let other_buffer = Arc::new(Mutex::new(Vec::new()));
            let other_sender = PromptSender::Buffer {
                buffer: other_buffer.clone(),
            };
            other_sender
                .send_user_message_chunk(
                    service.projections.session_updates(),
                    "s-other",
                    "prompt-1",
                    &PromptBlock {
                        kind: "text".to_string(),
                        text: "hello".to_string(),
                        meta: None,
                    },
                    0,
                )
                .await
                .expect("echo send");
            let lines = other_buffer.lock().await;
            lines
                .iter()
                .map(|line| {
                    parse_response(line)["params"]["_meta"]["eventId"]
                        .as_str()
                        .expect("eventId")
                        .to_string()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(other_lines, vec!["s-other-1"]);
        assert_eq!(
            service
                .projections
                .sequencer_arc()
                .event_counter("s-events"),
            3
        );
        assert_eq!(
            service.projections.sequencer_arc().event_counter("s-other"),
            1
        );
    }

    #[tokio::test]
    async fn set_mode_and_prompt_never_duplicate_an_event_id() {
        let (_staging, service) = test_service().await;
        service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({ "cwd": "/tmp", "mcpServers": [], "_meta": { "sessionId": "s-dup" } }),
            ))
            .await;
        let mode_dispatch = service
            .handle_acp_payload(&request_payload(
                "session/set_mode",
                json!({ "sessionId": "s-dup", "modeId": "yolo" }),
            ))
            .await;
        let mode_notification = parse_response(&mode_dispatch.notifications[0]);
        let mode_event_id = mode_notification["params"]["_meta"]["eventId"]
            .as_str()
            .expect("mode eventId")
            .to_string();

        let buffer = Arc::new(Mutex::new(Vec::new()));
        let sender = PromptSender::Buffer {
            buffer: buffer.clone(),
        };
        sender
            .send_user_message_chunk(
                service.projections.session_updates(),
                "s-dup",
                "prompt-1",
                &crate::commands::grok_shim::turn::PromptBlock {
                    kind: "text".to_string(),
                    text: "hello".to_string(),
                    meta: None,
                },
                0,
            )
            .await
            .expect("echo send");
        let lines = buffer.lock().await;
        let echo: Value = serde_json::from_str(&lines[0]).expect("echo line");
        let echo_event_id = echo["params"]["_meta"]["eventId"]
            .as_str()
            .expect("echo eventId")
            .to_string();

        assert_ne!(mode_event_id, echo_event_id);
        assert_eq!(mode_event_id, "s-dup-1");
        assert_eq!(echo_event_id, "s-dup-2");
    }

    #[tokio::test]
    async fn session_new_streams_exact_mcp_completion_after_the_live_response() {
        let (_staging, service) = test_service().await;
        let payload = request_payload(
            "session/new",
            json!({
                "cwd": "/tmp",
                "mcpServers": [],
                "_meta": { "sessionId": "s-mcp-live" },
            }),
        );
        let (frames_tx, mut frames_rx) = tokio::sync::mpsc::unbounded_channel::<
            crate::commands::grok_shim::protocol::ServerEnvelope,
        >();

        service
            .handle_acp(&payload, AcpOutbound::for_frames(frames_tx))
            .await
            .expect("session/new dispatch");

        let first = frames_rx.recv().await.expect("session/new response frame");
        let crate::commands::grok_shim::protocol::ServerEnvelope::Acp { payload } = first else {
            panic!("session/new emitted a non-ACP frame first");
        };
        let response = parse_response(&payload);
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["sessionId"], "s-mcp-live");

        let second = frames_rx.recv().await.expect("MCP completion frame");
        let crate::commands::grok_shim::protocol::ServerEnvelope::Acp { payload } = second else {
            panic!("session/new emitted a non-ACP completion frame");
        };
        assert_eq!(
            parse_response(&payload),
            json!({
                "jsonrpc": "2.0",
                "method": "_x.ai/mcp_initialized",
                "params": {
                    "sessionId": "s-mcp-live",
                    "mcpToolCount": 0,
                    "elapsedMs": 0,
                },
            })
        );

        assert!(
            frames_rx.try_recv().is_err(),
            "session/new must emit exactly one notification and one response"
        );
    }

    #[tokio::test]
    async fn set_mode_streams_its_notification_before_the_response_on_the_live_path() {
        let (_staging, service) = test_service().await;
        service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({ "cwd": "/tmp", "mcpServers": [], "_meta": { "sessionId": "s-mode-live" } }),
            ))
            .await;

        let payload = request_payload(
            "session/set_mode",
            json!({ "sessionId": "s-mode-live", "modeId": "yolo" }),
        );

        let (frames_tx, mut frames_rx) = tokio::sync::mpsc::unbounded_channel::<
            crate::commands::grok_shim::protocol::ServerEnvelope,
        >();
        let outbound = AcpOutbound::for_frames(frames_tx);
        service
            .handle_acp(&payload, outbound)
            .await
            .expect("set_mode dispatch");

        let mut saw_mode_update = false;
        let mut saw_response = false;
        while let Ok(frame) =
            tokio::time::timeout(std::time::Duration::from_secs(10), frames_rx.recv()).await
        {
            let Some(crate::commands::grok_shim::protocol::ServerEnvelope::Acp { payload }) = frame
            else {
                continue;
            };
            let value: Value = serde_json::from_str(&payload).expect("acp payload is JSON");
            if value.get("id").is_some() {
                saw_response = true;
                break;
            }
            assert!(!saw_response, "a notification arrived after the response");
            assert_eq!(
                value["params"]["update"]["sessionUpdate"], "current_mode_update",
                "the streamed notification is the mode update"
            );
            assert_eq!(
                value["params"]["_meta"]["eventId"], "s-mode-live-1",
                "the mode update consumes the session's first event id"
            );
            saw_mode_update = true;
        }
        assert!(saw_mode_update, "the mode update streamed live");
        assert!(saw_response, "the set_mode response arrived");
        assert!(
            service
                .projections
                .sequencer_arc()
                .event_counter("s-mode-live")
                == 1,
            "exactly one event id was consumed"
        );
    }

    #[tokio::test]
    async fn set_mode_over_a_closed_live_outbound_fails_without_committing_mode_or_event_id() {
        let (_staging, service) = test_service().await;
        service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({
                    "cwd": "/tmp",
                    "mcpServers": [],
                    "_meta": { "sessionId": "s-mode-closed" },
                }),
            ))
            .await;

        let payload = request_payload(
            "session/set_mode",
            json!({ "sessionId": "s-mode-closed", "modeId": "yolo" }),
        );

        let (frames_tx, frames_rx) = tokio::sync::mpsc::unbounded_channel::<
            crate::commands::grok_shim::protocol::ServerEnvelope,
        >();
        let outbound = AcpOutbound::for_frames(frames_tx);
        drop(frames_rx);

        let dispatch_result = service.handle_acp(&payload, outbound).await;
        let send_error =
            dispatch_result.expect_err("the closed outbound must fail the set_mode dispatch");
        assert!(
            send_error.to_string().contains("connection is closed"),
            "the dispatch must surface the closed-connection send error: {send_error}"
        );

        let sessions = service.sessions.lock().await;
        let state = sessions
            .get("s-mode-closed")
            .expect("the session state must exist after session/new");
        assert_eq!(
            state.mode_id, "default",
            "a failed live send must leave the recorded mode untouched"
        );
        drop(sessions);

        assert_eq!(
            service
                .projections
                .sequencer_arc()
                .event_counter("s-mode-closed"),
            0,
            "a failed live send must consume no event id"
        );

        let buffer = Arc::new(Mutex::new(Vec::new()));
        let sender = PromptSender::Buffer {
            buffer: buffer.clone(),
        };
        sender
            .send_user_message_chunk(
                service.projections.session_updates(),
                "s-mode-closed",
                "prompt-1",
                &PromptBlock {
                    kind: "text".to_string(),
                    text: "hello".to_string(),
                    meta: None,
                },
                0,
            )
            .await
            .expect("the recovery send must succeed");
        let lines = buffer.lock().await;
        let echo: Value = serde_json::from_str(&lines[0]).expect("echo line");
        assert_eq!(
            echo["params"]["_meta"]["eventId"], "s-mode-closed-1",
            "the next successful send must receive the session's first id"
        );
    }
    #[tokio::test]
    async fn two_sessions_dispatch_through_the_production_entry_with_independent_event_ids() {
        let (_staging, service) = test_service().await;
        let service = Arc::new(service);
        for session in ["s-concurrent-a", "s-concurrent-b"] {
            service . handle_acp_payload (& request_payload ("session/new" , json ! ({ "cwd" : "/tmp" , "mcpServers" : [] , "_meta" : { "sessionId" : session } }) ,)) . await ;
        }
        let service_a = service.clone();
        let service_b = service.clone();
        let dispatch_a = tokio::spawn(async move {
            service_a
                .handle_acp_payload(&request_payload(
                    "session/set_mode",
                    json ! ({ "sessionId" : "s-concurrent-a" , "modeId" : "yolo" }),
                ))
                .await
        });
        let dispatch_b = tokio::spawn(async move {
            service_b
                .handle_acp_payload(&request_payload(
                    "session/set_mode",
                    json ! ({ "sessionId" : "s-concurrent-b" , "modeId" : "default" }),
                ))
                .await
        });
        let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            (dispatch_a.await, dispatch_b.await)
        })
        .await
        .expect("the two buffered session dispatches must both complete");
        let a = a.expect("join a").notifications;
        let b = b.expect("join b").notifications;
        assert_eq!(a.len(), 1, "session A streamed one mode update");
        assert_eq!(b.len(), 1, "session B streamed one mode update");
        let event_id = |line: &str| {
            parse_response(line)["params"]["_meta"]["eventId"]
                .as_str()
                .expect("eventId")
                .to_string()
        };
        assert_eq!(event_id(&a[0]), "s-concurrent-a-1");
        assert_eq!(event_id(&b[0]), "s-concurrent-b-1");
        let sequencer = service.projections.sequencer_arc();
        assert_eq!(sequencer.event_counter("s-concurrent-a"), 1);
        assert_eq!(sequencer.event_counter("s-concurrent-b"), 1);
    }

    #[tokio::test]
    async fn set_model_and_set_mode_require_a_known_session() {
        let (_staging, service) = test_service().await;
        let set_model = service
            .handle_acp_payload(&request_payload(
                "session/set_model",
                json!({ "sessionId": "missing", "modelId": "GLM-5.3-NVFP4" }),
            ))
            .await;
        assert!(
            parse_response(set_model.response.as_deref().expect("response"))
                .get("error")
                .is_some(),
            "set_model on an unknown session must fail"
        );
        let set_mode = service
            .handle_acp_payload(&request_payload(
                "session/set_mode",
                json!({ "sessionId": "missing", "modeId": "yolo" }),
            ))
            .await;
        assert!(
            parse_response(set_mode.response.as_deref().expect("response"))
                .get("error")
                .is_some(),
            "set_mode on an unknown session must fail"
        );
    }

    #[tokio::test]
    async fn interject_and_compact_answer_method_not_found() {
        let (_staging, service) = test_service().await;
        for (method, params) in [
            (
                "x.ai/interject",
                json!({ "sessionId": "s1", "text": "hi", "interjectionId": "i1" }),
            ),
            ("x.ai/compact_conversation", json!({ "sessionId": "s1" })),
        ] {
            let dispatch = service
                .handle_acp_payload(&request_payload(method, params))
                .await;
            assert!(dispatch.notifications.is_empty());
            let response = parse_response(dispatch.response.as_deref().expect("response line"));
            assert_eq!(
                response["error"]["code"], -32601,
                "{method} must answer with the audited method-not-found code"
            );
            assert!(
                !response["error"]["message"]
                    .as_str()
                    .expect("message")
                    .is_empty(),
                "{method} must carry the owned-transition explanation"
            );
        }
    }

    #[tokio::test]
    async fn stubs_never_write_documents() {
        let (_staging, service) = test_service().await;
        service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({ "cwd": "/tmp", "mcpServers": [], "_meta": { "sessionId": "s-stubs" } }),
            ))
            .await;
        for (method, params) in [
            (
                "x.ai/interject",
                json!({ "sessionId": "s-stubs", "text": "hi", "interjectionId": "i1" }),
            ),
            (
                "x.ai/compact_conversation",
                json!({ "sessionId": "s-stubs" }),
            ),
        ] {
            service
                .handle_acp_payload(&request_payload(method, params))
                .await;
        }
        let node = service.config.node.clone();
        let query = r#"{
            AgentRequest(filter: { session_id: { _eq: "s-stubs" } }) { request_id }
            AgentMessage(filter: { session_id: { _eq: "s-stubs" } }) { sequence }
        }"#
        .to_string();
        let response = node.execute(&query).await;
        ensure_no_errors(&response, "stub document check").expect("query ok");
        let requests = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .expect("AgentRequest array");
        let messages = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentMessage"))
            .and_then(Value::as_array)
            .expect("AgentMessage array");
        assert!(
            requests.is_empty() && messages.is_empty(),
            "rejected stubs must not fabricate AgentRequest or AgentMessage rows"
        );
    }

    #[tokio::test]
    async fn unknown_methods_answer_method_not_found() {
        let (_staging, service) = test_service().await;
        let dispatch = service
            .handle_acp_payload(&request_payload("terminal/invent", json!({})))
            .await;
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert_eq!(response["error"]["code"], -32601);
        assert!(
            response["error"]["message"]
                .as_str()
                .expect("message")
                .contains("terminal/invent"),
            "the unrouted method must be named in the error"
        );
    }

    #[tokio::test]
    async fn wait_for_exit_answers_the_pagers_exact_method_not_found_error() {
        let (_staging, service) = test_service().await;
        let dispatch = service
            .handle_acp_payload(&request_payload("terminal/wait_for_exit", json!({})))
            .await;
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert_eq!(response["error"]["code"], -32601);
        assert_eq!(
            response["error"]["message"], PAGER_WAIT_FOR_EXIT_MESSAGE,
            "wait_for_exit must answer with the pager's exact message"
        );
    }

    #[tokio::test]
    async fn other_terminal_methods_answer_the_shaped_not_supported_error() {
        let (_staging, service) = test_service().await;
        for method in [
            "terminal/create",
            "terminal/output",
            "terminal/kill",
            "terminal/release",
        ] {
            let dispatch = service
                .handle_acp_payload(&request_payload(method, json!({})))
                .await;
            let response = parse_response(dispatch.response.as_deref().expect("response line"));
            assert_eq!(response["error"]["code"], -32601, "{method}");
            let message = response["error"]["message"].as_str().expect("message");
            assert!(
                message.contains(&format!("{method}: ")),
                "{method} must be named with the shaped prefix: {message}"
            );
            assert!(
                message.contains("clientTerminal=false"),
                "{method} must explain the agent-side terminal routing: {message}"
            );
            assert!(
                !message.contains("is not supported by the Gents Grok shim"),
                "{method} must not collapse to the generic method-not-found: {message}"
            );
        }
    }

    #[tokio::test]
    async fn undecodable_payloads_answer_invalid_request() {
        let (_staging, service) = test_service().await;
        let dispatch = service.handle_acp_payload("not json").await;
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert_eq!(response["id"], Value::Null);
        assert_eq!(response["error"]["code"], -32600);
    }

    #[tokio::test]
    async fn request_validation_failures_answer_invalid_params() {
        let (_staging, service) = test_service().await;
        service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({ "cwd": "/tmp", "mcpServers": [], "_meta": { "sessionId": "s-table" } }),
            ))
            .await;

        let text_block = json!({"type": "text", "text": "hello"});
        let cases: Vec<(&str, Value)> = vec![
            (
                "session/set_model",
                json!({ "sessionId": "s-table", "modelId": "" }),
            ),
            ("session/set_model", json!({ "sessionId": "s-table" })),
            (
                "session/set_model",
                json!({ "sessionId": "s-table", "modelId": "gpt-9" }),
            ),
            (
                "session/new",
                json!({ "cwd": "/tmp", "_meta": { "sessionId": "s-never", "modelId": "gpt-9" } }),
            ),
            ("session/set_model", json!({ "modelId": "GLM-5.3-NVFP4" })),
            (
                "session/set_model",
                json!({ "sessionId": "  ", "modelId": "GLM-5.3-NVFP4" }),
            ),
            (
                "session/set_mode",
                json!({ "sessionId": "s-table", "modeId": "" }),
            ),
            ("session/set_mode", json!({ "sessionId": "s-table" })),
            ("authenticate", json!({ "methodId": "oauth" })),
            ("authenticate", json!({ "methodId": "" })),
            ("session/prompt", json!({ "prompt": [text_block.clone()] })),
            ("session/prompt", json!({ "sessionId": "s-table" })),
            (
                "session/prompt",
                json!({ "sessionId": "s-table", "prompt": [] }),
            ),
            (
                "session/prompt",
                json!({ "sessionId": "s-table", "prompt": [{"type": "text"}] }),
            ),
            (
                "session/prompt",
                json!({
                    "sessionId": "s-table",
                    "prompt": [text_block.clone()],
                    "_meta": { "screenMode": "sideways" },
                }),
            ),
            (
                "session/set_model",
                json!({
                    "sessionId": "s-table",
                    "modelId": "GLM-5.3-NVFP4",
                    "_meta": { "reasoningEffort": "high" },
                }),
            ),
            ("x.ai/subagent/get", json!({})),
            ("x.ai/subagent/get", json!({ "sessionId": 7 })),
            ("x.ai/subagent/get", json!({ "sessionId": "  " })),
            ("x.ai/subagent/list_running", json!({})),
            ("x.ai/subagent/list_running", json!({ "sessionId": 7 })),
            ("x.ai/subagent/list_running", json!({ "sessionId": "  " })),
            ("x.ai/subagent/get", json!({ "sessionId": "s-table" })),
            (
                "x.ai/subagent/get",
                json!({ "sessionId": "s-table", "subagentId": 7 }),
            ),
            (
                "x.ai/subagent/get",
                json!({ "sessionId": "s-table", "subagentId": "  " }),
            ),
        ];
        for (method, params) in cases {
            let dispatch = service
                .handle_acp_payload(&request_payload(method, params.clone()))
                .await;
            let response = parse_response(dispatch.response.as_deref().expect("response line"));
            assert_eq!(
                response["error"]["code"], -32602,
                "{method} with {params} must answer invalid params"
            );
            assert!(
                !response["error"]["message"]
                    .as_str()
                    .expect("message")
                    .is_empty(),
                "{method} must carry a diagnostic message"
            );
        }

        for (method, params) in [
            (
                "x.ai/interject",
                json!({ "sessionId": "s-table", "text": "hi" }),
            ),
            (
                "x.ai/compact_conversation",
                json!({ "sessionId": "s-table" }),
            ),
            ("terminal/wait_for_exit", json!({})),
            ("terminal/create", json!({})),
            ("some/unknown/method", json!({})),
            ("x.ai/subagent/invent", json!({ "sessionId": "s-table" })),
        ] {
            let dispatch = service
                .handle_acp_payload(&request_payload(method, params))
                .await;
            let response = parse_response(dispatch.response.as_deref().expect("response line"));
            assert_eq!(
                response["error"]["code"], -32601,
                "{method} must stay method-not-found"
            );
        }

        let get = service
            .handle_acp_payload(&request_payload(
                "x.ai/subagent/get",
                json!({ "sessionId": "s-table", "subagentId": "child-1" }),
            ))
            .await;
        let get = parse_response(get.response.as_deref().expect("response line"));
        assert_eq!(get["result"], json!({ "result": {"snapshot": null} }));

        let list = service
            .handle_acp_payload(&request_payload(
                "x.ai/subagent/list_running",
                json!({ "sessionId": "s-table" }),
            ))
            .await;
        let list = parse_response(list.response.as_deref().expect("response line"));
        assert_eq!(list["result"], json!({ "result": {"subagents": []} }));

        let cancel = service
            .handle_acp_payload(&request_payload(
                "x.ai/subagent/cancel",
                json!({ "sessionId": "s-table", "subagentId": "child-1" }),
            ))
            .await;
        let cancel = parse_response(cancel.response.as_deref().expect("response line"));
        assert_eq!(
            cancel["result"],
            json!({ "result": {"subagentId": "child-1", "cancelled": false,
                "outcome": {"kind": "not_found"}} })
        );

        let dispatch = service
            .handle_acp_payload(&request_payload(
                SUBAGENT_GET_METHOD,
                json!({"subagentId": "unknown-child"}),
            ))
            .await;
        let response = parse_response(dispatch.response.as_deref().unwrap());
        assert!(response.get("error").is_none(), "{response}");
        assert!(response["result"].get("result").is_some());
        for method in [SUBAGENT_GET_METHOD, SUBAGENT_LIST_RUNNING_METHOD] {
            let dispatch = service
                .handle_acp_payload(&request_payload(
                    method,
                    json!({"sessionId": "not-registered", "subagentId": "child-1"}),
                ))
                .await;
            let response = parse_response(dispatch.response.as_deref().unwrap());
            assert_eq!(response["error"]["code"], -32602, "{response}");
        }

        let node = service.config.node.clone();
        let query = r#"{
            AgentRequest { request_id }
            AgentMessage { message_key }
            AgentSession { session_id }
        }"#
        .to_string();
        let response = node.execute(&query).await;
        ensure_no_errors(&response, "rejected-input document check").expect("query ok");
        let count = |key: &str| -> usize {
            response
                .data
                .as_ref()
                .and_then(|data| data.get(key))
                .and_then(Value::as_array)
                .map(|rows| rows.len())
                .unwrap_or_default()
        };
        assert_eq!(
            count("AgentRequest"),
            0,
            "no AgentRequest rows were fabricated"
        );
        assert_eq!(
            count("AgentMessage"),
            0,
            "no AgentMessage rows were fabricated"
        );
        assert_eq!(
            count("AgentSession"),
            1,
            "only the legitimate s-table session exists"
        );
    }

    #[tokio::test]
    async fn operational_failures_stay_internal_errors() {
        let (_staging, service) = test_service().await;
        let node = service.config.node.clone();
        let seed = r#"mutation {
            create_AgentSession(input: {
                session_id: "s-internal-op",
                node_did: "did:test:foreign-agent",
                requester_did: "did:test:foreign-agent",
                agent_id: "did:test:grok-shim:default",
                created_at: "2026-08-31T22:46:45Z",
                closed_at: "2026-08-31T22:46:46Z"
            }) { _docID }
        }"#
        .to_string();
        let response = node.execute(&seed).await;
        ensure_no_errors(&response, "foreign session seed").expect("seed ok");

        let dispatch = service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({
                    "cwd": "/tmp",
                    "mcpServers": [],
                    "_meta": { "sessionId": "s-internal-op" },
                }),
            ))
            .await;
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert_eq!(
            response["error"]["code"], -32603,
            "a durable identity mismatch is an operational internal error"
        );
    }

    #[tokio::test]
    async fn notifications_never_produce_a_response_body() {
        let (_staging, service) = test_service().await;
        let dispatch = service
            .handle_acp_payload(&notification_payload(
                "session/cancel",
                json!({ "sessionId": "s1", "_meta": { "promptId": "p1" } }),
            ))
            .await;
        assert!(dispatch.response.is_none());
        assert!(dispatch.notifications.is_empty());

        let unknown = service
            .handle_acp_payload(&notification_payload("x.ai/unknown", json!({})))
            .await;
        assert!(
            unknown.response.is_none(),
            "unknown notifications are dropped"
        );
    }
}
