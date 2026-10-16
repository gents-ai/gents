mod support;

/// The embedded P2P transport is process-global: scenarios contend when libtest
/// runs them concurrently, and event-source reconciliation boots the transport
/// even single-node. Acquire before starting a node so readiness deadlines
/// measure runtime work, not time blocked behind another transport user.
static P2P_E2E_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[path = "e2e_triggers/app_collection_pairing_p2p_e2e.rs"]
mod app_collection_pairing_p2p_e2e;
#[path = "e2e_triggers/event_source_trigger_e2e.rs"]
mod event_source_trigger_e2e;
#[path = "e2e_triggers/event_source_trigger_p2p_e2e.rs"]
mod event_source_trigger_p2p_e2e;
#[path = "e2e_triggers/workspace_change_unit_cycle.rs"]
mod workspace_change_unit_cycle;
#[path = "e2e_triggers/write_tool_trigger_e2e.rs"]
mod write_tool_trigger_e2e;
