mod support;

/// The embedded P2P transport is process-global enough that independent
/// scenarios contend when libtest runs them concurrently. Event-source
/// reconciliation also boots that transport even in its single-node test, so
/// it must share this guard with the multi-node harnesses. Acquire the guard
/// before starting a node so per-test readiness deadlines measure runtime work
/// rather than time blocked behind another process-global transport.
static P2P_E2E_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[path = "e2e_triggers/app_collection_pairing_p2p_e2e.rs"]
mod app_collection_pairing_p2p_e2e;
#[path = "e2e_triggers/event_source_trigger_e2e.rs"]
mod event_source_trigger_e2e;
#[path = "e2e_triggers/event_source_trigger_p2p_e2e.rs"]
mod event_source_trigger_p2p_e2e;
#[path = "e2e_triggers/write_tool_trigger_e2e.rs"]
mod write_tool_trigger_e2e;
