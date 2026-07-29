#[path = "commands/chat.rs"]
mod chat;
#[path = "commands/config.rs"]
mod config;
#[path = "commands/mcp_health.rs"]
pub mod mcp_health;
#[path = "commands/peer.rs"]
mod peer;
#[path = "commands/task.rs"]
mod task;
#[path = "commands/tool_service.rs"]
mod tool_service;
#[path = "commands/util.rs"]
mod util;

pub use chat::{rename_conversation, send_chat_message};
#[cfg_attr(test, allow(unused_imports))]
pub use config::{
    delete_backend_config, delete_behavior_config, delete_event_trigger_config,
    delete_inference_profile_config, delete_schedule_config, delete_skill_config,
    delete_task_config, delete_tool_selection_config, delete_tool_service_config,
    save_agent_config, save_backend_config, save_behavior_config, save_inference_profile_config,
    save_skill_config, save_tool_selection_config,
};
pub use peer::{add_peer, pair_bearer, remove_peer, rename_peer, repair_p2p};
pub use task::{
    run_schedule_config, run_task_config, save_event_trigger_config, save_schedule_config,
    save_task_config,
};
pub use tool_service::{save_tool_service_config, test_tool_service_config};
