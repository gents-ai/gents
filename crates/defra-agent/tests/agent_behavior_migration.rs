//! Upgrade-path integration test for the AgentBehavior description+summary
//! migration (ensure_agent_behavior_migrations).
//!
//! Scenario: a DB that was created on a pre-#377 schema version does not have
//! the `description` or `summary` fields on `AgentBehavior`. When the server
//! starts up and calls `from_default_behavior_documents` (which issues a
//! GraphQL query selecting those fields), it must NOT crash with an "unknown
//! field" error. The fix is that `ensure_agent_behavior_migrations` runs
//! BEFORE the first behavior read on the serve path.
//!
//! This test reproduces the upgrade path by:
//!   1. Adding the OLD AgentBehavior schema (without description/summary).
//!   2. Adding all other runtime schemas (AgentBehavior already exists, so the
//!      `add_schema` call for it is a no-op — exactly what happens on upgrade).
//!   3. Creating an AgentBehavior row with only old-schema fields.
//!   4. Running ensure_agent_behavior_migrations.
//!   5. Querying AgentBehavior selecting description + summary — must succeed.
//!
//! Without the serve.rs fix this test still passes (the migration itself is
//! correct), but it validates that the migration+read ordering works end-to-end
//! and that the migration is genuinely needed for old-schema DBs.

mod support;

use std::sync::Arc;

use defra_agent::defra_node::EmbeddedNode;
use defra_agent::ensure_runtime_schemas;
use tempfile::TempDir;

/// Old-schema AgentBehavior SDL — identical to the current schema except that
/// `description` and `summary` are absent. This represents the schema state of
/// any DB created before branch #377 landed.
const AGENT_BEHAVIOR_OLD_SDL: &str = r#"type AgentBehavior {
    behavior_id: String @index(unique: true)
    agent_did: String @index
    display_name: String
    system_prompt: String
    backend_id: String @index
    model_name: String
    tool_selection_id: String @index
    inference_profile_id: String @index
    compaction_strategy: String
    compaction_threshold: Float
    enabled: Boolean @index
    created_at: String
}"#;

struct OldSchemaDb {
    node: Arc<EmbeddedNode>,
    _tempdir: TempDir,
}

/// Boot a node with the OLD AgentBehavior schema (no description/summary),
/// then install all other runtime schemas. This mirrors what happens when
/// a pre-#377 defra-agent install boots a current binary: the existing
/// AgentBehavior collection is already present (from the old SDL), so
/// `ensure_runtime_schemas` silently skips it ("already exists"), leaving the
/// collection without the new fields.
async fn old_schema_db(name: &str) -> OldSchemaDb {
    let tempdir = tempfile::Builder::new()
        .prefix(&format!("defra-agent-behavior-migration-{name}-"))
        .tempdir()
        .expect("tempdir");
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(tempdir.path())
            .build()
            .await
            .expect("embedded node"),
    );

    // Step 1: Register the OLD schema first — this locks AgentBehavior into
    // the pre-#377 shape (no description, no summary).
    node.add_schema(AGENT_BEHAVIOR_OLD_SDL)
        .await
        .expect("add old AgentBehavior schema");

    // Step 2: Install all other runtime schemas. AgentBehavior already exists
    // so its add_schema is a no-op (the helper swallows "already exists").
    ensure_runtime_schemas(&node)
        .await
        .expect("ensure runtime schemas");

    OldSchemaDb {
        node,
        _tempdir: tempdir,
    }
}

/// Verify that after running ensure_agent_behavior_migrations on an
/// old-schema DB, a GraphQL query that selects description + summary
/// succeeds rather than failing with "unknown field".
#[tokio::test]
async fn migration_adds_description_and_summary_to_old_schema_db() {
    let db = old_schema_db("description_summary_add").await;
    let node = db.node.clone();

    // Confirm the collection exists without description/summary before migration.
    let collection_before = node
        .get_collection("AgentBehavior")
        .expect("get AgentBehavior before migration");
    let collection_before = collection_before.expect("AgentBehavior collection must exist");
    assert!(
        !collection_before
            .fields
            .iter()
            .any(|f| f.name == "description"),
        "pre-migration collection must NOT have description field"
    );
    assert!(
        !collection_before.fields.iter().any(|f| f.name == "summary"),
        "pre-migration collection must NOT have summary field"
    );

    // Create an old-schema AgentBehavior row (only fields present before #377).
    let mutation = r#"mutation {
        create_AgentBehavior(input: {
            behavior_id: "test:default",
            agent_did: "did:key:test",
            display_name: "Test",
            system_prompt: "",
            backend_id: "b1",
            model_name: "test-model",
            tool_selection_id: "",
            inference_profile_id: "",
            compaction_strategy: "StripThenSummarize",
            compaction_threshold: 0.75,
            enabled: true,
            created_at: "2025-01-01T00:00:00Z"
        }) { _docID }
    }"#;
    let resp = node.execute(mutation).await;
    assert!(
        !resp.has_errors(),
        "create AgentBehavior on old schema failed: {:?}",
        resp.errors
    );

    // Run the migration.
    defra_agent::migration::ensure_agent_behavior_migrations(node.clone())
        .await
        .expect("ensure_agent_behavior_migrations must succeed on old-schema DB");

    // Verify the collection now has description and summary.
    let collection_after = node
        .get_collection("AgentBehavior")
        .expect("get AgentBehavior after migration")
        .expect("AgentBehavior collection must still exist");
    assert!(
        collection_after
            .fields
            .iter()
            .any(|f| f.name == "description"),
        "post-migration collection must have description field"
    );
    assert!(
        collection_after.fields.iter().any(|f| f.name == "summary"),
        "post-migration collection must have summary field"
    );

    // Crucially: query selecting description + summary must succeed.
    // This is the exact query issued by list_agent_behavior_records (and
    // transitively by from_default_behavior_documents on the serve path).
    let query = r#"{
        AgentBehavior(filter: { agent_did: { _eq: "did:key:test" } }) {
            behavior_id
            agent_did
            display_name
            description
            summary
            system_prompt
            backend_id
            model_name
            tool_selection_id
            inference_profile_id
            compaction_strategy
            compaction_threshold
            enabled
            created_at
        }
    }"#;
    let resp = node.execute(query).await;
    assert!(
        !resp.has_errors(),
        "query AgentBehavior with description+summary after migration must NOT error: {:?}",
        resp.errors
    );

    // The existing row should have been found; description and summary are
    // null (no row transform needed for additive-nullable fields).
    let rows = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentBehavior"))
        .and_then(|v| v.as_array())
        .expect("AgentBehavior result must be an array");
    assert_eq!(rows.len(), 1, "expected exactly one row after migration");
    let row = &rows[0];
    assert_eq!(
        row.get("behavior_id").and_then(|v| v.as_str()),
        Some("test:default"),
        "behavior_id must match"
    );
    // description and summary are absent in old rows — they should come back
    // as null rather than causing an error.
    assert!(
        row.get("description").map(|v| v.is_null()).unwrap_or(true),
        "description must be null for pre-migration row"
    );
    assert!(
        row.get("summary").map(|v| v.is_null()).unwrap_or(true),
        "summary must be null for pre-migration row"
    );
}

/// Verify idempotency: running the migration twice on the same DB (once after
/// the schema is already patched) is a no-op and does not error.
#[tokio::test]
async fn migration_is_idempotent_on_already_patched_db() {
    let db = old_schema_db("idempotency").await;
    let node = db.node.clone();

    defra_agent::migration::ensure_agent_behavior_migrations(node.clone())
        .await
        .expect("first migration run");

    // Second run must also succeed without error.
    defra_agent::migration::ensure_agent_behavior_migrations(node.clone())
        .await
        .expect("second migration run (idempotency check)");
}

/// Verify that the migration is a no-op on a fresh (current-schema) DB where
/// AgentBehavior already has description and summary from the SDL.
#[tokio::test]
async fn migration_is_noop_on_fresh_current_schema_db() {
    // Use the standard test_db helper which loads the current SDL (with description+summary).
    let db = support::test_db("behavior_migration_fresh").await;

    defra_agent::migration::ensure_agent_behavior_migrations(db.node.clone())
        .await
        .expect("migration on fresh DB must succeed");

    // A subsequent query must still work.
    let query = r#"{ AgentBehavior { behavior_id description summary } }"#;
    let resp = db.node.execute(query).await;
    assert!(
        !resp.has_errors(),
        "query on fresh DB after migration must succeed: {:?}",
        resp.errors
    );
}
