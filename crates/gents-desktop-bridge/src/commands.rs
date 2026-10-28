#[path = "commands/background.rs"]
mod background;
pub use background::background_tool;

pub(crate) async fn process_stop(
    core: &gents_desktop_core::client::ClientCore,
    request: &crate::types::DesktopProcessStopRequest,
) -> anyhow::Result<()> {
    let receipt = core
        .stop_background_process(
            &request.node_did,
            &request.session_id,
            &request.tool_call_id,
            &request.tool_call_doc_id,
        )
        .await?;
    match receipt.outcome {
        gents_protocol::process_control::ProcessStopOutcome::Stopped
        | gents_protocol::process_control::ProcessStopOutcome::AlreadyTerminal => Ok(()),
        _ => anyhow::bail!("process stop failed: {}", receipt.reason),
    }
}

#[path = "commands/chat.rs"]
mod chat;
#[path = "commands/chat_resolve.rs"]
mod chat_resolve;
#[path = "commands/chat_submission.rs"]
mod chat_submission;
pub use chat_submission::{acknowledge_chat_submission, recover_chat_submission};
#[path = "commands/goal.rs"]
mod goal;
pub use goal::{control_goal, session_goal};
#[path = "commands/goal_submission.rs"]
mod goal_submission;
pub use goal_submission::{
    acknowledge_goal_submission, recover_goal_submission, resume_goal_submission,
};
#[path = "commands/config.rs"]
mod config;
#[path = "commands/mailbox.rs"]
mod mailbox;
#[path = "commands/mcp_health.rs"]
pub mod mcp_health;
#[path = "commands/peer.rs"]
mod peer;
#[path = "commands/session.rs"]
mod session;
#[path = "commands/session_history.rs"]
mod session_history;
pub use session_history::session_history;
#[path = "commands/task.rs"]
mod task;
#[path = "commands/tool_service.rs"]
mod tool_service;
#[path = "commands/util.rs"]
mod util;

pub use chat::{
    edit_pending_queue, fork_session_before_message, inventory_file_change, rename_session,
    request_output, retry_request, send_chat_message, session_file_inventory, set_session_archived,
    steer_pending_message, tool_file_changes,
};
#[cfg_attr(test, allow(unused_imports))]
pub use config::{
    apply_config_components, delete_agent_config, delete_backend_config, delete_context_config,
    delete_inference_profile_config, delete_schedule_config, delete_skill_config,
    delete_task_config, delete_tool_service_config, delete_tools_config, delete_trigger_config,
    patch_config_components, save_agent_config, save_backend_config, save_inference_profile_config,
    save_node_config, save_skill_config, save_tools_config, set_default_agent,
};
pub use mailbox::{dismiss_mailbox, list_mailbox, start_mailbox_request};
pub use peer::{remove_peer, rename_peer, repair_p2p, request_status_enrollment};
pub use task::{
    delete_event_source_config, run_schedule_config, run_task_config, save_event_source_config,
    save_schedule_config, save_task_config, save_trigger_config,
};
pub use tool_service::{save_tool_service_config, test_tool_service_config};

pub use session::{message_attachments, session_context_details, session_rows, session_snapshot};
