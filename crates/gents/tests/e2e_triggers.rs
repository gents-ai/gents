mod support;

/// The embedded P2P transport is process-global enough that independent
/// multi-node scenarios contend when libtest runs them concurrently. Keep the
/// scenarios parallel with non-P2P trigger tests while serializing only the
/// P2P harnesses that share this integration-test process.
static P2P_E2E_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[path = "e2e_triggers/app_collection_pairing_p2p_e2e.rs"]
mod app_collection_pairing_p2p_e2e;
#[path = "e2e_triggers/event_trigger_e2e.rs"]
mod event_trigger_e2e;
#[path = "e2e_triggers/event_trigger_p2p_e2e.rs"]
mod event_trigger_p2p_e2e;
#[path = "e2e_triggers/trigger_engine_e2e.rs"]
mod trigger_engine_e2e;
#[path = "e2e_triggers/write_tool_trigger_e2e.rs"]
mod write_tool_trigger_e2e;
