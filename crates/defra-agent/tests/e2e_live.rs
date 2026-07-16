//! Live suites against a real inference endpoint (all ignored by default).
//!
//! One binary per family: each module was a standalone test binary; the
//! consolidation cuts link time without changing any test.

mod support;

#[path = "e2e_live/backend_auth_live.rs"]
mod backend_auth_live;
#[path = "e2e_live/edit_file_live.rs"]
mod edit_file_live;
#[path = "e2e_live/interrupt_live.rs"]
mod interrupt_live;
#[path = "e2e_live/p2p_admission_concurrent_live.rs"]
mod p2p_admission_concurrent_live;
#[path = "e2e_live/post_status_json_live.rs"]
mod post_status_json_live;
#[path = "e2e_live/steward_loop_live.rs"]
mod steward_loop_live;
#[path = "e2e_live/subagent_delegation_live.rs"]
mod subagent_delegation_live;
#[path = "e2e_live/workflow_orchestration_live.rs"]
mod workflow_orchestration_live;
