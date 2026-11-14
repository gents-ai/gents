#[path = "background_panic.rs"]
mod background_panic;
#[path = "background_policy.rs"]
mod background_policy;
#[path = "change_capture.rs"]
mod change_capture;
#[path = "materialize_provenance.rs"]
mod materialize_provenance;
#[path = "message_agent_tools.rs"]
mod message_agent_tools;
#[path = "message_hop.rs"]
mod message_hop;
#[path = "message_inspection.rs"]
mod message_inspection;
#[path = "pack_export.rs"]
mod pack_export;
#[path = "query_result.rs"]
mod query_result;
#[path = "queue.rs"]
mod queue;
#[path = "queue_coalescing.rs"]
mod queue_coalescing;
#[path = "r4c_read_tool_output.rs"]
mod r4c_read_tool_output;
#[path = "script_composition.rs"]
mod script_composition;
#[path = "session_history_discovery.rs"]
mod session_history_discovery;
#[path = "sessions.rs"]
mod sessions;
#[path = "stream_processor.rs"]
mod stream_processor;
#[path = "wait_settlement.rs"]
mod wait_settlement;
#[cfg(target_os = "macos")]
#[path = "workspace_rejection.rs"]
mod workspace_rejection;
