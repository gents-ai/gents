#[path = "e2e_runtime/client_submission_contention.rs"]
mod client_submission_contention;
#[path = "e2e_runtime/completion_retry_tape.rs"]
mod completion_retry_tape;
#[path = "e2e_runtime/document_config_bootstrap.rs"]
mod document_config_bootstrap;
#[path = "e2e_runtime/effective_model_binding.rs"]
mod effective_model_binding;
#[path = "e2e_runtime/fork_invariants.rs"]
mod fork_invariants;
#[path = "e2e_runtime/provider_fixture_redaction.rs"]
mod provider_fixture_redaction;
#[path = "e2e_runtime/rendered_request_capture.rs"]
mod rendered_request_capture;
#[path = "e2e_runtime/runtime_observability.rs"]
mod runtime_observability;
#[path = "e2e_runtime/schedule_snapshot_reconcile.rs"]
mod schedule_snapshot_reconcile;
#[path = "e2e_runtime/self_config_tools.rs"]
mod self_config_tools;
#[path = "e2e_runtime/storage_budget.rs"]
mod storage_budget;

#[test]
fn provider_wire_fixtures_do_not_contain_credentials() {
    provider_fixture_redaction::provider_wire_fixtures_do_not_contain_credentials()
        .expect("provider wire fixtures should be redacted");
}

#[path = "e2e_runtime/queue_management.rs"]
mod queue_management;
#[path = "e2e_runtime/steering_delivery.rs"]
mod steering_delivery;

#[path = "e2e_runtime/session_input_edit.rs"]
mod session_input_edit;

#[path = "e2e_runtime/enrollment_renewal.rs"]
mod enrollment_renewal;
