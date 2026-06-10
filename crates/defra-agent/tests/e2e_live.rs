//! Live suites against a real inference endpoint (all ignored by default).
//!
//! One binary per family: each module was a standalone test binary; the
//! consolidation cuts link time without changing any test.

mod support;

#[path = "e2e_live/backend_auth_live.rs"]
mod backend_auth_live;
#[path = "e2e_live/interrupt_live.rs"]
mod interrupt_live;
#[path = "e2e_live/steward_loop_live.rs"]
mod steward_loop_live;
#[path = "e2e_live/subagent_delegation_live.rs"]
mod subagent_delegation_live;
