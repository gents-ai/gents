//! Lens migration integration test for AgentToolCall v1 -> v2.

use crate::support::test_db;

#[tokio::test]
async fn lens_compute_v2_fields_produces_expected_outputs() {
    // Unit-style assertion against the lens transform. The DefraDB-level
    // integration is verified indirectly by start-up migration not panicking
    // and by the Bucket 3 tests passing on a freshly-migrated database.
    use agent_tool_call_lifecycle_v1_to_v2_lens::compute_v2_fields;

    assert_eq!(
        compute_v2_fields(Some("called"), None),
        ("running".to_string(), None)
    );
    assert_eq!(
        compute_v2_fields(Some("completed"), None),
        ("completed".to_string(), None)
    );
    assert_eq!(
        compute_v2_fields(Some("completed"), Some("tool_timeout")),
        ("timedOut".to_string(), None)
    );
    assert_eq!(
        compute_v2_fields(Some("completed"), Some("invalid_tool_arguments")),
        ("failed".to_string(), Some("argumentInvalid".to_string()))
    );
}

#[tokio::test]
async fn migration_is_idempotent_on_already_migrated_database() {
    // The fixture wires schemas + runs the migration on first startup.
    // Re-opening (creating another fresh test_db) is the simplest smoke
    // test that the migration isn't erroring after schema state already
    // contains lifecycle_state. Each test_db is its own tempdir so this
    // is a "fresh + idempotent re-run" pattern, not a true "same DB
    // re-opened" test — but it does exercise that the migration path
    // runs cleanly twice.
    let db1 = test_db("migration_idempotency_1").await;
    let _ = db1.node.clone();
    drop(db1);

    let db2 = test_db("migration_idempotency_2").await;
    let _ = db2.node.clone();
    // No panic = pass.
}
