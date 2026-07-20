mod cascade;
pub(crate) mod cause_derivation;
mod commands;
mod logging;
mod snapshot;
mod state;
mod tauri_commands;
#[cfg(test)]
mod tests;
mod types;

use std::sync::OnceLock;

use self::logging::init_tracing;
use self::state::DesktopAppState;

static TAURI_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

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
            tauri_commands::lifecycle::desktop_bootstrap_summary,
            tauri_commands::lifecycle::desktop_init_local_standard,
            tauri_commands::lifecycle::desktop_client_start,
            tauri_commands::lifecycle::desktop_client_shutdown,
            tauri_commands::peers::desktop_peer_add,
            tauri_commands::peers::desktop_peer_remove,
            tauri_commands::peers::desktop_peer_rename,
            tauri_commands::peers::desktop_peer_status_fetch,
            tauri_commands::peers::desktop_p2p_repair,
            tauri_commands::peers::desktop_network_status,
            tauri_commands::lifecycle::desktop_client_snapshot,
            tauri_commands::lifecycle::desktop_set_selected_agent,
            tauri_commands::lifecycle::desktop_observer_metrics,
            tauri_commands::chat::desktop_session_snapshot,
            tauri_commands::chat::desktop_request_timeline,
            tauri_commands::tools_explain::desktop_tool_surface_explain,
            tauri_commands::chat::desktop_chat_send,
            tauri_commands::chat::desktop_conversation_rename,
            tauri_commands::chat::desktop_session_fork,
            tauri_commands::chat::desktop_request_resend,
            tauri_commands::config::desktop_agent_config_save,
            tauri_commands::config::desktop_behavior_save,
            tauri_commands::config::desktop_skill_save,
            tauri_commands::config::desktop_skill_delete,
            tauri_commands::config::desktop_task_delete,
            tauri_commands::config::desktop_schedule_delete,
            tauri_commands::config::desktop_event_trigger_delete,
            tauri_commands::config::desktop_backend_delete,
            tauri_commands::config::desktop_inference_profile_delete,
            tauri_commands::config::desktop_tool_selection_delete,
            tauri_commands::config::desktop_tool_service_delete,
            tauri_commands::config::desktop_behavior_delete,
            tauri_commands::config::desktop_backend_save,
            tauri_commands::config::desktop_inference_profile_save,
            tauri_commands::config::desktop_tool_selection_save,
            tauri_commands::config::desktop_tool_service_save,
            tauri_commands::config::desktop_tool_service_test,
            tauri_commands::tasks::desktop_task_save,
            tauri_commands::tasks::desktop_schedule_save,
            tauri_commands::tasks::desktop_schedule_run,
            tauri_commands::tasks::desktop_event_trigger_save,
            tauri_commands::tasks::desktop_task_run,
            tauri_commands::operations::desktop_operations_snapshot,
            tauri_commands::operations::desktop_list_subagent_tree,
            tauri_commands::operations::desktop_preview_interrupt_cascade,
            tauri_commands::operations::desktop_interrupt_request,
            tauri_commands::operations::desktop_list_backends_with_health,
            tauri_commands::operations::desktop_list_mcp_services_with_health,
            tauri_commands::operations::desktop_probe_mcp_service
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
