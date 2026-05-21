use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use super::protocol::{HttpRequestData, HttpResponse};
use crate::bridge::cascade::{build_cascade_preview, interrupt_request};
use crate::bridge::commands::{
    add_peer, rename_conversation, repair_p2p, run_schedule_config, run_task_config,
    save_agent_config, save_backend_config, save_behavior_config, save_event_trigger_config,
    save_inference_profile_config, save_schedule_config, save_task_config,
    save_tool_selection_config, save_tool_service_config, send_chat_message,
    test_tool_service_config,
};
use crate::bridge::types::{
    AgentConfigSaveRequest, BackendSaveRequest, BehaviorSaveRequest, ChatSendRequest,
    ConversationRenameRequest, DesktopInterruptRequest, DesktopPreviewInterruptCascadeRequest,
    EventTriggerSaveRequest, InferenceProfileSaveRequest, PeerAddRequest, ScheduleRunRequest,
    ScheduleSaveRequest, TaskRunRequest, TaskSaveRequest, ToolSelectionSaveRequest,
    ToolServiceSaveRequest, ToolServiceTestRequest,
};
use crate::diagnostics::{
    build_desktop_client_snapshot, build_desktop_session_snapshot, build_request_diagnostics_bundle,
};
use crate::live_fixture::LiveBridgeFixture;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionSnapshotRequest {
    #[serde(default)]
    agent_did: Option<String>,
    session_id: String,
    request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelectedAgentRequest {
    #[serde(default)]
    agent_did: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct VersionResponse {
    version: u64,
}

pub(super) fn handle_request(
    runtime: &tokio::runtime::Handle,
    fixture: &Arc<LiveBridgeFixture>,
    request: HttpRequestData,
) -> Result<HttpResponse> {
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/health") => Ok(HttpResponse::json_ok(
            serde_json::json!({ "status": "ok" }).to_string(),
        )),
        ("GET", "/desktop/version") => Ok(HttpResponse::json_ok(serde_json::to_string(
            &VersionResponse {
                version: fixture.update_version(),
            },
        )?)),
        ("GET", "/desktop/client/snapshot") => {
            let snapshot = runtime.block_on(build_desktop_client_snapshot(fixture));
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/init") => Ok(HttpResponse::json_ok(serde_json::to_string(
            &fixture.init_summary(),
        )?)),
        ("POST", "/desktop/client/start") => {
            let snapshot = runtime.block_on(build_desktop_client_snapshot(fixture));
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/client/shutdown") => Ok(HttpResponse::json_ok(
            serde_json::json!({
                "bootstrap": runtime.block_on(fixture.build_bootstrap_summary()),
                "client": serde_json::Value::Null,
            })
            .to_string(),
        )),
        ("POST", "/desktop/selected-agent") => {
            let request = serde_json::from_str::<SelectedAgentRequest>(&request.body)
                .context("decoding selected agent request")?;
            let did = request
                .agent_did
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
            let core = fixture.desktop_core();
            core.set_selected_agent_did(did.clone());
            if let Some(did) = did {
                runtime.block_on(core.ensure_agent_loaded(&did))?;
            }
            Ok(HttpResponse::json_ok(serde_json::json!({}).to_string()))
        }
        ("POST", "/desktop/peer/add") => {
            let request = decode::<PeerAddRequest>(&request.body, "decoding peer add request")?;
            runtime.block_on(add_peer(fixture.desktop_core().as_ref(), request))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/p2p/repair") => {
            runtime.block_on(repair_p2p(
                fixture.desktop_core().as_ref(),
                Duration::from_millis(250),
            ))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/session/snapshot") => {
            let request = serde_json::from_str::<SessionSnapshotRequest>(&request.body)
                .context("decoding session snapshot request")?;
            let snapshot = runtime.block_on(build_desktop_session_snapshot(
                fixture,
                request.agent_did.as_deref(),
                &request.session_id,
                request.request_id.as_deref(),
            ));
            Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
        }
        ("POST", "/desktop/request/diagnostics") => {
            let request = serde_json::from_str::<SessionSnapshotRequest>(&request.body)
                .context("decoding request diagnostics request")?;
            let request_id = request
                .request_id
                .as_deref()
                .ok_or_else(|| anyhow!("requestId is required"))?;
            let diagnostics = runtime.block_on(build_request_diagnostics_bundle(
                fixture,
                &request.session_id,
                request_id,
            ));
            Ok(HttpResponse::json_ok(serde_json::to_string(&diagnostics)?))
        }
        ("POST", "/desktop/chat/send") => {
            let request = decode::<ChatSendRequest>(&request.body, "decoding chat send request")?;
            let result =
                runtime.block_on(send_chat_message(fixture.desktop_core().as_ref(), request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/conversation/rename") => {
            let request =
                decode::<ConversationRenameRequest>(&request.body, "decoding rename request")?;
            runtime.block_on(rename_conversation(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(HttpResponse::json_ok(
                serde_json::json!({ "status": "ok" }).to_string(),
            ))
        }
        ("POST", "/desktop/agent/save") => {
            let request = decode::<AgentConfigSaveRequest>(
                &request.body,
                "decoding agent config save request",
            )?;
            runtime.block_on(save_agent_config(fixture.desktop_core().as_ref(), request))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/behavior/save") => {
            let request =
                decode::<BehaviorSaveRequest>(&request.body, "decoding behavior save request")?;
            runtime.block_on(save_behavior_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/backend/save") => {
            let request =
                decode::<BackendSaveRequest>(&request.body, "decoding backend save request")?;
            runtime.block_on(save_backend_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/inference-profile/save") => {
            let request = decode::<InferenceProfileSaveRequest>(
                &request.body,
                "decoding inference profile save request",
            )?;
            runtime.block_on(save_inference_profile_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/tool-selection/save") => {
            let request = decode::<ToolSelectionSaveRequest>(
                &request.body,
                "decoding tool selection save request",
            )?;
            runtime.block_on(save_tool_selection_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/tool-service/save") => {
            let request = decode::<ToolServiceSaveRequest>(
                &request.body,
                "decoding tool service save request",
            )?;
            runtime.block_on(save_tool_service_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/tool-service/test") => {
            let request = decode::<ToolServiceTestRequest>(
                &request.body,
                "decoding tool service test request",
            )?;
            let result = runtime.block_on(test_tool_service_config(request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/task/save") => {
            let request = decode::<TaskSaveRequest>(&request.body, "decoding task save request")?;
            runtime.block_on(save_task_config(fixture.desktop_core().as_ref(), request))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/schedule/save") => {
            let request =
                decode::<ScheduleSaveRequest>(&request.body, "decoding schedule save request")?;
            runtime.block_on(save_schedule_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/schedule/run") => {
            let request =
                decode::<ScheduleRunRequest>(&request.body, "decoding schedule run request")?;
            let result = runtime.block_on(run_schedule_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/event-trigger/save") => {
            let request = decode::<EventTriggerSaveRequest>(
                &request.body,
                "decoding event trigger save request",
            )?;
            runtime.block_on(save_event_trigger_config(
                fixture.desktop_core().as_ref(),
                request,
            ))?;
            Ok(snapshot_response(runtime, fixture)?)
        }
        ("POST", "/desktop/task/run") => {
            let request = decode::<TaskRunRequest>(&request.body, "decoding task run request")?;
            let result =
                runtime.block_on(run_task_config(fixture.desktop_core().as_ref(), request))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/interrupt/preview") => {
            let request = decode::<DesktopPreviewInterruptCascadeRequest>(
                &request.body,
                "decoding interrupt preview request",
            )?;
            let result = runtime
                .block_on(build_cascade_preview(fixture.desktop_core(), &request))
                .map_err(|e| anyhow!("{e}"))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        ("POST", "/desktop/interrupt/request") => {
            let request =
                decode::<DesktopInterruptRequest>(&request.body, "decoding interrupt request")?;
            let result = runtime
                .block_on(interrupt_request(fixture.desktop_core(), &request))
                .map_err(|e| anyhow!("{e}"))?;
            Ok(HttpResponse::json_ok(serde_json::to_string(&result)?))
        }
        _ => Ok(HttpResponse::json_error("404 Not Found", "not found")),
    }
}

fn decode<T: serde::de::DeserializeOwned>(body: &str, context: &str) -> Result<T> {
    serde_json::from_str::<T>(body).context(context.to_string())
}

fn snapshot_response(
    runtime: &tokio::runtime::Handle,
    fixture: &Arc<LiveBridgeFixture>,
) -> Result<HttpResponse> {
    let snapshot = runtime.block_on(build_desktop_client_snapshot(fixture));
    Ok(HttpResponse::json_ok(serde_json::to_string(&snapshot)?))
}
