//! Trigger/schedule engine end-to-end suites.
//!
//! One binary per family: each module was a standalone test binary; the
//! consolidation cuts link time without changing any test.

mod support;

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
