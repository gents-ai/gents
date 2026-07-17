#[path = "commands/chat.rs"]
mod chat;
#[path = "commands/config.rs"]
mod config;
#[path = "commands/mcp_health.rs"]
pub(crate) mod mcp_health;
#[path = "commands/peer.rs"]
mod peer;
#[path = "commands/task.rs"]
mod task;
#[path = "commands/tool_service.rs"]
mod tool_service;
#[path = "commands/util.rs"]
mod util;

pub(crate) use chat::{rename_conversation, send_chat_message};
#[cfg_attr(test, allow(unused_imports))]
pub(crate) use config::{
    delete_skill_config, save_agent_config, save_backend_config, save_behavior_config,
    save_inference_profile_config, save_skill_config, save_tool_selection_config,
};
pub(crate) use peer::{add_peer, remove_peer, rename_peer, repair_p2p};
pub(crate) use task::{
    run_schedule_config, run_task_config, save_event_trigger_config, save_schedule_config,
    save_task_config,
};
pub(crate) use tool_service::{save_tool_service_config, test_tool_service_config};
