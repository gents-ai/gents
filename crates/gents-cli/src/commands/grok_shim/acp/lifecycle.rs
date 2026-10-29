use anyhow::Result;
use serde_json::{json, Value};

use super::errors::{invalid_params, required_session_id};
use super::session_document::ensure_session_document;
use super::session_state::{AcpSessionState, ModeStateCommit, SessionLoadReservation};
use super::wire::{AcpRequest, RequestOutcome};
use super::{
    AcpService, ACP_PROTOCOL_VERSION, GENTS_AUTH_METHOD_ID, MCP_INITIALIZED_METHOD,
    MODELS_UPDATE_METHOD, SESSION_LOAD_METHOD,
};
use crate::commands::grok_shim::projection::{
    stamp_update_meta, UpdateTimestamps, SESSION_UPDATE_METHOD,
};
use crate::commands::grok_shim::turn::sender::{PromptSender, PromptSenderLine};

impl AcpService {
    pub(super) async fn handle_initialize(&self, request: &AcpRequest) -> Result<RequestOutcome> {
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

    pub(super) async fn handle_authenticate(&self, request: &AcpRequest) -> Result<RequestOutcome> {
        let method_id = request
            .params
            .get("methodId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if method_id != GENTS_AUTH_METHOD_ID {
            return Err(invalid_params(format!(
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

    pub(super) async fn handle_session_new(&self, request: &AcpRequest) -> Result<RequestOutcome> {
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
                return Err(invalid_params(format!(
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
        match crate::commands::grok_shim::session_record::write_record(grok_home, cwd, session_id) {
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

    pub(super) async fn handle_session_load(
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
        let rows = crate::commands::grok_shim::sessions::load(
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

    pub(super) async fn activate_loaded_session(&self, payload: &str, sender: &PromptSender) {
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

    pub(super) async fn handle_session_set_model(
        &self,
        request: &AcpRequest,
    ) -> Result<RequestOutcome> {
        let session_id = required_session_id(&request.params)?;
        let requested = request
            .params
            .get("modelId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid_params("session/set_model requires a non-empty modelId"))?;
        if requested != self.config.current_model.model_id {
            return Err(invalid_params(format!(
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
            return Err(invalid_params(format!(
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

    pub(super) async fn handle_session_set_mode(
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
}
