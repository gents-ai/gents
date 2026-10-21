use std::collections::BTreeSet;

use gents_migration::{
    ensure_migrations, ensure_migrations_dynamic, ensure_migrations_with_registry,
    BaselineCollectionOwned, CollectionExpectation, DynamicRegistry, Error, MigrationStep,
    Registry, CLIENT_AUTHORED_COLLECTIONS,
};

mod common;
use common::fresh_node;

#[test]
fn default_baseline_matches_ordered_protocol_catalog() {
    let actual = gents_migration::DEFAULT_BASELINE
        .iter()
        .map(|entry| (entry.name, entry.sdl))
        .collect::<Vec<_>>();
    let expected = gents_protocol::schemas::RUNTIME_COLLECTION_NAMES
        .iter()
        .copied()
        .zip(gents_protocol::schemas::RUNTIME_ALL.iter().copied())
        .chain(
            gents_protocol::schemas::ALL_COLLECTION_NAMES
                .iter()
                .copied()
                .zip(gents_protocol::schemas::ALL.iter().copied()),
        )
        .collect::<Vec<_>>();

    assert_eq!(actual, expected, "baseline must use the canonical live SDL");
    assert!(gents_migration::DEFAULT_STEPS.is_empty());
    let stepped_client_collections = gents_migration::DEFAULT_STEPS
        .iter()
        .filter_map(MigrationStep::collection)
        .filter(|collection| CLIENT_AUTHORED_COLLECTIONS.contains(collection))
        .collect::<BTreeSet<_>>();
    assert!(
        stepped_client_collections.is_empty(),
        "client-authored collections gained migration steps: {stepped_client_collections:?} — \
         do not freeze-and-chain these; fold the change into the live SDL and re-pin the \
         baseline to the fresh-apply CID (see CLIENT_AUTHORED_COLLECTIONS in registry.rs, \
         #1123/#1125)"
    );
}

#[test]
fn default_baseline_covers_every_protocol_collection_once() {
    let baseline_names = gents_migration::DEFAULT_BASELINE
        .iter()
        .map(|entry| entry.name)
        .collect::<BTreeSet<_>>();
    let protocol_names = gents_protocol::schemas::ALL_COLLECTION_NAMES
        .iter()
        .chain(gents_protocol::schemas::RUNTIME_COLLECTION_NAMES.iter())
        .copied()
        .collect::<BTreeSet<_>>();

    assert_eq!(
        gents_migration::DEFAULT_BASELINE.len(),
        baseline_names.len(),
        "migration baseline must not contain duplicate collections"
    );
    assert!(
        gents_migration::DEFAULT_BASELINE
            .iter()
            .all(|entry| entry.expected_version.is_some()),
        "production migration baseline must pin every root version"
    );
    assert_eq!(
        baseline_names, protocol_names,
        "migration baseline must cover the full protocol schema catalog"
    );
}

#[tokio::test]
async fn fresh_node_runtime_baseline_is_diagnostics_only() {
    let node = fresh_node().await;
    ensure_migrations(node.as_ref())
        .await
        .expect("ensure migrations");
    let runtime = node
        .get_collection(gents_protocol::schemas::NODE_RUNTIME_NAME)
        .expect("get NodeRuntime")
        .expect("NodeRuntime installed");
    assert_eq!(
        runtime.version_id,
        "bafyreif7je44mc6mdxtpp6wpakbdhuzsjibgtifxlo6pzgfhf2sqlcdrgq"
    );
    let field_names = runtime
        .fields
        .iter()
        .map(|field| field.name.as_str())
        .collect::<BTreeSet<_>>();
    assert!(!field_names.contains("runnable_behavior_count"));
    assert!(!field_names.contains("unavailable_behavior_count"));
    assert!(!field_names.contains("process_state"));
    assert!(!field_names.contains("active_generation"));
    assert!(!field_names.contains("router_generation"));
    assert!(!field_names.contains("default_agent_id"));
    assert!(field_names.contains("reconcile_phase"));
    assert!(field_names.contains("updated_at"));
    node.shutdown().await;
}

#[tokio::test]
async fn existing_agent_session_schema_upgrades_in_place_and_preserves_rows() {
    let node = fresh_node().await;
    let legacy_sdl = gents_protocol::schemas::AGENT_SESSION
        .lines()
        .filter(|line| !line.trim_start().starts_with("stopped_at:"))
        .collect::<Vec<_>>()
        .join("\n");
    node.add_schema(&legacy_sdl)
        .await
        .expect("register pre-stop AgentSession schema");
    let legacy = node
        .get_collection(gents_protocol::schemas::AGENT_SESSION_NAME)
        .expect("read legacy AgentSession schema")
        .expect("legacy AgentSession exists");
    assert_eq!(
        legacy.version_id,
        "bafyreietunsrqsijc3k6x3gf6y2nwkim74uf6j4lskv7p3pjrl44q4tthe"
    );

    let created = node
        .execute(
            r#"mutation {
        add_AgentSession(input: {
            session_id: "existing-session"
            node_did: "node-before-stop"
            agent_id: "agent-before-stop"
            created_at: "2026-10-21T00:00:00Z"
            title: "retained title"
        }) {_docID}
    }"#,
        )
        .await;
    assert!(!created.has_errors(), "seed existing session: {created:?}");
    let old_doc_id = created.data.unwrap()["add_AgentSession"][0]["_docID"]
        .as_str()
        .expect("created session document ID")
        .to_owned();
    let old_field_ids = legacy
        .fields
        .iter()
        .map(|field| (field.name.clone(), field.id.clone()))
        .collect::<std::collections::HashMap<_, _>>();

    let first = ensure_migrations(node.as_ref())
        .await
        .expect("append stopped_at to existing AgentSession schema");
    assert!(first.steps_applied > 0);
    let migrated = node
        .get_collection(gents_protocol::schemas::AGENT_SESSION_NAME)
        .expect("read upgraded AgentSession schema")
        .expect("upgraded AgentSession exists");
    assert_ne!(migrated.version_id, legacy.version_id);
    for (name, id) in old_field_ids {
        let current = migrated
            .fields
            .iter()
            .find(|field| field.name == name)
            .unwrap_or_else(|| panic!("existing field disappeared: {name}"));
        assert_eq!(current.id, id, "AgentSession.{name} field ID changed");
    }
    assert!(migrated
        .indexes
        .iter()
        .any(|index| index.name == "AgentSession_stopped_at_ASC"));

    let read = node
        .execute(
            r#"{ AgentSession(filter: {session_id: {_eq: "existing-session"}}) {
                _docID session_id node_did agent_id created_at title stopped_at
            } }"#,
        )
        .await;
    assert!(
        !read.has_errors(),
        "read upgraded session: {:?}",
        read.errors
    );
    let row = read.data.as_ref().unwrap()["AgentSession"]
        .as_array()
        .and_then(|rows| rows.first())
        .expect("preserved session row");
    assert_eq!(row["session_id"], "existing-session");
    assert_eq!(row["_docID"], old_doc_id);
    assert_eq!(row["node_did"], "node-before-stop");
    assert_eq!(row["agent_id"], "agent-before-stop");
    assert_eq!(row["title"], "retained title");
    assert!(row["stopped_at"].is_null());

    let second = ensure_migrations(node.as_ref())
        .await
        .expect("recheck upgraded AgentSession schema");
    assert_eq!(second.steps_applied, 0);
    let repeated = node
        .get_collection(gents_protocol::schemas::AGENT_SESSION_NAME)
        .expect("read repeated AgentSession schema")
        .expect("repeated AgentSession exists");
    assert_eq!(repeated.version_id, migrated.version_id);
    node.shutdown().await;
}

#[tokio::test]
async fn ensure_migrations_registers_baseline_and_is_idempotent() {
    let node = fresh_node().await;
    let report1 = ensure_migrations(node.as_ref())
        .await
        .expect("first ensure");
    assert!(
        report1.baseline_registered + report1.baseline_already_present
            >= gents_migration::DEFAULT_BASELINE.len(),
        "expected full baseline coverage, got {report1:?}"
    );
    assert_eq!(report1.steps_applied, gents_migration::DEFAULT_STEPS.len());
    if gents_migration::DEFAULT_STEPS.is_empty() {
        assert_eq!(
            report1.materialization.collections_attempted, 0,
            "a chain-free baseline has no old document versions to materialize"
        );
    }

    let report2 = ensure_migrations(node.as_ref())
        .await
        .expect("second ensure");
    assert_eq!(report2.steps_applied, 0);
    assert_eq!(
        report2.steps_already_current,
        gents_migration::DEFAULT_STEPS.len()
    );
    assert!(
        report2.baseline_already_present >= gents_migration::DEFAULT_BASELINE.len()
            || report2.baseline_registered + report2.baseline_already_present
                >= gents_migration::DEFAULT_BASELINE.len(),
        "re-run should be cheap/idempotent: {report2:?}"
    );

    for entry in gents_migration::DEFAULT_BASELINE {
        let cv = node
            .get_collection(entry.name)
            .expect("get_collection")
            .unwrap_or_else(|| panic!("missing collection {}", entry.name));
        assert!(cv.is_active, "{} should be active", entry.name);
        assert!(
            !cv.is_placeholder,
            "{} should not be placeholder",
            entry.name
        );
    }

    node.shutdown().await;
}

#[tokio::test]
async fn agent_request_baseline_is_chain_free_and_migrations_are_idempotent() {
    let node = fresh_node().await;
    let baseline = gents_migration::DEFAULT_BASELINE
        .iter()
        .find(|entry| entry.name == gents_protocol::schemas::AGENT_REQUEST_NAME)
        .expect("AgentRequest baseline");
    node.add_schema(baseline.sdl)
        .await
        .expect("register request baseline");

    let create = r#"mutation {
        create_AgentRequest(input: {
            request_id: "existing-request"
            node_did: "did:key:existing"
            session_id: "existing-session"
            content: "hello"
        }) { request_id content }
    }"#;
    let response = node.execute(create).await;
    assert!(
        !response.has_errors(),
        "create request: {:?}",
        response.errors
    );

    ensure_migrations(node.as_ref())
        .await
        .expect("apply production migrations");

    let cv = node
        .get_collection(gents_protocol::schemas::AGENT_REQUEST_NAME)
        .expect("get_collection")
        .expect("AgentRequest present");
    assert_eq!(Some(cv.version_id.as_str()), baseline.expected_version);

    let response = node
        .execute(
            r#"{ AgentRequest(filter: {request_id: {_eq: "existing-request"}}) {
                request_id content
            } }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "query request: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .expect("AgentRequest rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["request_id"], "existing-request");
    assert_eq!(rows[0]["content"], "hello");

    let rerun = ensure_migrations(node.as_ref())
        .await
        .expect("re-run ensure_migrations");
    assert_eq!(rerun.steps_applied, 0, "re-run applied steps: {rerun:?}");
    assert_eq!(
        rerun.steps_already_current,
        gents_migration::DEFAULT_STEPS.len()
    );
    let cv = node
        .get_collection(gents_protocol::schemas::AGENT_REQUEST_NAME)
        .expect("get_collection")
        .expect("AgentRequest present after re-run");
    assert_eq!(
        Some(cv.version_id.as_str()),
        baseline.expected_version,
        "re-run must not move AgentRequest off its baseline root"
    );

    node.shutdown().await;
}

#[tokio::test]
async fn goal_creation_claim_baseline_enforces_uniqueness() {
    let node = fresh_node().await;
    ensure_migrations(node.as_ref()).await.expect("baseline");
    let first = node
        .execute(
            r#"mutation { create_GoalCreationClaim(input: {
                creation_key: "goal-create:key"
                goal_id: "keyed-1"
                session_id: "session-1"
                node_did: "did:key:existing"
                objective: "objective"
                token_budget: 100
                created_at: "2026-09-02T00:00:00Z"
            }) { creation_key goal_id } }"#,
        )
        .await;
    assert!(!first.has_errors(), "first Goal claim: {:?}", first.errors);
    let duplicate = node
        .execute(
            r#"mutation { create_GoalCreationClaim(input: {
                creation_key: "goal-create:key"
                goal_id: "keyed-2"
                session_id: "session-1"
                node_did: "did:key:existing"
                objective: "different"
                token_budget: 200
                created_at: "2026-09-02T00:00:01Z"
            }) { creation_key goal_id } }"#,
        )
        .await;
    assert!(
        duplicate.has_errors(),
        "duplicate creation_key must violate the unique index"
    );
    node.shutdown().await;
}

#[tokio::test]
async fn rendered_request_reaches_a_pre_existing_store_through_the_baseline() {
    let node = fresh_node().await;
    for entry in gents_migration::DEFAULT_BASELINE {
        if entry.name == gents_protocol::schemas::RENDERED_REQUEST_NAME {
            continue;
        }
        node.add_schema(entry.sdl)
            .await
            .unwrap_or_else(|error| panic!("register {}: {error}", entry.name));
    }
    let create = r#"mutation {
        create_AgentSession(input: { session_id: "pre-upgrade-session" }) { session_id }
    }"#;
    let response = node.execute(create).await;
    assert!(
        !response.has_errors(),
        "seed pre-upgrade session: {:?}",
        response.errors
    );
    assert!(
        node.get_collection(gents_protocol::schemas::RENDERED_REQUEST_NAME)
            .expect("get_collection")
            .is_none(),
        "fixture must start without RenderedRequest"
    );

    ensure_migrations(node.as_ref())
        .await
        .expect("upgrade an existing store");

    let rendered = node
        .get_collection(gents_protocol::schemas::RENDERED_REQUEST_NAME)
        .expect("get_collection")
        .expect("RenderedRequest present after upgrade");
    assert!(rendered.is_active);
    assert!(!rendered.is_placeholder);
    let pin = gents_migration::DEFAULT_BASELINE
        .iter()
        .find(|entry| entry.name == gents_protocol::schemas::RENDERED_REQUEST_NAME)
        .and_then(|entry| entry.expected_version)
        .expect("RenderedRequest baseline pin");
    assert_eq!(rendered.version_id, pin);

    let response = node
        .execute(r#"{ AgentSession(filter: {session_id: {_eq: "pre-upgrade-session"}}) { session_id } }"#)
        .await;
    assert!(
        !response.has_errors(),
        "read pre-upgrade session: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentSession"))
        .and_then(serde_json::Value::as_array)
        .expect("AgentSession rows");
    assert_eq!(rows.len(), 1);

    let capture = r#"mutation {
        create_RenderedRequest(input: {
            capture_key: "rendered:v1:aaa"
            request_doc_id: "bae-req-1"
            request_id: "req-1"
            session_id: "pre-upgrade-session"
            node_did: "did:key:agent"
            requester_did: "did:key:requester"
            agent_id: "behavior-1"
            turn_index: 0
            attempt: 0
            capture_version: 1
            model_name: "test-model"
            source: "openai_responses"
            request_json: "{}"
            provenance_json: "{\"version\":1}"
            created_at: "2026-08-06T00:00:00Z"
        }) { capture_key turn_index attempt capture_version }
    }"#;
    let response = node.execute(capture).await;
    assert!(
        !response.has_errors(),
        "create RenderedRequest: {:?}",
        response.errors
    );
    let duplicate = node.execute(capture).await;
    assert!(
        duplicate.has_errors(),
        "unique capture_key must reject a second row: {:?}",
        duplicate.data
    );
    let response = node
        .execute(r#"{ RenderedRequest(filter: {capture_key: {_eq: "rendered:v1:aaa"}}) { capture_key } }"#)
        .await;
    assert!(
        !response.has_errors(),
        "read captures: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("RenderedRequest"))
        .and_then(serde_json::Value::as_array)
        .expect("RenderedRequest rows");
    assert_eq!(rows.len(), 1, "one capture per key, ever");

    let overwrite = node
        .execute(
            r#"mutation {
                update_RenderedRequest(
                    filter: { capture_key: { _eq: "rendered:v1:aaa" } }
                    input: { request_json: "{\"tampered\":true}" }
                ) { capture_key }
            }"#,
        )
        .await;
    assert!(
        overwrite.has_errors(),
        "@immutable must reject rewriting a captured request: {:?}",
        overwrite.data
    );

    node.shutdown().await;
}

#[tokio::test]
async fn multi_version_lineage_is_rejected() {
    let node = fresh_node().await;
    ensure_migrations(node.as_ref()).await.expect("baseline");

    let patch = r#"[{"op":"add","path":"/AgentRequest/Fields/-","value":{"Name":"__foreign_test_field","Kind":"String"}}]"#;
    let _ = node
        .patch_collection("AgentRequest", patch)
        .await
        .expect("foreign patch");

    let err = ensure_migrations(node.as_ref())
        .await
        .expect_err("foreign multi-version DAG must fail");
    assert!(err.is_foreign_version(), "unexpected error: {err}");
    assert!(!err.is_unknown_lineage());
    match err {
        Error::ForeignVersion { collection, .. } => assert_eq!(collection, "AgentRequest"),
        other => panic!("unexpected error: {other}"),
    }

    node.shutdown().await;
}

#[tokio::test]
async fn single_version_unknown_root_is_rejected() {
    const EXPECTED_SDL: &str = "type PinnedFixture { name: String label: String }";
    const FOREIGN_SDL: &str = "type PinnedFixture { name: String }";

    let authoring_node = fresh_node().await;
    authoring_node
        .add_schema(EXPECTED_SDL)
        .await
        .expect("register expected root");
    let expected_root = authoring_node
        .get_collection("PinnedFixture")
        .expect("load expected root")
        .expect("expected root exists")
        .version_id;
    authoring_node.shutdown().await;

    let node = fresh_node().await;
    node.add_schema(FOREIGN_SDL)
        .await
        .expect("register foreign root");
    let registry = DynamicRegistry {
        baseline: vec![BaselineCollectionOwned {
            name: "PinnedFixture".into(),
            sdl: EXPECTED_SDL.into(),
            expected_version: Some(expected_root),
            expected_state: CollectionExpectation::dag_only(),
        }],
        steps: vec![],
    };

    let err = ensure_migrations_dynamic(node.as_ref(), &registry)
        .await
        .expect_err("single unknown root must fail closed");
    assert!(
        matches!(err, Error::UnknownLineage { ref collection, .. } if collection == "PinnedFixture"),
        "unexpected error: {err}"
    );

    node.shutdown().await;
}

#[tokio::test]
async fn unknown_lineage_is_rejected_before_the_baseline_writes_the_store() {
    const EXPECTED_SDL: &str = "type PinnedFixture { name: String label: String }";
    const OLD_SDL: &str = "type PinnedFixture { name: String }";
    const ADDED_SDL: &str = "type AddedFixture { title: String }";

    let authoring_node = fresh_node().await;
    authoring_node
        .add_schema(EXPECTED_SDL)
        .await
        .expect("register expected root");
    let expected_root = authoring_node
        .get_collection("PinnedFixture")
        .expect("load expected root")
        .expect("expected root exists")
        .version_id;
    authoring_node.shutdown().await;

    let node = fresh_node().await;
    node.add_schema(OLD_SDL).await.expect("register old root");
    let before = node
        .get_all_collection_versions()
        .await
        .expect("versions before");
    let registry = DynamicRegistry {
        baseline: vec![
            BaselineCollectionOwned {
                name: "PinnedFixture".into(),
                sdl: EXPECTED_SDL.into(),
                expected_version: Some(expected_root),
                expected_state: CollectionExpectation::dag_only(),
            },
            BaselineCollectionOwned {
                name: "AddedFixture".into(),
                sdl: ADDED_SDL.into(),
                expected_version: None,
                expected_state: CollectionExpectation::dag_only(),
            },
        ],
        steps: vec![],
    };

    let err = ensure_migrations_dynamic(node.as_ref(), &registry)
        .await
        .expect_err("an older lineage must fail closed");
    assert!(err.is_unknown_lineage(), "unexpected error: {err}");
    assert!(
        node.get_collection("AddedFixture")
            .expect("load added collection")
            .is_none(),
        "rejecting an older store must not register this build's baseline into it"
    );
    let after = node
        .get_all_collection_versions()
        .await
        .expect("versions after");
    assert_eq!(
        before
            .iter()
            .map(|version| version.version_id.clone())
            .collect::<Vec<_>>(),
        after
            .iter()
            .map(|version| version.version_id.clone())
            .collect::<Vec<_>>(),
        "classification reads the version DAG without changing it"
    );

    node.shutdown().await;
}

#[tokio::test]
async fn empty_registry_injectable_for_tests() {
    let empty = Registry {
        baseline: &[],
        steps: &[],
        ordinary_indexes: &[],
    };
    let node = fresh_node().await;
    let report = ensure_migrations_with_registry(node.as_ref(), &empty)
        .await
        .expect("empty registry");
    assert_eq!(report.baseline_registered, 0);
    node.shutdown().await;
}

#[tokio::test]
async fn retained_file_images_require_a_native_owner_before_schema_install() {
    let directory = tempfile::tempdir().unwrap();
    let node = defra_node::EmbeddedNode::builder()
        .data_path(directory.path())
        .build()
        .await
        .unwrap();
    let error = ensure_migrations(&node).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("requires the configured node identity"),
        "{error}"
    );
    assert!(node.get_collection("ToolFileChange").unwrap().is_none());
    assert!(node.get_collection("AgentToolCall").unwrap().is_none());
    node.shutdown().await;
}

#[tokio::test]
async fn unprotected_file_images_are_rejected_without_changing_retained_data() {
    let node = common::fresh_node().await;
    let directive = format!(
        "@policy(id: \"{}\", resource: \"{}\")",
        gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_ID,
        gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_RESOURCE
    );
    let unprotected = gents_protocol::schemas::TOOL_FILE_CHANGE.replace(&directive, "");
    node.add_schema(&unprotected).await.unwrap();
    let created = node.execute("mutation { create_ToolFileChange(input: { tool_call_doc_id: \"retained-tool\" }) { _docID } }").await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let before = node
        .execute("{ ToolFileChange { _docID tool_call_doc_id } }")
        .await;
    let error = ensure_migrations(&node).await.unwrap_err();
    assert!(
        error.to_string().contains("incompatible document policy"),
        "{error}"
    );
    let after = node
        .execute("{ ToolFileChange { _docID tool_call_doc_id } }")
        .await;
    assert!(!after.has_errors(), "{:?}", after.errors);
    assert_eq!(after.data, before.data);
    assert!(node.get_collection("AgentToolCall").unwrap().is_none());
    assert!(node
        .get_collection("ToolFileChange")
        .unwrap()
        .unwrap()
        .policy
        .is_none());
    node.shutdown().await;
}

#[tokio::test]
async fn file_observation_install_preserves_retained_collection_lineages_and_documents() {
    let directory = tempfile::tempdir().unwrap();
    let identity = common::node_identity();
    let node = defra_node::EmbeddedNode::builder()
        .data_path(directory.path())
        .with_node_identity_did(&identity)
        .build()
        .await
        .unwrap();
    let baseline = gents_migration::DEFAULT_BASELINE
        .iter()
        .copied()
        .filter(|entry| {
            ![
                gents_protocol::schemas::TOOL_FILE_CHANGE_NAME,
                gents_protocol::schemas::FILE_IMAGE_HYDRATION_REQUEST_NAME,
                gents_protocol::schemas::FILE_IMAGE_HYDRATION_RECEIPT_NAME,
            ]
            .contains(&entry.name)
        })
        .collect::<Vec<_>>();
    let registry = Registry {
        baseline: &baseline,
        steps: gents_migration::DEFAULT_STEPS,
        ordinary_indexes: gents_migration::DEFAULT_REGISTRY.ordinary_indexes,
    };
    ensure_migrations_with_registry(&node, &registry)
        .await
        .unwrap();
    let created = node
        .execute(
            r#"mutation { create_AgentToolCall(input: {
        tool_call_key: "retained-session:retained-tool", request_id: "retained-request",
        request_doc_id: "retained-physical-request", session_id: "retained-session",
        node_did: "did:key:retained-node", requester_did: "did:key:retained-requester",
        tool_call_id: "retained-tool", lifecycle_state: "completed"
    }) { _docID } }"#,
        )
        .await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let query = "{ AgentToolCall { _docID tool_call_key request_id request_doc_id session_id node_did requester_did tool_call_id lifecycle_state } }";
    let before = node.execute(query).await;
    assert!(!before.has_errors(), "{:?}", before.errors);
    assert_eq!(
        before.data.as_ref().unwrap()["AgentToolCall"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let versions = baseline
        .iter()
        .map(|entry| {
            (
                entry.name,
                node.get_collection(entry.name).unwrap().unwrap().version_id,
            )
        })
        .collect::<Vec<_>>();
    node.shutdown().await;
    drop(node);

    let node = defra_node::EmbeddedNode::builder()
        .data_path(directory.path())
        .with_node_identity_did(&identity)
        .build()
        .await
        .unwrap();
    let report = ensure_migrations(&node).await.unwrap();
    assert_eq!(report.baseline_registered, 3);
    assert!(node
        .get_collection(gents_protocol::schemas::TOOL_FILE_CHANGE_NAME)
        .unwrap()
        .is_some());
    assert!(node
        .get_collection(gents_protocol::schemas::FILE_IMAGE_HYDRATION_REQUEST_NAME)
        .unwrap()
        .is_some());
    assert!(
        !node
            .get_collection(gents_protocol::schemas::FILE_IMAGE_HYDRATION_RECEIPT_NAME)
            .unwrap()
            .unwrap()
            .is_branchable
    );
    for (name, version) in versions {
        assert_eq!(
            node.get_collection(name).unwrap().unwrap().version_id,
            version,
            "{name}"
        );
    }
    let after = node.execute(query).await;
    assert!(!after.has_errors(), "{:?}", after.errors);
    assert_eq!(after.data, before.data);
    assert_eq!(
        ensure_migrations(&node).await.unwrap().baseline_registered,
        0
    );
    node.shutdown().await;
}

#[tokio::test]
async fn file_image_receipt_is_local_and_matches_its_pinned_root() {
    use gents_protocol::schemas::{
        FILE_IMAGE_HYDRATION_RECEIPT, FILE_IMAGE_HYDRATION_RECEIPT_NAME,
        TOOL_FILE_CHANGE_POLICY_ID, TOOL_FILE_CHANGE_POLICY_RESOURCE,
    };
    let node = fresh_node().await;
    node.add_schema(FILE_IMAGE_HYDRATION_RECEIPT).await.unwrap();
    let collection = node
        .get_collection(FILE_IMAGE_HYDRATION_RECEIPT_NAME)
        .unwrap()
        .unwrap();
    assert!(!collection.is_branchable);
    let policy = collection.policy.as_ref().unwrap();
    assert_eq!(policy.id, TOOL_FILE_CHANGE_POLICY_ID);
    assert_eq!(policy.resource_name, TOOL_FILE_CHANGE_POLICY_RESOURCE);
    assert!(gents_protocol::schemas::is_local_only_collection(
        FILE_IMAGE_HYDRATION_RECEIPT_NAME
    ));
    let baseline = gents_migration::DEFAULT_BASELINE
        .iter()
        .find(|entry| entry.name == FILE_IMAGE_HYDRATION_RECEIPT_NAME)
        .unwrap();
    assert_eq!(
        Some(collection.version_id.as_str()),
        baseline.expected_version
    );
    node.shutdown().await;
}
