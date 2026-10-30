use anyhow::Result;
use serde_json::{json, Value};

use super::errors::{invalid_params, required_session_id};
use super::wire::{AcpRequest, RequestOutcome};
use super::AcpService;
use crate::commands::grok_shim::projection::caused_sessions::{
    SUBAGENT_CANCEL_METHOD, SUBAGENT_GET_METHOD, SUBAGENT_LIST_RUNNING_METHOD,
};

impl AcpService {
    pub(super) async fn handle_subagent_ext_request(
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
                    return Err(invalid_params(format!(
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
        let result = crate::commands::grok_shim::projection::caused_sessions::handle(
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
