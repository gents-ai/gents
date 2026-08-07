mod support;

#[path = "e2e_runtime/adapter_projection_external_fixtures.rs"]
mod adapter_projection_external_fixtures;
#[path = "e2e_runtime/completion_retry_tape.rs"]
mod completion_retry_tape;
#[path = "e2e_runtime/defradb_time_travel.rs"]
mod defradb_time_travel;
#[path = "e2e_runtime/defradb_v0612_store_upgrade.rs"]
mod defradb_v0612_store_upgrade;
#[path = "e2e_runtime/document_config_bootstrap.rs"]
mod document_config_bootstrap;
#[path = "e2e_runtime/event_source_subscription_factory_smoke.rs"]
mod event_source_subscription_factory_smoke;
#[path = "e2e_runtime/fork_invariants.rs"]
mod fork_invariants;
#[path = "e2e_runtime/peer_pairing_desired_query.rs"]
mod peer_pairing_desired_query;
#[path = "e2e_runtime/projection_acp_policy_lifecycle.rs"]
mod projection_acp_policy_lifecycle;
#[path = "e2e_runtime/provider_fixture_redaction.rs"]
mod provider_fixture_redaction;
#[path = "e2e_runtime/provider_fixture_replay.rs"]
mod provider_fixture_replay;
#[path = "e2e_runtime/rendered_request_capture.rs"]
mod rendered_request_capture;
#[path = "e2e_runtime/runtime_observability.rs"]
mod runtime_observability;
#[path = "e2e_runtime/schedule_snapshot_reconcile.rs"]
mod schedule_snapshot_reconcile;
#[path = "e2e_runtime/self_config_tools.rs"]
mod self_config_tools;

#[test]
fn provider_wire_fixtures_do_not_contain_credentials() {
    provider_fixture_redaction::provider_wire_fixtures_do_not_contain_credentials()
        .expect("provider wire fixtures should be redacted");
}

#[test]
fn provider_wire_fixture_replay_consumes_every_recorded_exchange_once() {
    provider_fixture_replay::provider_wire_fixture_replay_consumes_every_recorded_exchange_once()
        .expect("provider wire fixture replay should consume all exchanges");
}

#[test]
fn provider_wire_fixture_replay_rejects_unmatched_and_leftover_requests() {
    provider_fixture_replay::provider_wire_fixture_replay_rejects_unmatched_and_leftover_requests()
        .expect("provider wire fixture replay should reject unmatched and leftover requests");
}
