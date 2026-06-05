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
//!
//! Additional tests added for PR #377:
//!
//! - `from_default_behavior_documents_succeeds_on_old_schema_db`: verifies that
//!   `DefraAgent::from_default_behavior_documents` no longer crashes on an
//!   old-schema DB (H2 fix — migration now runs inside the constructor before
//!   any behavior read).
//!
//! - `config_read_path_succeeds_on_old_schema_db`: verifies that the GraphQL
//!   query issued by the offline config diff/apply/export path
//!   (build_desired_state_live_bundle selecting EXPORT_AGENT_BEHAVIOR_FIELDS
//!   including description + summary) succeeds on an old-schema DB after the
//!   migration runs.
//!
//! - `live_fixture_subagent_target_entry_parses`: verifies that the JSON entry
//!   written by the desktop live fixture (H1 fix) round-trips through
//!   SubagentTarget::parse and yields the expected fields, confirming that bare
//!   strings are no longer stored and the entry would pass the runtime's parse
//!   filter.

mod support;

use std::sync::Arc;

use defra_agent::defra_node::EmbeddedNode;
use defra_agent::ensure_runtime_schemas;
use tempfile::TempDir;

use defra_agent::DefraAgent;

use support::fixtures::test_identity;

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

// ─── H2(a): from_default_behavior_documents on an old-schema DB ─────────────

/// Verify that `DefraAgent::from_default_behavior_documents` does NOT crash
/// when called against a DB that was created before branch #377 (i.e. without
/// `description`/`summary` on AgentBehavior).
///
/// The fix (H2): `from_default_behavior_documents` now runs
/// `ensure_agent_behavior_migrations` before any behavior read, so the schema
/// is patched in-place and the subsequent GraphQL query that selects
/// `description` + `summary` succeeds.
///
/// We seed the AgentPrincipal, AgentBehavior, and InferenceBackend using raw
/// GraphQL mutations rather than the Rust helpers that also SELECT the new
/// fields (which would fail on the old schema before any migration).  That keeps
/// the DB in a genuine pre-#377 state until `from_default_behavior_documents`
/// is invoked; the test passes only if the constructor itself runs the migration
/// before the first SELECT.
#[tokio::test]
async fn from_default_behavior_documents_succeeds_on_old_schema_db() {
    let identity: Arc<dyn defra_agent::AgentIdentity> =
        Arc::new(test_identity("from-default-behavior-pre377"));
    let agent_did = identity.did().to_string();

    let db = old_schema_db("from_default_behavior_pre377").await;
    let node = db.node.clone();

    // Seed an InferenceBackend (no old/new-field issue here).
    let backend_mutation = r#"mutation {
        create_InferenceBackend(input: {
            backend_id: "backend-pre377",
            name: "backend-pre377",
            provider_kind: "OpenAiCompatible",
            endpoint: "http://localhost:9999/v1",
            max_concurrent: 1,
            max_queue_depth: 100,
            enabled: true,
            models: ["test-model"],
            probe_status: "healthy"
        }) { _docID }
    }"#;
    let resp = node.execute(backend_mutation).await;
    assert!(
        !resp.has_errors(),
        "create InferenceBackend failed: {:?}",
        resp.errors
    );

    // Seed an AgentBehavior using the OLD schema fields only (no description/summary).
    let default_behavior_id = defra_agent::default_behavior_id_for_agent(&agent_did);
    let escaped_did = defra_agent::graphql::escape_graphql_string(&agent_did);
    let escaped_bh_id = defra_agent::graphql::escape_graphql_string(&default_behavior_id);
    let behavior_mutation = format!(
        r#"mutation {{
            create_AgentBehavior(input: {{
                behavior_id: "{escaped_bh_id}",
                agent_did: "{escaped_did}",
                display_name: "Pre-377 behavior",
                system_prompt: "",
                backend_id: "backend-pre377",
                model_name: "test-model",
                tool_selection_id: "",
                inference_profile_id: "",
                compaction_strategy: "StripThenSummarize",
                compaction_threshold: 0.75,
                enabled: true,
                created_at: "2025-01-01T00:00:00Z"
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&behavior_mutation).await;
    assert!(
        !resp.has_errors(),
        "create AgentBehavior (old schema) failed: {:?}",
        resp.errors
    );

    // Seed an AgentPrincipal referencing the behavior above.
    let principal_mutation = format!(
        r#"mutation {{
            create_AgentPrincipal(input: {{
                agent_did: "{escaped_did}",
                display_name: "Pre-377 principal",
                default_behavior_id: "{escaped_bh_id}",
                enabled: true
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&principal_mutation).await;
    assert!(
        !resp.has_errors(),
        "create AgentPrincipal failed: {:?}",
        resp.errors
    );

    // Confirm the collection is still in the pre-#377 state.
    let collection = node
        .get_collection("AgentBehavior")
        .expect("get_collection must succeed")
        .expect("AgentBehavior must exist");
    assert!(
        !collection.fields.iter().any(|f| f.name == "description"),
        "pre-test: AgentBehavior must NOT have description yet"
    );

    // This call must NOT return an "unknown field" error for description/summary.
    // The constructor now runs ensure_agent_behavior_migrations internally.
    DefraAgent::from_default_behavior_documents(
        node.clone(),
        identity,
        defra_agent::DocumentRuntimeOptions {
            tool_ceiling: defra_agent::ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .expect(
        "from_default_behavior_documents must succeed on old-schema DB after H2 fix; \
         an 'unknown field' error means the migration did not run before the read",
    );
}

// ─── H2(b): offline config read path on an old-schema DB ────────────────────

/// Verify that the GraphQL query issued by the offline config diff/apply/export
/// path (selecting `description` and `summary`) succeeds on an old-schema DB
/// after the migration has been applied.
///
/// The offline path (`resolve_config_access` → `build_desired_state_live_bundle`)
/// now calls `ensure_agent_behavior_migrations` before returning the node to
/// callers, so the collection is patched and field-selecting queries no longer
/// fail with "unknown field".
#[tokio::test]
async fn config_read_path_succeeds_on_old_schema_db() {
    let db = old_schema_db("config_read_pre377").await;
    let node = db.node.clone();

    // Seed a minimal AgentBehavior row (old-schema only).
    let mutation = r#"mutation {
        create_AgentBehavior(input: {
            behavior_id: "config-read-test:default",
            agent_did: "did:key:config-read-test",
            display_name: "Config Read Test",
            system_prompt: "",
            backend_id: "b-config-read",
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
        "seed AgentBehavior failed: {:?}",
        resp.errors
    );

    // Run the migration (mirrors what resolve_config_access now does).
    defra_agent::migration::ensure_agent_behavior_migrations(node.clone())
        .await
        .expect("migration must succeed on old-schema DB");

    // Issue exactly the query that EXPORT_AGENT_BEHAVIOR_FIELDS drives:
    // behavior_id agent_did display_name description summary system_prompt ...
    let query = r#"{
        AgentBehavior(filter: { agent_did: { _eq: "did:key:config-read-test" } }) {
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
        "offline config read selecting description+summary must NOT error after migration: {:?}",
        resp.errors
    );

    let rows = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentBehavior"))
        .and_then(|v| v.as_array())
        .expect("AgentBehavior must be an array");
    assert_eq!(
        rows.len(),
        1,
        "expected exactly one AgentBehavior row, got {}",
        rows.len()
    );
    // Pre-#377 rows have null description/summary — not an error.
    let row = &rows[0];
    assert!(
        row.get("description").map(|v| v.is_null()).unwrap_or(true),
        "description must be null for pre-migration row"
    );
    assert!(
        row.get("summary").map(|v| v.is_null()).unwrap_or(true),
        "summary must be null for pre-migration row"
    );
}

// ─── H1: desktop live fixture subagent target is a valid JSON entry ──────────

/// Verify that the `subagent_targets` entry produced by the desktop live fixture
/// (after the H1 fix) is valid JSON and round-trips through `SubagentTarget::parse`.
///
/// Before the fix, the fixture stored the bare `behavior_id` string; the runtime
/// silently dropped non-JSON entries and `tools_enabled()` returned false. After
/// the fix, the entry is a properly serialized `SubagentTarget` JSON object.
#[test]
fn live_fixture_subagent_target_entry_parses() {
    let agent_did = "did:key:z6MkTestFixture";
    let behavior_id = format!("{agent_did}:live-repo-audit-subagent");

    // Re-create the entry exactly as the fixed fixture does.
    let entry = defra_agent::subagent_target_entry(
        "repo-audit-subagent",
        agent_did,
        &behavior_id,
        Some("Local repository audit subagent for the desktop live fixture".to_string()),
    );

    // A bare string like the old fixture would produce fails to parse.
    assert!(
        defra_agent::SubagentTarget::parse(&behavior_id).is_err(),
        "bare behavior_id string must NOT parse as SubagentTarget (regression guard)"
    );

    // The fixed entry must parse successfully.
    let parsed = defra_agent::SubagentTarget::parse(&entry)
        .expect("fixed fixture entry must parse as a valid SubagentTarget JSON object");

    assert_eq!(parsed.name, "repo-audit-subagent");
    assert_eq!(parsed.agent_did, agent_did);
    assert_eq!(parsed.behavior_id, behavior_id);
    assert!(
        parsed.description.is_some(),
        "description must be present in the fixture entry"
    );
    assert!(
        parsed.is_structurally_valid(),
        "parsed SubagentTarget must be structurally valid (all fields non-empty)"
    );
}
