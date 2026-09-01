//! Grok shim ACP service: initialize, session lifecycle, and stubbed edges.
//!
//! This module owns the JSON-RPC 2.0 surface the stock Grok pager drives once
//! it has registered with the Gents leader socket ([`super::server`]) and its
//! ACP payloads are forwarded here. It implements:
//!
//! - `initialize` — the fixed capability/auth advertisement. `loadSession` is
//!   always `false`: the shim never fabricates a replay, so the pager must
//!   not offer session restore.
//! - `authenticate` — the single `gents.runtime` auth method always succeeds;
//!   client credentials are the transport's concern, not a Gents document.
//! - `session/new` — honors the preferred `_meta.sessionId`, creates exactly
//!   one `AgentSession` document for the returned id (create-only on the
//!   `@immutable` `agent_did`/`requester_did` fields, matching the runtime's
//!   `request_session_projection`), and returns the audited nested result
//!   shape `{"sessionId", "models": {"availableModels", "currentModelId"},
//!   "_meta"}`. Model, context window, and behavior identity all come from
//!   the bound configuration (`AgentBehavior` + `InferenceProfile`), never
//!   from a per-session override the runtime does not model.
//! - `session/set_model` — validates against the bound catalog and emits the
//!   `x.ai/models/update` ext notification. Gents has no per-session model
//!   field, so the switch is connection-local shim state; the bound behavior
//!   still selects the model the runtime serves.
//! - `session/set_mode` — records the pager's mode and emits a
//!   `current_mode_update` session notification. Mode is a client capability
//!   concern, not an `AgentSession` field.
//! - `session/prompt` / `session/cancel` — dispatched to the sibling
//!   [`super::turn::TurnManager`], which owns the connection-scoped pending
//!   prompt, deferred response, and interrupt lifecycle.
//! - `x.ai/subagent/*` — routed to the sibling
//!   [`super::projection::subagents`] shaped not-found stubs, which own those
//!   result shapes.
//!
//! Shaped stubs return JSON-RPC method-not-found (`-32601`) with an explicit
//! owned-transition explanation, never a fabricated success:
//! - `session/load` — the runtime owns replay through normal request
//!   execution; the shim does not synthesize `_meta.isReplay` streams.
//! - `x.ai/interject` — the owned completion loop has no formally specified
//!   injection transition; writing a detached `AgentMessage` would not affect
//!   provider input.
//! - `x.ai/compact_conversation` — `CompactionEntry` is runtime-owned and has
//!   no `tokens_before`/`tokens_after` fields; `AgentSession` has no usage
//!   counters.
//!
//! Anything not routed above — including the client-side `terminal/*`
//! methods, whose pager-style not-supported stub is owned by
//! [`super::projection::tools`] — falls back to the same explicit
//! method-not-found error so an unported control edge stays visible on the
//! wire instead of being silently swallowed.
//!
//! All GraphQL values pass through `gents::graphql::escape_graphql_string`
//! and every query runs in-process on the embedded node via
//! `EmbeddedNode::execute`; no HTTP GraphQL helper is used.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context, Result};
use gents::defra_node::EmbeddedNode;
use gents::graphql::{ensure_no_errors, escape_graphql_string};
use serde_json::{json, Value};

use super::projection::ProjectionEngine;
use super::server::AcpDelegate;
use super::turn::TurnManager;

/// JSON-RPC 2.0 error code for a malformed request envelope.
pub(crate) const JSONRPC_INVALID_REQUEST: i64 = -32600;

/// JSON-RPC 2.0 error code for an unknown/unhandled method. Every shaped stub
/// in this module answers with exactly this code so the pager classifies the
/// edge as unsupported rather than failed.
pub(crate) const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;

/// JSON-RPC 2.0 error code for structurally invalid request params.
pub(crate) const JSONRPC_INVALID_PARAMS: i64 = -32602;

/// JSON-RPC 2.0 error code for an internal shim failure.
pub(crate) const JSONRPC_INTERNAL_ERROR: i64 = -32603;

/// The single auth method the shim advertises: Gents runtime identity is the
/// only supported credential surface.
pub(crate) const GENTS_AUTH_METHOD_ID: &str = "gents.runtime";

/// ACP protocol version the shim speaks.
pub(crate) const ACP_PROTOCOL_VERSION: i64 = 1;

/// Wire name of the `initialize` request method.
pub(crate) const INITIALIZE_METHOD: &str = "initialize";

/// Wire name of the `authenticate` request method.
pub(crate) const AUTHENTICATE_METHOD: &str = "authenticate";

/// Wire name of the `session/new` request method.
pub(crate) const SESSION_NEW_METHOD: &str = "session/new";

/// Wire name of the `session/load` request method (shaped stub).
pub(crate) const SESSION_LOAD_METHOD: &str = "session/load";

/// Wire name of the `session/set_model` request method.
pub(crate) const SESSION_SET_MODEL_METHOD: &str = "session/set_model";

/// Wire name of the `session/set_mode` request method.
pub(crate) const SESSION_SET_MODE_METHOD: &str = "session/set_mode";

/// Wire name of the `session/prompt` request method (owned by `turn.rs`).
pub(crate) const SESSION_PROMPT_METHOD: &str = "session/prompt";

/// Wire name of the `session/cancel` notification (owned by `turn.rs`).
pub(crate) const SESSION_CANCEL_METHOD: &str = "session/cancel";

/// Wire name of the `x.ai/interject` ext request (shaped stub).
pub(crate) const INTERJECT_METHOD: &str = "x.ai/interject";

/// Wire name of the `x.ai/compact_conversation` ext request (shaped stub).
pub(crate) const COMPACT_CONVERSATION_METHOD: &str = "x.ai/compact_conversation";

/// Ext notification method emitted after a model catalog switch.
pub(crate) const MODELS_UPDATE_METHOD: &str = "x.ai/models/update";

/// Session notification method carrying every session update, including the
/// `current_mode_update` this module emits.
pub(crate) const SESSION_UPDATE_METHOD: &str = "session/update";

/// Reasoning efforts advertised for the bound model. These mirror the
/// canonical values the pager's `model_state` accepts and Gents'
/// `ReasoningEffort` parses; `minimal`/`max`/`ultra` stay unadvertised
/// because the pager's catalog has no canonical rendering for them.
pub(crate) const ADVERTISED_REASONING_EFFORTS: &[(&str, &str)] = &[
    ("none", "No reasoning"),
    ("low", "Low"),
    ("medium", "Medium"),
    ("high", "High"),
    ("xhigh", "Extra high"),
];

// ---------------------------------------------------------------------------
// Bound configuration
// ---------------------------------------------------------------------------

/// Bound model catalog entry derived from the serving configuration.
///
/// `model_id` is the wire-facing model identifier. For a behavior with a
/// `backend_id` the catalog id is `<backend_id>::<model_name>` (the codex
/// shim's selection format); otherwise it is the bare `model_name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundModel {
    /// Wire-facing model identifier (`modelId` on the wire).
    pub(crate) model_id: String,
    /// Human-readable display name (`name` on the wire).
    pub(crate) name: String,
    /// Context window from the bound `InferenceProfile`, advertised as
    /// `meta.totalContextTokens`.
    pub(crate) total_context_tokens: u64,
}

impl BoundModel {
    /// Serialize this entry into the pager's `availableModels` item shape.
    fn catalog_entry(&self) -> Value {
        let efforts = ADVERTISED_REASONING_EFFORTS
            .iter()
            .enumerate()
            .map(|(index, (value, label))| {
                json!({
                    "id": format!("effort-{index}"),
                    "value": value,
                    "label": label,
                    "description": format!("Reasoning effort {label}"),
                    "default": *value == "high",
                })
            })
            .collect::<Vec<_>>();
        json!({
            "modelId": self.model_id,
            "name": self.name,
            "meta": {
                "totalContextTokens": self.total_context_tokens,
                "acceptsImages": true,
                "inputModalities": ["text", "image"],
                "supportsReasoningEffort": true,
                "reasoningEfforts": efforts,
            },
        })
    }
}

impl BoundModel {
    /// Build the `models` object of a `session/new` result.
    ///
    /// The audited wire shape nests `availableModels` and `currentModelId`
    /// under a `models` key — see `recon-input/audited-ledger.json`
    /// (`session:new-load`) and the live probe's
    /// `session["models"]["currentModelId"]` read. Splicing the catalog keys
    /// into the top-level result object breaks the pager, so the nesting is
    /// asserted by the tests in this file.
    pub(crate) fn models_object(&self) -> Value {
        json!({
            "availableModels": [self.catalog_entry()],
            "currentModelId": self.model_id,
        })
    }
}

/// Immutable bound configuration for the ACP service.
///
/// Every field is resolved once at bind time from the bound behavior and
/// inference profile (see `grok_shim.rs` assembly); nothing here is
/// per-session runtime state.
#[derive(Debug, Clone)]
pub(crate) struct AcpServiceConfig {
    /// In-process embedded node used for every GraphQL query/mutation.
    pub(crate) node: Arc<EmbeddedNode>,
    /// Serving agent DID (the `@immutable` `AgentSession.agent_did` value).
    pub(crate) agent_did: Arc<str>,
    /// Serving agent display name (stamped on `AgentSession.agent_name`).
    pub(crate) agent_name: Arc<str>,
    /// Bound behavior id (stamped on `AgentSession.behavior_id`).
    pub(crate) behavior_id: Arc<str>,
    /// Bound model the runtime serves for this behavior.
    pub(crate) current_model: BoundModel,
}

impl AcpServiceConfig {
    /// Build the `models` object of a `session/new` result for the currently
    /// bound model. Delegates to [`BoundModel::models_object`], which owns the
    /// audited nested shape and its tests.
    pub(crate) fn models_object(&self) -> Value {
        self.current_model.models_object()
    }
}

// ---------------------------------------------------------------------------
// Connection-local session state
// ---------------------------------------------------------------------------

/// Per-session shim state.
///
/// Gents documents record session identity (`AgentSession`) and request
/// history (`AgentRequest`/`AgentResponse`); they have no cwd, model, or mode
/// fields. Everything the pager needs that the runtime does not model is
/// connection-local state here and is never persisted.
#[derive(Debug, Clone)]
struct AcpSessionState {
    /// `_meta.yoloMode` captured from `session/new`; connection-local.
    yolo_mode: bool,
    /// `_meta.autoMode` captured from `session/new`; connection-local.
    auto_mode: bool,
    /// Current pager mode id from `session/set_mode`.
    mode_id: String,
    /// Current reasoning effort from `session/set_model` `_meta`, if set.
    reasoning_effort: Option<String>,
}

impl AcpSessionState {
    fn new(yolo_mode: bool, auto_mode: bool) -> Self {
        Self {
            yolo_mode,
            auto_mode,
            mode_id: "default".to_string(),
            reasoning_effort: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Dispatch envelope
// ---------------------------------------------------------------------------

/// One parsed ACP JSON-RPC frame, dispatch-level only.
///
/// The wire envelope and frame codec are owned by [`super::protocol`]; this
/// is the dispatch-side view of a single decoded ACP payload line.
#[derive(Debug, Clone)]
pub(crate) struct AcpRequest {
    /// JSON-RPC request id. `None` marks a notification.
    pub(crate) id: Option<Value>,
    /// Requested method name.
    pub(crate) method: String,
    /// Request params (`{}` when absent).
    pub(crate) params: Value,
}

impl AcpRequest {
    /// Parse one ACP payload line. A non-object or id-less-when-claimed frame
    /// is rejected so the caller can answer `-32600`.
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
        // JSON-RPC 2.0: a request carries an id; a notification does not. A
        // JSON `null` id is treated as absent, matching the pager's decoder.
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

/// The wire output of one dispatched frame.
///
/// Notifications are emitted before the response so a pager that drains
/// until it sees its response id observes the same order the reference agent
/// produces (for example `x.ai/models/update` before the `session/set_model`
/// response).
#[derive(Debug, Clone, Default)]
pub(crate) struct AcpDispatch {
    /// Serialized JSON-RPC notification lines to emit first.
    pub(crate) notifications: Vec<String>,
    /// Serialized JSON-RPC response line, or `None` for notifications.
    pub(crate) response: Option<String>,
}

/// Outcome of a request handler: the result value plus notifications to emit
/// before the response envelope is written.
struct RequestOutcome {
    notifications: Vec<Value>,
    result: Value,
}

/// Shape a successful JSON-RPC response envelope.
fn response_line(id: &Value, result: &Value) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }))
    .expect("JSON-RPC response envelope is serializable")
}

/// Shape a JSON-RPC error envelope.
fn error_line(id: &Value, code: i64, message: &str) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    }))
    .expect("JSON-RPC error envelope is serializable")
}

/// Shape a JSON-RPC notification line.
fn notification_line(method: &str, params: &Value) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    }))
    .expect("JSON-RPC notification envelope is serializable")
}

// ---------------------------------------------------------------------------
// ACP service
// ---------------------------------------------------------------------------

/// The ACP service implementing the leader server's delegate.
pub(crate) struct AcpService {
    config: AcpServiceConfig,
    /// Connection-scoped prompt/cancel lifecycle (sibling slice `turn.rs`).
    turns: Arc<TurnManager>,
    /// Request-id-scoped durable projections (sibling slice `projection.rs`).
    projections: Arc<ProjectionEngine>,
    /// Monotonic per-session event counter. Every `session/update` carries
    /// `_meta.eventId = "{sessionId}-{counter}"`; counters never repeat
    /// within a session so the pager's dedup never drops a live update.
    event_counters: tokio::sync::Mutex<BTreeMap<String, u64>>,
    /// Connection-local session registry.
    sessions: tokio::sync::Mutex<BTreeMap<String, AcpSessionState>>,
}

impl AcpService {
    /// Assemble the service from bound configuration and the sibling
    /// engines. The assembly slice (`grok_shim.rs`) constructs the
    /// [`TurnManager`] and [`ProjectionEngine`] from the same embedded node
    /// and bound behavior/model/context configuration.
    pub(crate) fn new(
        config: AcpServiceConfig,
        turns: Arc<TurnManager>,
        projections: Arc<ProjectionEngine>,
    ) -> Self {
        Self {
            config,
            turns,
            projections,
            event_counters: tokio::sync::Mutex::new(BTreeMap::new()),
            sessions: tokio::sync::Mutex::new(BTreeMap::new()),
        }
    }

    /// The shared projection engine, for the turn loop and assembly wiring.
    pub(crate) fn projections(&self) -> &Arc<ProjectionEngine> {
        &self.projections
    }

    /// Allocate the next monotonic event id for a session.
    pub(crate) async fn next_event_id(&self, session_id: &str) -> String {
        let mut counters = self.event_counters.lock().await;
        let counter = counters.entry(session_id.to_string()).or_insert(0);
        *counter += 1;
        format!("{session_id}-{counter}")
    }

    /// Build the `_meta` block a `session/update` notification carries.
    ///
    /// `totalTokens` is the connection-local cumulative projection sourced
    /// from `AgentResponse.token_count`; that projection is owned by the
    /// sibling `projection::messages`, so this seam only formats the envelope
    /// and omits the key rather than fabricating a count. `promptId`
    /// correlates the update with its turn and `eventId` is monotonic.
    pub(crate) async fn session_update_meta(
        &self,
        session_id: &str,
        prompt_id: Option<&str>,
        total_tokens: Option<u64>,
    ) -> Value {
        let event_id = self.next_event_id(session_id).await;
        let mut meta = json!({
            "eventId": event_id,
        });
        if let Some(prompt_id) = prompt_id {
            meta["promptId"] = json!(prompt_id);
        }
        if let Some(total_tokens) = total_tokens {
            meta["totalTokens"] = json!(total_tokens);
        }
        meta
    }

    /// Dispatch one decoded ACP payload line.
    ///
    /// This is the delegate entry point: it never fails and never panics.
    /// Every failure mode — including an undecodable payload and an unknown
    /// method — is answered with a shaped JSON-RPC error so the pager is
    /// never left waiting on a response that never arrives.
    pub(crate) async fn handle_acp_payload(&self, payload: &str) -> AcpDispatch {
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
        self.dispatch_request(request).await
    }

    /// Dispatch a notification. Notifications have no response body; an
    /// unknown method is logged and dropped rather than answered.
    async fn dispatch_notification(&self, request: AcpRequest) -> AcpDispatch {
        match request.method.as_str() {
            SESSION_CANCEL_METHOD => {
                if let Err(error) = self.turns.cancel(&request.params).await {
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

    /// Dispatch a request, mapping every handler failure to a shaped error.
    async fn dispatch_request(&self, request: AcpRequest) -> AcpDispatch {
        let id = request.id.clone().unwrap_or_else(|| Value::Null);
        let method = request.method.clone();
        match self.handle_request(&request).await {
            Ok(outcome) => {
                let notifications = outcome
                    .notifications
                    .iter()
                    .map(|params| {
                        // Each notification value carries its own method.
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
                AcpDispatch {
                    notifications,
                    response: Some(response_line(&id, &outcome.result)),
                }
            }
            Err(error) => {
                let code = error_code_for(&error);
                tracing::warn!(%error, %method, code, "grok shim ACP request failed");
                AcpDispatch {
                    notifications: Vec::new(),
                    response: Some(error_line(&id, code, &error.to_string())),
                }
            }
        }
    }

    /// Route one request to its handler.
    async fn handle_request(&self, request: &AcpRequest) -> Result<RequestOutcome> {
        match request.method.as_str() {
            INITIALIZE_METHOD => self.handle_initialize(request).await,
            AUTHENTICATE_METHOD => self.handle_authenticate(request).await,
            SESSION_NEW_METHOD => self.handle_session_new(request).await,
            SESSION_SET_MODEL_METHOD => self.handle_session_set_model(request).await,
            SESSION_SET_MODE_METHOD => self.handle_session_set_mode(request).await,
            SESSION_PROMPT_METHOD => self.handle_session_prompt(request).await,
            // The three audited shaped stubs: explicit method-not-found with
            // the owned-transition explanation, never a fabricated success.
            SESSION_LOAD_METHOD | INTERJECT_METHOD | COMPACT_CONVERSATION_METHOD => {
                Err(shaped_stub_error(&request.method, &request.params))
            }
            other if other.starts_with("x.ai/subagent/") => {
                self.handle_subagent_ext_request(other, request).await
            }
            other => {
                // Unrouted methods — including the client-side terminal/*
                // methods whose pager-style not-supported stub is owned by
                // `projection::tools` — stay explicit on the wire.
                Err(anyhow::Error::new(ShapedMethodNotFound {
                    message: format!("method {other:?} is not supported by the Gents Grok shim"),
                }))
            }
        }
    }

    /// Handle `initialize`.
    ///
    /// `loadSession` is hardcoded `false`: the shaped `session/load` stub
    /// below means the pager must not present restore as available.
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
                    "loadSession": false,
                    "prompt": true,
                    "cancel": true,
                    "setMode": true,
                    "setModel": true,
                },
                "authMethods": [
                    { "id": GENTS_AUTH_METHOD_ID, "description": "Gents runtime identity" }
                ],
            }),
        })
    }

    /// Handle `authenticate`.
    ///
    /// The only advertised method is `gents.runtime`; an unknown `methodId`
    /// is an explicit error, not a silent success. No credential document is
    /// written: the runtime's own identity is the credential.
    async fn handle_authenticate(&self, request: &AcpRequest) -> Result<RequestOutcome> {
        let method_id = request
            .params
            .get("methodId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if method_id != GENTS_AUTH_METHOD_ID {
            anyhow::bail!(
                "unsupported auth method {method_id:?}; the Grok shim advertises only \
                 {GENTS_AUTH_METHOD_ID:?}"
            );
        }
        Ok(RequestOutcome {
            notifications: Vec::new(),
            result: json!({
                "_meta": { "provider": "gents" },
            }),
        })
    }

    /// Handle `session/new`.
    ///
    /// The preferred `_meta.sessionId` is honored verbatim when non-empty;
    /// otherwise a fresh uuid is minted. Exactly one `AgentSession` document
    /// exists for the returned id afterwards — create-only when absent,
    /// matching the runtime's `request_session_projection` semantics, and
    /// never rewriting the `@immutable` `agent_did`/`requester_did` fields on
    /// an existing row.
    ///
    /// `cwd` and `mcpServers` are accepted and deliberately not persisted:
    /// `AgentSession` has no cwd field and the runtime serves from its own
    /// working directory, so fabricating either would be a schema violation.
    /// No `AgentConversation` and no `AgentRequest` rows are created here;
    /// the runtime materializes those through normal request execution, and
    /// fabricating them would desynchronize the durable timeline.
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
        let yolo_mode = meta
            .get("yoloMode")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let auto_mode = meta
            .get("autoMode")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if let Some(requested) = requested_model.map(str::trim).filter(|v| !v.is_empty()) {
            if requested != self.config.current_model.model_id {
                anyhow::bail!(
                    "model {requested:?} is not in the bound catalog; the Grok shim serves \
                     {:?} from the bound AgentBehavior/InferenceProfile",
                    self.config.current_model.model_id
                );
            }
        }

        let session_id = preferred.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        ensure_session_document(&self.config, &session_id).await?;

        self.sessions.lock().await.insert(
            session_id.clone(),
            AcpSessionState::new(yolo_mode, auto_mode),
        );
        tracing::info!(%session_id, "grok shim session/new");

        // Audited result shape: sessionId + nested models + _meta. The
        // `models` object must stay nested — the pager reads
        // `result["models"]["currentModelId"]`.
        Ok(RequestOutcome {
            notifications: Vec::new(),
            result: json!({
                "sessionId": session_id,
                "models": self.config.models_object(),
                "_meta": {
                    "yoloMode": yolo_mode,
                    "autoMode": auto_mode,
                    "modelId": self.config.current_model.model_id,
                },
            }),
        })
    }

    /// Handle `session/set_model`.
    ///
    /// The runtime has no per-session model field: the bound
    /// `AgentBehavior` selects the model every request is served with. The
    /// switch therefore validates against the bound catalog, records the
    /// requested reasoning effort, and emits `x.ai/models/update` so the
    /// pager refreshes its catalog in place (leaving current/effort alone,
    /// as the reference `update_catalog` does).
    async fn handle_session_set_model(&self, request: &AcpRequest) -> Result<RequestOutcome> {
        let session_id = required_session_id(&request.params)?;
        let requested = request
            .params
            .get("modelId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("session/set_model requires a non-empty modelId"))?;
        if requested != self.config.current_model.model_id {
            anyhow::bail!(
                "model {requested:?} is not in the bound catalog; the Grok shim serves \
                 {:?} from the bound AgentBehavior/InferenceProfile",
                self.config.current_model.model_id
            );
        }
        let reasoning_effort = request
            .params
            .get("_meta")
            .and_then(|meta| meta.get("reasoningEffort"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        if let Some(effort) = reasoning_effort.as_deref() {
            gents::config::ReasoningEffort::parse(effort)
                .with_context(|| format!("invalid reasoningEffort {effort:?}"))?;
        }

        {
            let mut sessions = self.sessions.lock().await;
            let state = sessions
                .get_mut(&session_id)
                .ok_or_else(|| anyhow::anyhow!("unknown session {session_id:?}"))?;
            state.reasoning_effort = reasoning_effort;
        }

        tracing::info!(%session_id, %requested, "grok shim session/set_model");
        // Empty result per the audited wire; the catalog refresh rides the
        // x.ai/models/update ext notification emitted before the response.
        Ok(RequestOutcome {
            notifications: vec![json!({
                "__method": MODELS_UPDATE_METHOD,
                "models": self.config.models_object(),
            })],
            result: json!({}),
        })
    }

    /// Handle `session/set_mode`.
    ///
    /// Mode is a client capability concern with no `AgentSession` field; the
    /// switch records it and emits a `current_mode_update` session
    /// notification so the pager renders the new mode.
    async fn handle_session_set_mode(&self, request: &AcpRequest) -> Result<RequestOutcome> {
        let session_id = required_session_id(&request.params)?;
        let mode_id = request
            .params
            .get("modeId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("session/set_mode requires a non-empty modeId"))?;

        {
            let mut sessions = self.sessions.lock().await;
            let state = sessions
                .get_mut(&session_id)
                .ok_or_else(|| anyhow::anyhow!("unknown session {session_id:?}"))?;
            state.mode_id = mode_id.to_string();
        }

        let meta = self.session_update_meta(&session_id, None, None).await;
        tracing::info!(%session_id, %mode_id, "grok shim session/set_mode");
        Ok(RequestOutcome {
            notifications: vec![json!({
                "__method": SESSION_UPDATE_METHOD,
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "current_mode_update",
                    "currentMode": mode_id,
                },
                "_meta": meta,
            })],
            result: json!({}),
        })
    }

    /// Handle `session/prompt` by delegating the whole turn to the sibling
    /// [`TurnManager`].
    ///
    /// The turn manager owns the connection-scoped pending prompt, registers
    /// the runtime request id before the first fallible outbound send, keeps
    /// the JSON-RPC response deferred until terminalization, and streams the
    /// turn's `session/update` notifications. Those notifications are
    /// returned here and emitted before the response, matching the pager's
    /// drain-until-response-id loop.
    async fn handle_session_prompt(&self, request: &AcpRequest) -> Result<RequestOutcome> {
        let session_id = required_session_id(&request.params)?;
        let outcome = self.turns.prompt(&session_id, &request.params).await?;
        Ok(RequestOutcome {
            notifications: outcome.notifications,
            result: json!({ "stopReason": outcome.stop_reason }),
        })
    }

    /// Route one `x.ai/subagent/*` ext request to the sibling subagents leaf,
    /// which owns the audited not-found result shapes. The leaf is pure: it
    /// never queries `Task` rows and never fabricates child documents.
    async fn handle_subagent_ext_request(
        &self,
        method: &str,
        request: &AcpRequest,
    ) -> Result<RequestOutcome> {
        let result =
            super::projection::subagents::handle_subagent_ext_request(method, &request.params)?;
        Ok(RequestOutcome {
            notifications: Vec::new(),
            result,
        })
    }
}

// ---------------------------------------------------------------------------
// Delegate binding
// ---------------------------------------------------------------------------

/// A JSON-RPC method-not-found failure, distinguished from an internal error
/// so the dispatcher can shape the envelope with the exact audited code.
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

/// Build the shaped method-not-found error for one of the three audited
/// stubs, carrying the explicit owned-transition explanation the audit
/// requires. None of these stubs touches a document.
fn shaped_stub_error(method: &str, params: &Value) -> anyhow::Error {
    let session = optional_session_id(params).unwrap_or_default();
    let message = match method {
        SESSION_LOAD_METHOD => format!(
            "session/load is not supported by the Gents Grok shim: session replay is owned \
             by the Gents runtime's normal request execution, and the shim will not \
             fabricate a _meta.isReplay stream or a restore summary for session \
             {session:?}. initialize advertises loadSession=false for this reason."
        ),
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

/// Map a handler error to its JSON-RPC code.
///
/// A [`ShapedMethodNotFound`] is the audited `-32601`; a missing/empty
/// `sessionId` is `-32602`; everything else is an internal error whose
/// message is surfaced verbatim for diagnosis.
fn error_code_for(error: &anyhow::Error) -> i64 {
    if error.downcast_ref::<ShapedMethodNotFound>().is_some() {
        JSONRPC_METHOD_NOT_FOUND
    } else if is_missing_session_id(error) {
        JSONRPC_INVALID_PARAMS
    } else {
        JSONRPC_INTERNAL_ERROR
    }
}

/// Whether an error is the missing-session-id param error.
fn is_missing_session_id(error: &anyhow::Error) -> bool {
    error.to_string().contains("requires a non-empty sessionId")
}

/// Extract the required `sessionId` from request params.
fn required_session_id(params: &Value) -> Result<String> {
    params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("request requires a non-empty sessionId"))
}

/// Extract an optional `sessionId` from request params for stub messaging.
fn optional_session_id(params: &Value) -> Option<String> {
    params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Create the `AgentSession` document for `session_id` if absent.
///
/// Mirrors the runtime's `request_session_projection`: update the mutable
/// identity fields when the row exists, create when it does not. The
/// `@immutable` fields (`agent_did`, `requester_did`) are only ever supplied
/// on create, and — matching the runtime's claim-admission behavior — a row
/// bound to a different behavior id is an explicit error rather than a
/// silent rewrite of session identity.
async fn ensure_session_document(config: &AcpServiceConfig, session_id: &str) -> Result<()> {
    let escaped_session_id = escape_graphql_string(session_id);
    let lookup = format!(
        r#"{{
            AgentSession(filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}) {{
                session_id
                behavior_id
            }}
        }}"#
    );
    let response = config.node.execute(&lookup).await;
    ensure_no_errors(&response, "grok shim AgentSession lookup")?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentSession"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(existing) = rows
        .iter()
        .filter_map(|row| row.get("behavior_id").and_then(Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())
    {
        if existing != config.behavior_id.as_ref() {
            anyhow::bail!(
                "session {session_id:?} already exists with behavior {existing:?}; the \
                 Grok shim is bound to {:?} and will not rewrite session identity",
                config.behavior_id
            );
        }
    }

    let escaped_agent_name = escape_graphql_string(&config.agent_name);
    let escaped_agent_did = escape_graphql_string(&config.agent_did);
    let escaped_behavior_id = escape_graphql_string(&config.behavior_id);
    // DateTime fields round-trip through the "....Z" form the runtime's own
    // fixtures use; to_rfc3339() emits "+00:00" instead.
    let started = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let escaped_started = escape_graphql_string(&started);
    if rows.is_empty() {
        let create = format!(
            r#"mutation {{
                create_AgentSession(input: {{
                    session_id: "{escaped_session_id}",
                    agent_name: "{escaped_agent_name}",
                    agent_did: "{escaped_agent_did}",
                    behavior_id: "{escaped_behavior_id}",
                    started: "{escaped_started}",
                    status: "active"
                }}) {{ _docID }}
            }}"#
        );
        let response = config.node.execute(&create).await;
        ensure_no_errors(&response, "grok shim AgentSession create")?;
    } else {
        // Reactivate an existing row without touching the immutable fields.
        let update = format!(
            r#"mutation {{
                update_AgentSession(
                    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                    input: {{
                        agent_name: "{escaped_agent_name}",
                        behavior_id: "{escaped_behavior_id}",
                        status: "active",
                        ended: null
                    }}
                ) {{ _docID }}
            }}"#
        );
        let response = config.node.execute(&update).await;
        ensure_no_errors(&response, "grok shim AgentSession reactivate")?;
    }
    Ok(())
}

/// Bind the service to the leader server's delegate trait.
///
/// `spawn_leader` takes an `Arc<dyn AcpDelegate>`, so the trait must be
/// object-safe, and the workspace forbids adding an `async-trait` dependency
/// to this crate. The object-safe form is therefore a borrowed boxed future
/// over the payload line that [`super::protocol`] decodes from the
/// `{"type":"acp","payload":"..."}` envelope. The exact trait signature is
/// owned by the `server` slice; convergence reconciles this one impl block
/// if the sibling chose an equivalent spelling.
impl AcpDelegate for AcpService {
    fn handle_acp<'a>(
        &'a self,
        payload: &'a str,
    ) -> Pin<Box<dyn Future<Output = AcpDispatch> + Send + 'a>> {
        Box::pin(self.handle_acp_payload(payload))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn bound_model() -> BoundModel {
        BoundModel {
            model_id: "GLM-5.3-NVFP4".to_string(),
            name: "GLM 5.3 NVFP4".to_string(),
            total_context_tokens: 262_144,
        }
    }

    async fn config() -> AcpServiceConfig {
        let dir = tempfile::tempdir().expect("tempdir");
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(dir.path().join("node"))
                .with_storage_backend(gents::defra_node::StorageBackend::Lark)
                .build()
                .await
                .expect("embedded node"),
        );
        // Keep the tempdir alive for the node's lifetime by leaking it: the
        // node's storage path must outlive the config.
        std::mem::forget(dir);
        AcpServiceConfig {
            node,
            agent_did: Arc::from("did:test:grok-shim"),
            agent_name: Arc::from("grok-shim-test"),
            behavior_id: Arc::from("did:test:grok-shim:default"),
            current_model: bound_model(),
        }
    }

    /// Build a service over a throwaway node with schemas ensured.
    ///
    /// The sibling constructors are the assembly slice's seam: `TurnManager`
    /// and `ProjectionEngine` are built from the same embedded node. Their
    /// exact constructor signatures are owned by those slices and reconciled
    /// at convergence.
    async fn test_service() -> AcpService {
        let config = config().await;
        gents::schema::ensure_runtime_schemas(config.node.as_ref())
            .await
            .expect("runtime schemas");
        let turns = Arc::new(TurnManager::new(config.node.clone()));
        let projections = Arc::new(ProjectionEngine::new(config.node.clone()));
        AcpService::new(config, turns, projections)
    }

    fn request_payload(method: &str, params: Value) -> String {
        serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        }))
        .expect("request payload")
    }

    fn notification_payload(method: &str, params: Value) -> String {
        serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .expect("notification payload")
    }

    fn parse_response(line: &str) -> Value {
        serde_json::from_str(line).expect("response line is JSON")
    }

    // -- Pure shape tests (no node, no siblings) -----------------------------

    #[test]
    fn models_object_is_nested_and_complete() {
        let models = bound_model().models_object();
        // THE audited shape: nested under "models", not spliced to top level.
        assert_eq!(
            models["currentModelId"], "GLM-5.3-NVFP4",
            "currentModelId must live inside the models object"
        );
        let available = models["availableModels"]
            .as_array()
            .expect("availableModels is an array");
        assert_eq!(available.len(), 1);
        assert_eq!(available[0]["modelId"], "GLM-5.3-NVFP4");
        assert_eq!(available[0]["meta"]["totalContextTokens"], 262_144);
        assert_eq!(
            available[0]["meta"]["inputModalities"],
            json!(["text", "image"])
        );
        assert!(
            available[0]["meta"]["reasoningEfforts"]
                .as_array()
                .expect("reasoningEfforts array")
                .iter()
                .any(|effort| effort["value"] == "high"),
            "catalog must advertise the high reasoning effort"
        );
        assert!(
            available[0]["meta"]["reasoningEfforts"]
                .as_array()
                .expect("reasoningEfforts array")
                .iter()
                .any(|effort| effort["value"] == "none"),
            "catalog must advertise the none reasoning effort"
        );
    }

    #[test]
    fn catalog_entry_has_no_leading_underscore_keys() {
        let entry = bound_model().catalog_entry();
        assert!(entry.get("meta").is_some());
        assert_eq!(entry["modelId"], "GLM-5.3-NVFP4");
        // The display name falls back to the raw id when absent; ours is set.
        assert_eq!(entry["name"], "GLM 5.3 NVFP4");
    }

    #[test]
    fn acp_request_parses_request_and_notification() {
        let request =
            AcpRequest::from_payload(&request_payload("initialize", json!({}))).expect("parse");
        assert_eq!(request.id, Some(json!(1)));
        assert_eq!(request.method, "initialize");
        assert!(!request.is_notification());

        let notification = AcpRequest::from_payload(&notification_payload(
            "session/cancel",
            json!({ "sessionId": "s1" }),
        ))
        .expect("parse");
        assert_eq!(notification.id, None);
        assert!(notification.is_notification());
        assert_eq!(notification.params["sessionId"], "s1");
    }

    #[test]
    fn acp_request_rejects_malformed_payloads() {
        assert!(AcpRequest::from_payload("not json").is_err());
        assert!(AcpRequest::from_payload("[]").is_err());
        assert!(AcpRequest::from_payload("{}").is_err());
        assert!(AcpRequest::from_payload("{\"id\":1}").is_err());
        // A null id is a notification, matching the pager's decoder.
        let null_id =
            AcpRequest::from_payload("{\"jsonrpc\":\"2.0\",\"id\":null,\"method\":\"x\"}")
                .expect("parse");
        assert!(null_id.is_notification());
    }

    #[test]
    fn response_and_error_envelopes_are_shaped() {
        let response = parse_response(&response_line(&json!(7), &json!({"ok": true})));
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 7);
        assert_eq!(response["result"]["ok"], true);

        let error = parse_response(&error_line(&json!(7), -32601, "nope"));
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(error["id"], 7);
        assert_eq!(error["error"]["code"], -32601);
        assert_eq!(error["error"]["message"], "nope");

        let notification = parse_response(&notification_line("x.ai/models/update", &json!({})));
        assert_eq!(notification["jsonrpc"], "2.0");
        assert_eq!(notification["method"], "x.ai/models/update");
        assert!(notification.get("id").is_none());
    }

    #[test]
    fn shaped_stub_messages_name_the_owned_transition() {
        let load = shaped_stub_error(
            SESSION_LOAD_METHOD,
            &json!({ "sessionId": "s1", "cwd": "/tmp", "mcpServers": [] }),
        );
        let message = load.to_string();
        assert!(
            message.contains("runtime's normal request execution"),
            "session/load stub must explain the owned transition: {message}"
        );
        assert!(
            message.contains("isReplay"),
            "session/load stub must name the replay fabrication it refuses: {message}"
        );
        assert!(
            message.contains("loadSession=false"),
            "session/load stub must reconcile with the advertised capability: {message}"
        );

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
    }

    #[test]
    fn required_session_id_rejects_missing_and_empty() {
        assert!(required_session_id(&json!({})).is_err());
        assert!(required_session_id(&json!({ "sessionId": "" })).is_err());
        assert!(required_session_id(&json!({ "sessionId": "  " })).is_err());
        assert_eq!(
            required_session_id(&json!({ "sessionId": " s1 " })).expect("session id"),
            "s1"
        );
    }

    #[test]
    fn optional_session_id_tolerates_absence() {
        assert_eq!(optional_session_id(&json!({})), None);
        assert_eq!(optional_session_id(&json!({ "sessionId": "" })), None);
        assert_eq!(
            optional_session_id(&json!({ "sessionId": "s1" })),
            Some("s1".to_string())
        );
    }

    // -- Service tests (node-backed) ----------------------------------------

    #[tokio::test]
    async fn initialize_advertises_load_session_false_and_gents_auth() {
        let service = test_service().await;
        let dispatch = service
            .handle_acp_payload(&request_payload(
                "initialize",
                json!({ "protocolVersion": 1, "clientInfo": { "name": "probe" } }),
            ))
            .await;
        assert!(dispatch.notifications.is_empty());
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert_eq!(response["id"], 1);
        assert_eq!(
            response["result"]["agentCapabilities"]["loadSession"],
            false
        );
        assert_eq!(response["result"]["authMethods"][0]["id"], "gents.runtime");
    }

    #[tokio::test]
    async fn authenticate_accepts_gents_runtime_and_rejects_others() {
        let service = test_service().await;
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
        assert_eq!(response["error"]["code"], -32603);
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
        let service = test_service().await;
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
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        let result = &response["result"];
        assert_eq!(result["sessionId"], "grok-edge-preferred");
        // THE audited read path: result["models"]["currentModelId"].
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
        // And must NOT leak the catalog keys to the top level.
        assert!(result.get("availableModels").is_none());
        assert!(result.get("currentModelId").is_none());
        assert_eq!(result["_meta"]["yoloMode"], true);
        assert_eq!(result["_meta"]["autoMode"], false);
        assert_eq!(result["_meta"]["modelId"], "GLM-5.3-NVFP4");
    }

    #[tokio::test]
    async fn session_new_creates_exactly_one_session_and_zero_requests() {
        let service = test_service().await;
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
                session_id behavior_id agent_did status
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
        assert_eq!(sessions[0]["behavior_id"], "did:test:grok-shim:default");
        assert_eq!(sessions[0]["agent_did"], "did:test:grok-shim");
        assert_eq!(sessions[0]["status"], "active");
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
    async fn session_new_is_idempotent_for_the_same_session_id() {
        let service = test_service().await;
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
    async fn session_new_escapes_session_ids_in_queries() {
        let service = test_service().await;
        // A quote/backslash-rich id proves every interpolated value is
        // escaped rather than spliced raw into the GraphQL document.
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
        let service = test_service().await;
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
        assert!(
            response["error"]["message"]
                .as_str()
                .expect("message")
                .contains("bound catalog"),
            "rejection must name the bound catalog"
        );
    }

    #[tokio::test]
    async fn set_model_emits_models_update_and_rejects_unknown_model() {
        let service = test_service().await;
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
        assert!(
            response.get("error").is_some(),
            "unknown model must be rejected"
        );

        let dispatch = service
            .handle_acp_payload(&request_payload(
                "session/set_model",
                json!({
                    "sessionId": "s-set-model",
                    "modelId": "GLM-5.3-NVFP4",
                    "_meta": { "reasoningEffort": "high" },
                }),
            ))
            .await;
        assert_eq!(dispatch.notifications.len(), 1);
        let notification = parse_response(&dispatch.notifications[0]);
        assert_eq!(notification["method"], "x.ai/models/update");
        assert_eq!(
            notification["params"]["models"]["currentModelId"],
            "GLM-5.3-NVFP4"
        );
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert_eq!(response["result"], json!({}), "set_model result is empty");
    }

    #[tokio::test]
    async fn set_model_rejects_an_invalid_reasoning_effort() {
        let service = test_service().await;
        service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({ "cwd": "/tmp", "mcpServers": [], "_meta": { "sessionId": "s-effort" } }),
            ))
            .await;
        let dispatch = service
            .handle_acp_payload(&request_payload(
                "session/set_model",
                json!({
                    "sessionId": "s-effort",
                    "modelId": "GLM-5.3-NVFP4",
                    "_meta": { "reasoningEffort": "extreme" },
                }),
            ))
            .await;
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert!(
            response.get("error").is_some(),
            "an unparsable reasoningEffort must fail explicitly"
        );
    }

    #[tokio::test]
    async fn set_mode_emits_current_mode_update() {
        let service = test_service().await;
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
        assert_eq!(notification["params"]["update"]["currentMode"], "yolo");
        assert_eq!(notification["params"]["sessionId"], "s-set-mode");
        assert!(
            notification["params"]["_meta"]["eventId"]
                .as_str()
                .expect("eventId")
                .starts_with("s-set-mode-"),
            "eventId must be {{sessionId}}-{{counter}}"
        );
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert_eq!(response["result"], json!({}), "set_mode result is empty");
    }

    #[tokio::test]
    async fn event_ids_are_monotonic_per_session() {
        let service = test_service().await;
        assert_eq!(service.next_event_id("s-events").await, "s-events-1");
        assert_eq!(service.next_event_id("s-events").await, "s-events-2");
        assert_eq!(service.next_event_id("s-events").await, "s-events-3");
        // A different session has an independent counter.
        assert_eq!(service.next_event_id("s-other").await, "s-other-1");
        assert_eq!(service.next_event_id("s-events").await, "s-events-4");
    }

    #[tokio::test]
    async fn session_update_meta_carries_prompt_and_tokens_when_present() {
        let service = test_service().await;
        let meta = service
            .session_update_meta("s-meta", Some("prompt-1"), Some(128))
            .await;
        assert_eq!(meta["promptId"], "prompt-1");
        assert_eq!(meta["totalTokens"], 128);
        assert!(meta["eventId"].as_str().expect("eventId").contains('-'));

        // Omitted counts are omitted, never fabricated.
        let bare = service.session_update_meta("s-meta", None, None).await;
        assert!(bare.get("promptId").is_none());
        assert!(bare.get("totalTokens").is_none());
    }

    #[tokio::test]
    async fn set_model_and_set_mode_require_a_known_session() {
        let service = test_service().await;
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
    async fn session_load_interject_and_compact_answer_method_not_found() {
        let service = test_service().await;
        for (method, params) in [
            (
                "session/load",
                json!({ "sessionId": "s1", "cwd": "/tmp", "mcpServers": [] }),
            ),
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
        let service = test_service().await;
        service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({ "cwd": "/tmp", "mcpServers": [], "_meta": { "sessionId": "s-stubs" } }),
            ))
            .await;
        for (method, params) in [
            ("session/load", json!({ "sessionId": "s-stubs" })),
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
        let service = test_service().await;
        let dispatch = service
            .handle_acp_payload(&request_payload("terminal/create", json!({})))
            .await;
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert_eq!(response["error"]["code"], -32601);
        assert!(
            response["error"]["message"]
                .as_str()
                .expect("message")
                .contains("terminal/create"),
            "the unrouted method must be named in the error"
        );
    }

    #[tokio::test]
    async fn undecodable_payloads_answer_invalid_request() {
        let service = test_service().await;
        let dispatch = service.handle_acp_payload("not json").await;
        let response = parse_response(dispatch.response.as_deref().expect("response line"));
        assert_eq!(response["id"], Value::Null);
        assert_eq!(response["error"]["code"], -32600);
    }

    #[tokio::test]
    async fn notifications_never_produce_a_response_body() {
        let service = test_service().await;
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
