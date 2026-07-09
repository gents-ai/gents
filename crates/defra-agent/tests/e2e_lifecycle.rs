//! Request lifecycle + interrupt end-to-end suites.
//!
//! One binary per family: each module was a standalone test binary; the
//! consolidation cuts link time without changing any test.

mod support;

#[path = "e2e_lifecycle/interrupt_observer.rs"]
mod interrupt_observer;
#[path = "e2e_lifecycle/interruption_integration.rs"]
mod interruption_integration;
#[path = "e2e_lifecycle/lifecycle_claim.rs"]
mod lifecycle_claim;
#[path = "e2e_lifecycle/lifecycle_queue.rs"]
mod lifecycle_queue;
#[path = "e2e_lifecycle/lifecycle_recovery.rs"]
mod lifecycle_recovery;
#[path = "e2e_lifecycle/lifecycle_terminal.rs"]
mod lifecycle_terminal;
#[path = "e2e_lifecycle/p2p_admission_backpressure_e2e.rs"]
mod p2p_admission_backpressure_e2e;
#[path = "e2e_lifecycle/replicated_request_convergence_p2p_e2e.rs"]
mod replicated_request_convergence_p2p_e2e;
