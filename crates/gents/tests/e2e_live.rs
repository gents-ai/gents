mod support;

#[path = "e2e_live/backend_auth_live.rs"]
mod backend_auth_live;
#[path = "e2e_live/edit_file_live.rs"]
mod edit_file_live;
#[path = "e2e_live/goal_continuation_live.rs"]
mod goal_continuation_live;
#[path = "e2e_live/interrupt_live.rs"]
mod interrupt_live;
#[path = "e2e_live/lsp_live.rs"]
mod lsp_live;
#[path = "e2e_live/mailbox_live.rs"]
mod mailbox_live;
#[path = "e2e_live/p2p_admission_concurrent_live.rs"]
mod p2p_admission_concurrent_live;
#[path = "e2e_live/seed_live.rs"]
mod seed_live;
#[path = "e2e_live/session_message_live.rs"]
mod session_message_live;
#[path = "e2e_live/steward_loop_live.rs"]
mod steward_loop_live;

#[cfg(target_os = "macos")]
#[path = "e2e_live/artifact_compiler_live.rs"]
mod artifact_compiler_live;

#[path = "e2e_live/steering_live.rs"]
mod steering_live;

#[path = "e2e_runtime/disabled_agent_sweep.rs"]
mod disabled_agent_sweep;

#[path = "e2e_triggers/event_source_trigger_e2e.rs"]
mod event_source_trigger_e2e;

static P2P_E2E_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[path = "e2e_runtime/workspace_binding_recovery.rs"]
mod workspace_binding_recovery;

#[path = "e2e_live/mailbox_p2p_isolation.rs"]
mod mailbox_p2p_isolation;

#[path = "e2e_triggers/write_tool_trigger_e2e.rs"]
mod write_tool_trigger_e2e;

#[path = "e2e_live/background_live.rs"]
mod background_live;

#[path = "e2e_runtime/queued_user_messages.rs"]
mod queued_user_messages;

#[path = "e2e_runtime/field_recovery_turn.rs"]
mod field_recovery_turn;

#[path = "e2e_live/plugin_goal_live.rs"]
mod plugin_goal_live;
#[allow(dead_code)]
#[path = "e2e_background/plugin_job_support.rs"]
mod plugin_job_support;
#[path = "e2e_live/steering_delivery_live.rs"]
mod steering_delivery_live;

#[path = "e2e_live/rendered_request_capture_live.rs"]
mod rendered_request_capture_live;
#[path = "e2e_live/runtime_entry_live.rs"]
mod runtime_entry_live;

#[path = "e2e_live/plugin_surface_live.rs"]
mod plugin_surface_live;
