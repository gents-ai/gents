mod support;

#[path = "e2e_lifecycle/interrupt_observer.rs"]
mod interrupt_observer;
#[path = "e2e_lifecycle/interruption_integration.rs"]
mod interruption_integration;
#[path = "e2e_lifecycle/lifecycle_claim.rs"]
mod lifecycle_claim;
#[path = "e2e_lifecycle/lifecycle_recovery.rs"]
mod lifecycle_recovery;
#[path = "e2e_lifecycle/lifecycle_terminal.rs"]
mod lifecycle_terminal;
#[path = "e2e_lifecycle/merge_parked_unique_boot_e2e.rs"]
mod merge_parked_unique_boot_e2e;
#[path = "e2e_lifecycle/p2p_admission_backpressure_e2e.rs"]
mod p2p_admission_backpressure_e2e;
#[path = "e2e_lifecycle/request_runtime_locality_p2p_e2e.rs"]
mod request_runtime_locality_p2p_e2e;
