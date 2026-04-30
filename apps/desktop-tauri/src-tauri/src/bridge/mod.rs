mod snapshot;
mod state;
mod types;

use std::fs::OpenOptions;
use std::path::Path;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use defra_agent::graphql::escape_graphql_string;
use defra_agent::mcp_pool::{resolve_mcp_url, McpPool};
use defra_agent_desktop_core::client::{ClientCore, DesktopPaths};
use defra_agent_desktop_core::local_runtime::{
    dangerously_overwrite_desktop_home, default_agent_home, fetch_runtime_connection_payload,
    init_standard_local_runtime, reset_desktop_runtime_state, DesktopInitOptions,
    DesktopInitSummary,
};
use defra_agent_protocol::client_protocol::ClientTurnState;
use defra_agent_protocol::row::{
    AgentBehaviorRow, AgentPrincipalRow, AgentRequestRow, EventTriggerRow, InferenceBackendRow,
    InferenceProfileRow, ScheduleRow, TaskRow, ToolSelectionRow, ToolServiceRegistryRow,
};
use tauri::{AppHandle, Emitter, State};
use tracing_subscriber::{prelude::*, EnvFilter};

use self::snapshot::{
    build_bootstrap_summary, build_client_snapshot, build_session_snapshot_from_store_for_agent,
};
use self::state::{current_core, spawn_client_update_task, DesktopAppState};
use self::types::{
    AgentConfigSaveRequest, BackendSaveRequest, BehaviorSaveRequest, ChatSendRequest,
    ChatSendResult, ClientUpdateEvent, ConversationRenameRequest, DesktopBootstrapSummary,
    DesktopClientSnapshot, DesktopInitRequest, DesktopSessionSnapshot, EventTriggerSaveRequest,
    InferenceProfileSaveRequest, PeerAddRequest, PeerStatusFetchRequest, ScheduleRunRequest,
    ScheduleSaveRequest, TaskRunRequest, TaskRunResult, TaskSaveRequest, ToolSelectionSaveRequest,
    ToolServiceSaveRequest, ToolServiceTestRequest, ToolServiceTestResult, ToolServiceToolView,
};

static TAURI_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

fn can_send_in_turn(state: ClientTurnState) -> bool {
    matches!(
        state,
        ClientTurnState::Completed
            | ClientTurnState::Failed
            | ClientTurnState::Superseded
            | ClientTurnState::Interrupted
    )
}

fn turn_state_label(state: ClientTurnState) -> &'static str {
    match state {
        ClientTurnState::WaitingForClaim => "waitingForClaim",
        ClientTurnState::Streaming => "streaming",
        ClientTurnState::Completed => "completed",
        ClientTurnState::Failed => "failed",
        ClientTurnState::Superseded => "superseded",
        ClientTurnState::Interrupted => "interrupted",
    }
}

fn trim_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn require_trimmed(name: &str, value: impl AsRef<str>) -> Result<String, String> {
    let value = value.as_ref().trim().to_string();
    if value.is_empty() {
        Err(format!("{name} is required"))
    } else {
        Ok(value)
    }
}

fn validate_event_kind(event_kind: &str) -> Result<(), String> {
    if event_kind == "created" {
        Ok(())
    } else {
        Err("event_kind currently supports only created".to_string())
    }
}

fn resolve_tool_service_endpoint(request: &ToolServiceTestRequest) -> Result<String, String> {
    let mcp_port = request
        .mcp_port
        .ok_or_else(|| "mcp_port is required".to_string())?;
    if !(1..=u16::MAX as i64).contains(&mcp_port) {
        return Err("mcp_port must be between 1 and 65535".to_string());
    }
    let hostname = trim_optional(request.hostname.clone()).unwrap_or_default();
    let tailscale_ip = trim_optional(request.tailscale_ip.clone()).unwrap_or_default();
    let lan_ip = trim_optional(request.lan_ip.clone()).unwrap_or_default();
    if hostname.is_empty() && tailscale_ip.is_empty() && lan_ip.is_empty() {
        return Err("hostname, tailscale_ip, or lan_ip is required".to_string());
    }
    Ok(resolve_mcp_url(
        &hostname,
        &tailscale_ip,
        &lan_ip,
        mcp_port as u16,
        request.mcp_path.as_deref().unwrap_or("/mcp"),
        "",
        None,
    ))
}

async fn emit_config_update_and_snapshot(
    app: &AppHandle,
    core: &Arc<ClientCore>,
) -> Result<DesktopClientSnapshot, String> {
    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent { reason: "config" },
    );
    build_client_snapshot(Some(core)).await
}

async fn load_agent_request_by_doc_id(
    core: &ClientCore,
    request_doc_id: &str,
) -> Result<AgentRequestRow, String> {
    let escaped_doc_id = escape_graphql_string(request_doc_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}, limit: 1) {{
                request_id
                agent_did
                behavior_id
                session_id
                status
                lifecycle_state
            }}
        }}"#
    );
    let response = core.node().execute(&query).await;
    if response.has_errors() {
        return Err(format!(
            "query manual task run request failed: {:?}",
            response.errors
        ));
    }

    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .ok_or_else(|| format!("manual task run request {request_doc_id} was not found"))?;
    serde_json::from_value(row).map_err(|error| error.to_string())
}

#[tauri::command]
fn desktop_bootstrap_summary() -> Result<DesktopBootstrapSummary, String> {
    tauri::async_runtime::block_on(build_bootstrap_summary())
}

#[tauri::command]
fn desktop_init_local_standard(request: DesktopInitRequest) -> Result<DesktopInitSummary, String> {
    tauri::async_runtime::block_on(async move {
        let agent_home = match request.agent_home {
            Some(path) => path,
            None => default_agent_home().map_err(|error| error.to_string())?,
        };
        let desktop_paths = match request.desktop_home {
            Some(root) => DesktopPaths::from_root(root),
            None => DesktopPaths::discover().map_err(|error| error.to_string())?,
        };

        if request.dangerously_overwrite {
            dangerously_overwrite_desktop_home(desktop_paths.root())
                .map_err(|error| error.to_string())?;
        } else if request.reset {
            let _ =
                reset_desktop_runtime_state(&desktop_paths).map_err(|error| error.to_string())?;
        }

        init_standard_local_runtime(DesktopInitOptions {
            agent_home,
            desktop_paths,
            label: request
                .label
                .filter(|label| !label.trim().is_empty())
                .unwrap_or_else(|| "Local Agent".to_string()),
        })
        .await
        .map_err(|error| error.to_string())
    })
}

#[tauri::command]
fn desktop_client_start(
    app: AppHandle,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    if let Some(core) = current_core(&state) {
        return tauri::async_runtime::block_on(build_client_snapshot(Some(&core)));
    }

    let core = Arc::new(
        tauri::async_runtime::block_on(ClientCore::start()).map_err(|error| error.to_string())?,
    );
    let updates_task = spawn_client_update_task(app.clone(), Arc::clone(&core));

    {
        let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
        bridge.core = Some(Arc::clone(&core));
        bridge.updates_task = Some(updates_task);
    }

    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent {
            reason: "lifecycle",
        },
    );

    tauri::async_runtime::block_on(build_client_snapshot(Some(&core)))
}

#[tauri::command]
fn desktop_client_shutdown(
    app: AppHandle,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let (core, updates_task) = {
        let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
        (bridge.core.take(), bridge.updates_task.take())
    };

    if let Some(task) = updates_task {
        task.abort();
    }

    if let Some(core) = core {
        tauri::async_runtime::block_on(core.shutdown()).map_err(|error| error.to_string())?;
    }

    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent {
            reason: "lifecycle",
        },
    );

    tauri::async_runtime::block_on(build_client_snapshot(None))
}

#[tauri::command]
fn desktop_peer_add(
    app: AppHandle,
    request: PeerAddRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    let label = require_trimmed("label", request.label)?;
    let agent_did = require_trimmed("agent_did", request.agent_did)?;
    let addr = require_trimmed("addr", request.addr)?;
    let graphql = trim_optional(request.graphql);

    tauri::async_runtime::block_on(async move {
        core.add_peer(&label, &addr, &agent_did, graphql.as_deref())
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
fn desktop_peer_status_fetch(request: PeerStatusFetchRequest) -> Result<serde_json::Value, String> {
    tauri::async_runtime::block_on(async move {
        fetch_runtime_connection_payload(&request.server_address)
            .await
            .map_err(|error| error.to_string())
    })
}

#[tauri::command]
fn desktop_p2p_repair(
    app: AppHandle,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        core.request_p2p_repair()
            .await
            .map_err(|error| error.to_string())?;
        tokio::time::sleep(Duration::from_millis(250)).await;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
fn desktop_client_snapshot(
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let core = current_core(&state);
    tauri::async_runtime::block_on(build_client_snapshot(core.as_ref()))
}

#[tauri::command]
fn desktop_session_snapshot(
    session_id: String,
    agent_did: Option<String>,
    request_id: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<Option<DesktopSessionSnapshot>, String> {
    let Some(core) = current_core(&state) else {
        return Ok(None);
    };

    let snapshot = tauri::async_runtime::block_on(async move { core.store().snapshot() });
    Ok(build_session_snapshot_from_store_for_agent(
        snapshot.as_ref(),
        agent_did.as_deref(),
        &session_id,
        request_id.as_deref(),
    ))
}

#[tauri::command]
fn desktop_chat_send(
    request: ChatSendRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ChatSendResult, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    let agent_did = request.agent_did.trim().to_string();
    if agent_did.is_empty() {
        return Err("agent_did is required".to_string());
    }

    let content = request.content.trim().to_string();
    if content.is_empty() {
        return Err("content is required".to_string());
    }

    let behavior_id = request
        .behavior_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    tauri::async_runtime::block_on(async move {
        let session_id = match request
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(session_id) => session_id.to_string(),
            None => {
                core.create_conversation(&agent_did, behavior_id.as_deref())
                    .await
                    .map_err(|error| error.to_string())?
                    .session_id
            }
        };

        let store = core.store().snapshot();
        if let Some(turn_state) = store.derive_turn(&session_id) {
            if !can_send_in_turn(turn_state) {
                return Err(format!(
                    "cannot send while current turn is {}",
                    turn_state_label(turn_state)
                ));
            }
        }

        let submitted = core
            .submit_request(&session_id, &agent_did, &content, behavior_id.as_deref())
            .await
            .map_err(|error| error.to_string())?;

        Ok(ChatSendResult {
            session_id,
            request_id: submitted.request_id,
            agent_did: submitted.agent_did,
            behavior_id: submitted.behavior_id,
        })
    })
}

#[tauri::command]
fn desktop_conversation_rename(
    request: ConversationRenameRequest,
    state: State<'_, DesktopAppState>,
) -> Result<(), String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    let session_id = request.session_id.trim().to_string();
    if session_id.is_empty() {
        return Err("session_id is required".to_string());
    }

    let title = request.title.trim().to_string();
    if title.is_empty() {
        return Err("title is required".to_string());
    }

    tauri::async_runtime::block_on(async move {
        core.rename_conversation(&session_id, &title)
            .await
            .map_err(|error| error.to_string())
    })
}

#[tauri::command]
fn desktop_agent_config_save(
    app: AppHandle,
    request: AgentConfigSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    let agent_did = require_trimmed("agent_did", request.agent_did)?;
    let display_name = require_trimmed("display_name", request.display_name)?;
    let default_behavior_id = require_trimmed("default_behavior_id", request.default_behavior_id)?;

    tauri::async_runtime::block_on(async move {
        let store = core.store().snapshot();
        let default_behavior_exists = store.behaviors.iter().any(|behavior| {
            behavior.agent_did.as_deref() == Some(agent_did.as_str())
                && behavior.behavior_id == default_behavior_id
        });
        if !default_behavior_exists {
            return Err(format!(
                "default_behavior_id {default_behavior_id} does not exist for {agent_did}"
            ));
        }

        let mut row = store
            .agent_principals
            .iter()
            .find(|row| row.agent_did == agent_did)
            .cloned()
            .unwrap_or_else(|| AgentPrincipalRow {
                agent_did: agent_did.clone(),
                display_name: None,
                default_behavior_id: None,
                enabled: Some(true),
                created_at: None,
                created_by: Some(agent_did.clone()),
            });
        row.display_name = Some(display_name);
        row.default_behavior_id = Some(default_behavior_id);
        row.enabled = Some(request.enabled.unwrap_or(true));
        core.save_agent_principal(&row)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
fn desktop_behavior_save(
    app: AppHandle,
    request: BehaviorSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    let agent_did = require_trimmed("agent_did", request.agent_did)?;
    let behavior_id = require_trimmed("behavior_id", request.behavior_id)?;
    let display_name = require_trimmed("display_name", request.display_name)?;

    tauri::async_runtime::block_on(async move {
        let store = core.store().snapshot();
        let mut row = store
            .behavior_row(&agent_did, &behavior_id)
            .cloned()
            .unwrap_or_else(|| AgentBehaviorRow {
                behavior_id: behavior_id.clone(),
                agent_did: Some(agent_did.clone()),
                display_name: None,
                system_prompt: None,
                backend_id: None,
                model_name: None,
                tool_selection_id: None,
                inference_profile_id: None,
                compaction_strategy: None,
                compaction_threshold: None,
                enabled: Some(true),
                created_at: None,
            });
        let inference_profile_id = trim_optional(request.inference_profile_id)
            .ok_or_else(|| "inference_profile_id is required".to_string())?;
        if !store
            .inference_profiles
            .iter()
            .any(|profile| profile.profile_id == inference_profile_id)
        {
            return Err(format!(
                "inference_profile_id {inference_profile_id} does not exist"
            ));
        }
        row.display_name = Some(display_name);
        row.agent_did = Some(agent_did);
        row.system_prompt = Some(request.system_prompt);
        row.backend_id = trim_optional(request.backend_id);
        row.tool_selection_id = trim_optional(request.tool_selection_id);
        row.inference_profile_id = Some(inference_profile_id);
        row.compaction_strategy = trim_optional(request.compaction_strategy);
        row.compaction_threshold = request.compaction_threshold;
        row.enabled = request.enabled.or(row.enabled).or(Some(true));
        if let Some(backend_id) = row.backend_id.as_deref() {
            if let Some(model_name) = store
                .inference_backends
                .iter()
                .find(|backend| backend.backend_id == backend_id)
                .and_then(|backend| backend.models.first())
                .cloned()
            {
                row.model_name = Some(model_name);
            }
        }
        core.save_behavior(&row)
            .await
            .map_err(|error| error.to_string())?;

        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
fn desktop_backend_save(
    app: AppHandle,
    request: BackendSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    let backend_id = require_trimmed("backend_id", request.backend_id)?;
    let name = require_trimmed("name", request.name)?;
    let provider_kind = require_trimmed("provider_kind", request.provider_kind)?;
    let endpoint = require_trimmed("endpoint", request.endpoint)?;
    let models = request
        .models
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if models.is_empty() {
        return Err("at least one model is required".to_string());
    }

    tauri::async_runtime::block_on(async move {
        let store = core.store().snapshot();
        let mut row = store
            .inference_backends
            .iter()
            .find(|row| row.backend_id == backend_id)
            .cloned()
            .unwrap_or_else(|| InferenceBackendRow {
                backend_id: backend_id.clone(),
                name: None,
                provider_kind: None,
                endpoint: None,
                api_key: None,
                api_key_env_var: None,
                max_concurrent: None,
                max_queue_depth: None,
                enabled: Some(true),
                models: Vec::new(),
                last_probe: None,
                probe_status: None,
            });
        row.name = Some(name);
        row.provider_kind = Some(provider_kind);
        row.endpoint = Some(endpoint);
        if request.clear_api_key.unwrap_or(false) {
            row.api_key = None;
        } else if request.api_key.is_some() {
            row.api_key = trim_optional(request.api_key);
        }
        if request.api_key_env_var.is_some() {
            row.api_key_env_var = trim_optional(request.api_key_env_var);
        }
        row.models = models;
        row.max_concurrent = request.max_concurrent;
        row.max_queue_depth = request.max_queue_depth;
        row.enabled = request.enabled.or(row.enabled).or(Some(true));
        row.probe_status = Some("healthy".to_string());
        core.save_backend(&row)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
fn desktop_inference_profile_save(
    app: AppHandle,
    request: InferenceProfileSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    let profile_id = require_trimmed("profile_id", request.profile_id)?;
    let display_name = require_trimmed("display_name", request.display_name)?;

    tauri::async_runtime::block_on(async move {
        let store = core.store().snapshot();
        let mut row = store
            .inference_profiles
            .iter()
            .find(|row| row.profile_id == profile_id)
            .cloned()
            .unwrap_or_else(|| InferenceProfileRow {
                profile_id: profile_id.clone(),
                display_name: None,
                context_window: None,
                max_output_tokens: None,
                max_turns: None,
                temperature: None,
                stream_batch_ms: None,
                deadline_duration_secs: None,
            });
        row.display_name = Some(display_name);
        row.context_window = request.context_window;
        row.max_output_tokens = request.max_output_tokens;
        row.max_turns = request.max_turns;
        row.temperature = request.temperature;
        row.stream_batch_ms = request.stream_batch_ms;
        row.deadline_duration_secs = request.deadline_duration_secs;
        core.save_inference_profile(&row)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
fn desktop_tool_selection_save(
    app: AppHandle,
    request: ToolSelectionSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    let agent_did = require_trimmed("agent_did", request.agent_did)?;
    let selection_id = require_trimmed("selection_id", request.selection_id)?;
    let display_name = require_trimmed("display_name", request.display_name)?;

    tauri::async_runtime::block_on(async move {
        let store = core.store().snapshot();
        let mut row = store
            .tool_selections
            .iter()
            .find(|row| row.selection_id == selection_id)
            .cloned()
            .unwrap_or_else(|| ToolSelectionRow {
                selection_id: selection_id.clone(),
                agent_did: Some(agent_did.clone()),
                display_name: None,
                enable_file_tools: Some(false),
                file_tools_mode: None,
                file_tool_root: None,
                enable_bash: Some(false),
                bash_mode: None,
                cli_tool_names: Vec::new(),
                enable_meta_tools: Some(false),
                delegate_to: Vec::new(),
            });
        row.agent_did = Some(agent_did);
        row.display_name = Some(display_name);
        row.enable_file_tools = request.enable_file_tools.or(row.enable_file_tools);
        row.file_tools_mode = trim_optional(request.file_tools_mode);
        row.file_tool_root = trim_optional(request.file_tool_root);
        row.enable_bash = request.enable_bash.or(row.enable_bash);
        row.bash_mode = trim_optional(request.bash_mode);
        row.cli_tool_names = request
            .cli_tool_names
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect();
        row.enable_meta_tools = request.enable_meta_tools.or(row.enable_meta_tools);
        row.delegate_to = request
            .delegate_to
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect();
        core.save_tool_selection(&row)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
fn desktop_tool_service_save(
    app: AppHandle,
    request: ToolServiceSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    let service_id = require_trimmed("service_id", request.service_id)?;
    let display_name = require_trimmed("display_name", request.display_name)?;

    tauri::async_runtime::block_on(async move {
        let store = core.store().snapshot();
        let mut row = store
            .tool_service_registries
            .iter()
            .find(|row| row.service_id == service_id)
            .cloned()
            .unwrap_or_else(|| ToolServiceRegistryRow {
                service_id: service_id.clone(),
                display_name: None,
                description: None,
                hostname: None,
                tailscale_ip: None,
                lan_ip: None,
                mcp_port: None,
                mcp_path: Some("/mcp".to_string()),
                tools: Vec::new(),
                status: Some("online".to_string()),
                version: None,
                updated_at: None,
            });
        row.display_name = Some(display_name);
        row.description = trim_optional(request.description);
        row.hostname = trim_optional(request.hostname);
        row.tailscale_ip = trim_optional(request.tailscale_ip);
        row.lan_ip = trim_optional(request.lan_ip);
        row.mcp_port = request.mcp_port;
        row.mcp_path = trim_optional(request.mcp_path).or_else(|| Some("/mcp".to_string()));
        row.status = trim_optional(request.status)
            .or_else(|| row.status.clone())
            .or_else(|| Some("online".to_string()));
        core.save_tool_service_registry(&row)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
fn desktop_tool_service_test(
    request: ToolServiceTestRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ToolServiceTestResult, String> {
    let _ = current_core(&state).ok_or_else(|| "desktop client is not running".to_string())?;
    let service_id = require_trimmed("service_id", request.service_id.clone())?;
    let endpoint = resolve_tool_service_endpoint(&request)?;

    tauri::async_runtime::block_on(async move {
        let pool = McpPool::new();
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            pool.list_tools(&service_id, &endpoint),
        )
        .await
        .map_err(|_| "MCP list_tools timed out".to_string())?
        .map_err(|error| error.to_string())?;
        let tools = result
            .tools
            .iter()
            .map(|tool| ToolServiceToolView {
                name: tool.name.to_string(),
                description: tool.description.as_deref().map(str::to_owned),
            })
            .collect::<Vec<_>>();
        Ok(ToolServiceTestResult {
            service_id,
            endpoint,
            status: "ok".to_string(),
            tool_count: tools.len(),
            tools,
            error: None,
        })
    })
}

#[tauri::command]
fn desktop_task_save(
    app: AppHandle,
    request: TaskSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    let task_id = require_trimmed("task_id", request.task_id)?;
    let name = require_trimmed("name", request.name)?;
    let behavior_id = require_trimmed("behavior_id", request.behavior_id)?;
    let prompt_template = require_trimmed("prompt_template", request.prompt_template)?;

    tauri::async_runtime::block_on(async move {
        let store = core.store().snapshot();
        let mut row = store
            .tasks
            .iter()
            .find(|row| row.task_id == task_id)
            .cloned()
            .unwrap_or_else(|| TaskRow {
                task_id: task_id.clone(),
                name: None,
                description: None,
                behavior_id: None,
                prompt_template: None,
                enabled: Some(true),
                output_schema_ref: None,
                created_at: None,
                updated_at: None,
            });
        row.name = Some(name);
        row.description = trim_optional(request.description);
        row.behavior_id = Some(behavior_id);
        row.prompt_template = Some(prompt_template);
        row.enabled = request.enabled.or(row.enabled).or(Some(true));
        row.output_schema_ref = trim_optional(request.output_schema_ref);
        core.save_task(&row)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
fn desktop_schedule_save(
    app: AppHandle,
    request: ScheduleSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    let schedule_id = require_trimmed("schedule_id", request.schedule_id)?;
    let task_id = require_trimmed("task_id", request.task_id)?;

    tauri::async_runtime::block_on(async move {
        let store = core.store().snapshot();
        let mut row = store
            .schedules
            .iter()
            .find(|row| row.schedule_id == schedule_id)
            .cloned()
            .unwrap_or_else(|| ScheduleRow {
                schedule_id: schedule_id.clone(),
                task_id: Some(task_id.clone()),
                interval_secs: None,
                enabled: Some(true),
                concurrency: Some("serial".to_string()),
                next_run_at: None,
                last_attempt_at: None,
                last_status: None,
                last_error: None,
                fire_count: None,
                created_at: None,
                updated_at: None,
            });
        row.task_id = Some(task_id);
        row.interval_secs = request.interval_secs;
        row.enabled = request.enabled.or(row.enabled).or(Some(true));
        row.concurrency = trim_optional(request.concurrency).or_else(|| Some("serial".to_string()));
        core.save_schedule(&row)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
fn desktop_schedule_run(
    app: AppHandle,
    request: ScheduleRunRequest,
    state: State<'_, DesktopAppState>,
) -> Result<TaskRunResult, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    let schedule_id = require_trimmed("schedule_id", request.schedule_id)?;

    tauri::async_runtime::block_on(async move {
        let store = core.store().snapshot();
        let schedule = store
            .schedules
            .iter()
            .find(|row| row.schedule_id == schedule_id)
            .cloned()
            .ok_or_else(|| format!("schedule {schedule_id} was not found"))?;
        let request_doc_id = core
            .fire_schedule_now(&schedule)
            .await
            .map_err(|error| error.to_string())?;
        let row = load_agent_request_by_doc_id(core.as_ref(), &request_doc_id).await?;
        let _ = app.emit(
            "desktop://client-updated",
            ClientUpdateEvent { reason: "config" },
        );
        Ok(TaskRunResult {
            request_doc_id,
            request_id: row.request_id,
            session_id: row.session_id.unwrap_or_default(),
            agent_did: row.agent_did.unwrap_or_default(),
            behavior_id: row.behavior_id.unwrap_or_default(),
            status: row.status,
            lifecycle_state: row.lifecycle_state,
        })
    })
}

#[tauri::command]
fn desktop_event_trigger_save(
    app: AppHandle,
    request: EventTriggerSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    let trigger_id = require_trimmed("trigger_id", request.trigger_id)?;
    let task_id = require_trimmed("task_id", request.task_id)?;
    let source_collection = require_trimmed("source_collection", request.source_collection)?;
    let event_kind = require_trimmed("event_kind", request.event_kind)?;
    validate_event_kind(&event_kind)?;

    tauri::async_runtime::block_on(async move {
        let store = core.store().snapshot();
        let mut row = store
            .event_triggers
            .iter()
            .find(|row| row.trigger_id == trigger_id)
            .cloned()
            .unwrap_or_else(|| EventTriggerRow {
                trigger_id: trigger_id.clone(),
                task_id: Some(task_id.clone()),
                source_collection: None,
                event_kind: None,
                filter: None,
                enabled: Some(true),
                concurrency: Some("serial".to_string()),
                created_at: None,
                updated_at: None,
                last_attempt_at: None,
                last_fired_source_doc_id: None,
                last_status: None,
                last_error: None,
                fire_count: None,
            });
        row.task_id = Some(task_id);
        row.source_collection = Some(source_collection);
        row.event_kind = Some(event_kind);
        row.filter = trim_optional(request.filter);
        row.enabled = request.enabled.or(row.enabled).or(Some(true));
        row.concurrency = trim_optional(request.concurrency).or_else(|| Some("serial".to_string()));
        core.save_event_trigger(&row)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
fn desktop_task_run(
    app: AppHandle,
    request: TaskRunRequest,
    state: State<'_, DesktopAppState>,
) -> Result<TaskRunResult, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    let task_id = require_trimmed("task_id", request.task_id)?;
    let args = request.args.unwrap_or_else(|| serde_json::json!({}));

    tauri::async_runtime::block_on(async move {
        let store = core.store().snapshot();
        let task = store
            .tasks
            .iter()
            .find(|row| row.task_id == task_id)
            .cloned()
            .ok_or_else(|| format!("task {task_id} was not found"))?;
        let request_doc_id = core
            .fire_task_now(&task, args)
            .await
            .map_err(|error| error.to_string())?;
        let row = load_agent_request_by_doc_id(core.as_ref(), &request_doc_id).await?;
        let _ = app.emit(
            "desktop://client-updated",
            ClientUpdateEvent { reason: "config" },
        );
        Ok(TaskRunResult {
            request_doc_id,
            request_id: row.request_id,
            session_id: row.session_id.unwrap_or_default(),
            agent_did: row.agent_did.unwrap_or_default(),
            behavior_id: row.behavior_id.unwrap_or_default(),
            status: row.status,
            lifecycle_state: row.lifecycle_state,
        })
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_tracing();
    let runtime = TAURI_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            // Merge replay can recurse deeply through collection heads. Give Tokio
            // worker threads enough stack to reach the merge depth guard instead of
            // aborting the whole desktop process with a native stack overflow.
            .thread_stack_size(32 * 1024 * 1024)
            .build()
            .expect("failed to build Tauri Tokio runtime")
    });
    tauri::async_runtime::set(runtime.handle().clone());

    tauri::Builder::default()
        .manage(DesktopAppState::default())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            desktop_bootstrap_summary,
            desktop_init_local_standard,
            desktop_client_start,
            desktop_client_shutdown,
            desktop_peer_add,
            desktop_peer_status_fetch,
            desktop_p2p_repair,
            desktop_client_snapshot,
            desktop_session_snapshot,
            desktop_chat_send,
            desktop_conversation_rename,
            desktop_agent_config_save,
            desktop_behavior_save,
            desktop_backend_save,
            desktop_inference_profile_save,
            desktop_tool_selection_save,
            desktop_tool_service_save,
            desktop_tool_service_test,
            desktop_task_save,
            desktop_schedule_save,
            desktop_schedule_run,
            desktop_event_trigger_save,
            desktop_task_run
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        with_default_transport_noise_filters(EnvFilter::new(
            "warn,\
                 defra_agent_desktop_core=info,\
                 defra_agent_desktop_tauri=info,\
                 defra_agent=info,\
                 defra_node=info",
        ))
    });
    let log_path = DesktopPaths::discover()
        .map(|paths| paths.log_file_path())
        .unwrap_or_else(|_| std::env::temp_dir().join("defra-agent-desktop.log"));
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let writer_path = log_path.clone();

    if desktop_console_log_enabled() {
        let file_writer_path = writer_path.clone();
        let file_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_target(true)
            .with_writer(move || open_log_writer(&file_writer_path));
        let stderr_layer = tracing_subscriber::fmt::layer()
            .with_target(false)
            .compact()
            .without_time();
        let _ = tracing_subscriber::registry()
            .with(env_filter)
            .with(stderr_layer)
            .with(file_layer)
            .try_init();
    } else {
        let file_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_target(true)
            .with_writer(move || open_log_writer(&writer_path));
        let _ = tracing_subscriber::registry()
            .with(env_filter)
            .with(file_layer)
            .try_init();
    }

    tracing::info!(path = %log_path.display(), "desktop logs initialized");
}

fn desktop_console_log_enabled() -> bool {
    std::env::var("DEFRA_AGENT_DESKTOP_CONSOLE_LOG")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

fn open_log_writer(path: &Path) -> std::fs::File {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap_or_else(|_| {
            let fallback = std::env::temp_dir().join("defra-agent-desktop.log");
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&fallback)
                .expect("open fallback desktop log file")
        })
}

fn with_default_transport_noise_filters(filter: EnvFilter) -> EnvFilter {
    [
        "iroh=error",
        "iroh_net=error",
        "iroh_relay=error",
        "iroh_gossip=error",
        "iroh_blobs=error",
        "iroh_quinn=error",
        "iroh_quinn_proto=error",
        "iroh_quinn_proto::connection=error",
        "quinn=error",
        "quinn_proto=error",
        "quinn_udp=error",
        "netwatch=error",
        "noq_proto::connection=error",
        "p2p::sync::replication::loop_runner=off",
    ]
    .into_iter()
    .fold(filter, |filter, directive| {
        filter.add_directive(directive.parse().expect("valid tracing directive"))
    })
}

#[cfg(test)]
mod tests {
    use defra_agent_desktop_core::local_runtime::runtime_status_url;

    #[test]
    fn peer_status_url_accepts_bare_host_and_graphql_endpoint() {
        assert_eq!(
            runtime_status_url("127.0.0.1:9181").expect("bare host should normalize"),
            "http://127.0.0.1:9181/status"
        );
        assert_eq!(
            runtime_status_url("http://127.0.0.1:9181/api/v0/graphql")
                .expect("graphql endpoint should normalize"),
            "http://127.0.0.1:9181/status"
        );
    }
}
