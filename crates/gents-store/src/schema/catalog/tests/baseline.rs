use std::collections::BTreeSet;

use gents_protocol::schemas;

use super::common::{self, fresh_node};
use crate::node_scope::ContextCalls as _;
use crate::schema::catalog::{
    baseline, ensure, ensure_runtime, runtime_catalog, validate_existing_schema,
    BaselineCollection, Catalog, SchemaError,
};

#[test]
fn default_baseline_matches_ordered_protocol_catalog() {
    let actual = baseline(gents_protocol::schemas::HomePolicyIds::node_store())
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
}

#[test]
fn default_baseline_covers_every_protocol_collection_once() {
    let baseline_names = baseline(gents_protocol::schemas::HomePolicyIds::node_store())
        .iter()
        .map(|entry| entry.name)
        .collect::<BTreeSet<_>>();
    let protocol_names = gents_protocol::schemas::ALL_COLLECTION_NAMES
        .iter()
        .chain(gents_protocol::schemas::RUNTIME_COLLECTION_NAMES.iter())
        .copied()
        .collect::<BTreeSet<_>>();

    assert_eq!(
        baseline(gents_protocol::schemas::HomePolicyIds::node_store()).len(),
        baseline_names.len(),
        "baseline must not contain duplicate collections"
    );
    assert!(
        baseline(gents_protocol::schemas::HomePolicyIds::node_store())
            .iter()
            .all(|entry| entry.expected_version.is_some()),
        "production baseline must pin every root version"
    );
    assert_eq!(
        baseline_names, protocol_names,
        "baseline must cover the full protocol schema catalog"
    );
}

#[tokio::test]
async fn ensure_registers_the_baseline_on_its_roots_and_is_idempotent() {
    let node = fresh_node().await;
    let ids = schemas::HomePolicyIds::node_store();
    ensure_runtime(node.as_ref()).await.expect("first ensure");
    let first = node
        .get_all_collection_versions()
        .await
        .expect("versions after first ensure");
    for entry in baseline(ids).iter() {
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
        assert_eq!(Some(cv.version_id.as_str()), entry.expected_version);
    }
    for index in runtime_catalog(ids).ordinary_indexes {
        let collection = node
            .get_collection(index.collection)
            .expect("get_collection")
            .expect("indexed collection");
        let reconciled = collection
            .indexes
            .iter()
            .find(|existing| existing.name == index.name)
            .unwrap_or_else(|| panic!("missing ordinary index {}", index.name));
        assert_eq!(
            reconciled
                .fields
                .iter()
                .map(|field| (field.name.as_str(), field.descending))
                .collect::<Vec<_>>(),
            index
                .fields
                .iter()
                .map(|field| (field.name, field.descending))
                .collect::<Vec<_>>(),
            "{}",
            index.name
        );
    }

    let runtime = node
        .get_collection(schemas::NODE_RUNTIME_NAME)
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
    for removed in [
        "runnable_behavior_count",
        "unavailable_behavior_count",
        "process_state",
        "active_generation",
        "router_generation",
        "default_agent_id",
    ] {
        assert!(!field_names.contains(removed), "unexpected field {removed}");
    }
    assert!(field_names.contains("reconcile_phase"));
    assert!(field_names.contains("updated_at"));

    let inference_call = node
        .get_collection(schemas::INFERENCE_CALL_NAME)
        .unwrap()
        .unwrap();
    for name in ["session_id", "model_name", "profile_id"] {
        assert_eq!(
            inference_call
                .indexes
                .iter()
                .filter(|index| {
                    !index.resolved_unique()
                        && index.fields.len() == 1
                        && index.fields[0].name == name
                        && !index.fields[0].descending
                })
                .count(),
            1,
            "single-field index for InferenceCall.{name}"
        );
    }

    let runtime_did = identity::Did::new(node.node_identity_did().unwrap()).unwrap();
    for (name, resource) in schemas::IDENTITY_POLICY_BINDINGS
        .iter()
        .filter(|(name, _)| schemas::is_replicated_protocol_collection(name))
    {
        let collection = node.get_collection(name).unwrap().unwrap();
        assert!(collection.is_branchable);
        let acp = node.document_acp();
        assert!(acp
            .is_doc_registered(
                ids.id(schemas::BuiltInPolicy::Household),
                resource,
                &collection.collection_id,
            )
            .await
            .unwrap());
        assert_eq!(
            acp.get_doc_owner(
                ids.id(schemas::BuiltInPolicy::Household),
                resource,
                &collection.collection_id,
            )
            .await
            .unwrap(),
            Some(runtime_did.clone())
        );
    }

    for name in [
        schemas::TOOL_FILE_INVENTORY_NAME,
        schemas::FILE_INVENTORY_REPLY_NAME,
        schemas::FILE_INVENTORY_PROGRESS_NAME,
    ] {
        let collection = node.get_collection(name).unwrap().unwrap();
        let policy = collection.policy.unwrap();
        assert_eq!(policy.id, ids.id(schemas::BuiltInPolicy::ToolFileChange));
        assert_eq!(
            policy.resource_name,
            schemas::TOOL_FILE_CHANGE_POLICY_RESOURCE
        );
        let owner = node
            .document_acp()
            .get_doc_owner(
                ids.id(schemas::BuiltInPolicy::ToolFileChange),
                schemas::TOOL_FILE_CHANGE_POLICY_RESOURCE,
                &collection.collection_id,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(owner.as_str(), node.node_identity_did().unwrap());
    }
    assert!(schemas::is_local_only_collection(
        schemas::FILE_INVENTORY_REPLY_NAME
    ));
    assert!(schemas::is_local_only_collection(
        schemas::FILE_INVENTORY_PROGRESS_NAME
    ));
    assert!(!schemas::BRANCHABLE_COLLECTION_NAMES.contains(&schemas::FILE_INVENTORY_PROGRESS_NAME));
    assert!(!schemas::is_replicated_protocol_collection(
        schemas::FILE_INVENTORY_REQUEST_NAME
    ));

    validate_existing_schema(&node, ids)
        .await
        .expect("existing schema written by this build is accepted");

    ensure_runtime(node.as_ref()).await.expect("second ensure");
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(node.get_all_collection_versions().await.unwrap()).unwrap(),
        "a second ensure changes nothing"
    );

    let first_claim = node
        .execute(
            r#"mutation { create_GoalCreationClaim(input: {
                creation_key: "goal-create:key"
                goal_id: "keyed-1"
                session_id: "session-1"
                session_doc_id: "session-doc-1"
                node_did: "did:key:existing"
                objective: "objective"
                token_budget: 100
                created_at: "2026-09-02T00:00:00Z"
            }) { creation_key goal_id } }"#,
        )
        .await;
    assert!(
        !first_claim.has_errors(),
        "first Goal claim: {:?}",
        first_claim.errors
    );
    let duplicate = node
        .execute(
            r#"mutation { create_GoalCreationClaim(input: {
                creation_key: "goal-create:key"
                goal_id: "keyed-2"
                session_id: "session-1"
                session_doc_id: "session-doc-1"
                node_did: "did:key:existing"
                objective: "different"
                token_budget: 200
                created_at: "2026-09-02T00:00:01Z"
            }) { creation_key goal_id } }"#,
        )
        .await;
    assert!(
        duplicate.has_errors(),
        "duplicate creation_key in one session must violate the unique index"
    );
    let other_session = node
        .execute(
            r#"mutation { create_GoalCreationClaim(input: {
                creation_key: "goal-create:key"
                goal_id: "keyed-3"
                session_id: "session-2"
                session_doc_id: "session-doc-2"
                node_did: "did:key:existing"
                objective: "objective"
                token_budget: 100
                created_at: "2026-09-02T00:00:02Z"
            }) { creation_key goal_id } }"#,
        )
        .await;
    assert!(
        !other_session.has_errors(),
        "the claim key is scoped to its session: {:?}",
        other_session.errors
    );

    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn agent_request_on_its_root_keeps_its_rows_and_ensure_is_idempotent() {
    let node = fresh_node().await;
    let baseline = baseline(gents_protocol::schemas::HomePolicyIds::node_store())
        .iter()
        .find(|entry| entry.name == gents_protocol::schemas::AGENT_REQUEST_NAME)
        .expect("AgentRequest baseline");
    node.add_collection(&gents_protocol::schemas::HomePolicyIds::node_store().render(baseline.sdl))
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

    ensure_runtime(node.as_ref())
        .await
        .expect("ensure the runtime schema");

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

    ensure_runtime(node.as_ref()).await.expect("re-run ensure");
    let cv = node
        .get_collection(gents_protocol::schemas::AGENT_REQUEST_NAME)
        .expect("get_collection")
        .expect("AgentRequest present after re-run");
    assert_eq!(
        Some(cv.version_id.as_str()),
        baseline.expected_version,
        "re-run must not move AgentRequest off its baseline root"
    );

    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn rendered_request_reaches_a_pre_existing_store_through_the_baseline() {
    let node = fresh_node().await;
    for entry in baseline(gents_protocol::schemas::HomePolicyIds::node_store()).iter() {
        if entry.name == gents_protocol::schemas::RENDERED_REQUEST_NAME {
            continue;
        }
        node.add_collection(
            &gents_protocol::schemas::HomePolicyIds::node_store().render(entry.sdl),
        )
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

    ensure_runtime(node.as_ref())
        .await
        .expect("ensure an existing store");

    let rendered = node
        .get_collection(gents_protocol::schemas::RENDERED_REQUEST_NAME)
        .expect("get_collection")
        .expect("RenderedRequest present after upgrade");
    assert!(rendered.is_active);
    assert!(!rendered.is_placeholder);
    let pin = baseline(gents_protocol::schemas::HomePolicyIds::node_store())
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
            agent_did: "did:key:agent"
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

    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn multi_version_lineage_is_rejected() {
    let node = fresh_node().await;
    ensure_runtime(node.as_ref()).await.expect("baseline");

    let patch = r#"[{"op":"add","path":"/AgentRequest/Fields/-","value":{"Name":"__foreign_test_field","Kind":"String"}}]"#;
    common::patch_collection(&node, patch).await;

    let err = ensure_runtime(node.as_ref())
        .await
        .expect_err("foreign multi-version DAG must fail");
    match err {
        SchemaError::ForeignVersion { collection, .. } => assert_eq!(collection, "AgentRequest"),
        other => panic!("unexpected error: {other}"),
    }

    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn unknown_lineage_is_rejected_before_the_baseline_writes_the_store() {
    const EXPECTED_SDL: &str = "type PinnedFixture { name: String label: String }";
    const OLD_SDL: &str = "type PinnedFixture { name: String }";
    const ADDED_SDL: &str = "type AddedFixture { title: String }";

    let authoring_node = fresh_node().await;
    authoring_node
        .add_collection(&gents_protocol::schemas::HomePolicyIds::node_store().render(EXPECTED_SDL))
        .await
        .expect("register expected root");
    let expected_root = authoring_node
        .get_collection("PinnedFixture")
        .expect("load expected root")
        .expect("expected root exists")
        .version_id;
    authoring_node
        .shutdown()
        .await
        .expect("shut down test node");

    let node = fresh_node().await;
    node.add_collection(&gents_protocol::schemas::HomePolicyIds::node_store().render(OLD_SDL))
        .await
        .expect("register old root");
    let before = node
        .get_all_collection_versions()
        .await
        .expect("versions before");
    let catalog = Catalog {
        baseline: &[
            BaselineCollection {
                name: "PinnedFixture",
                sdl: EXPECTED_SDL,
                expected_version: Some(&expected_root),
                expected_collection_id: None,
            },
            BaselineCollection {
                name: "AddedFixture",
                sdl: ADDED_SDL,
                expected_version: None,
                expected_collection_id: None,
            },
        ],
        ordinary_indexes: &[],
    };

    let err = ensure(node.as_ref(), &catalog)
        .await
        .expect_err("an older lineage must fail closed");
    assert!(
        matches!(err, SchemaError::UnknownLineage { .. }),
        "unexpected error: {err}"
    );
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

    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn retained_file_images_require_a_native_owner_before_schema_install() {
    let directory = tempfile::tempdir().unwrap();
    let node = defra_node::Node::builder()
        .data_path(directory.path())
        .build()
        .await
        .unwrap();
    let error = ensure_runtime(&node).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("requires the configured node identity"),
        "{error}"
    );
    assert!(node.get_collection("ToolFileChange").unwrap().is_none());
    assert!(node.get_collection("AgentToolCall").unwrap().is_none());
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn unprotected_file_images_are_rejected_without_changing_retained_data() {
    let node = common::fresh_node().await;
    let directive = format!(
        "@policy(id: \"{}\", resource: \"{}\")",
        gents_protocol::schemas::HomePolicyIds::node_store()
            .id(gents_protocol::schemas::BuiltInPolicy::ToolFileChange),
        gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_RESOURCE
    );
    let unprotected = gents_protocol::schemas::HomePolicyIds::node_store()
        .render(gents_protocol::schemas::TOOL_FILE_CHANGE)
        .replace(&directive, "");
    node.add_collection(&gents_protocol::schemas::HomePolicyIds::node_store().render(&unprotected))
        .await
        .unwrap();
    let created = node.execute("mutation { create_ToolFileChange(input: { tool_call_doc_id: \"retained-tool\" }) { _docID } }").await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let before = node
        .execute("{ ToolFileChange { _docID tool_call_doc_id } }")
        .await;
    let error = ensure_runtime(&node).await.unwrap_err();
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
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn file_observation_install_preserves_retained_collection_lineages_and_documents() {
    let directory = tempfile::tempdir().unwrap();
    let identity = common::node_identity();
    let node = defra_node::Node::builder()
        .data_path(directory.path())
        .with_node_identity_did(&identity)
        .build()
        .await
        .unwrap();
    let baseline = baseline(gents_protocol::schemas::HomePolicyIds::node_store())
        .iter()
        .copied()
        .filter(|entry| entry.name != gents_protocol::schemas::TOOL_FILE_CHANGE_NAME)
        .collect::<Vec<_>>();
    let catalog = Catalog {
        baseline: &baseline,
        ordinary_indexes: runtime_catalog(gents_protocol::schemas::HomePolicyIds::node_store())
            .ordinary_indexes,
    };
    ensure(&node, &catalog).await.unwrap();
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
    node.shutdown().await.expect("shut down test node");
    drop(node);

    let node = defra_node::Node::builder()
        .data_path(directory.path())
        .with_node_identity_did(&identity)
        .build()
        .await
        .unwrap();
    ensure_runtime(&node).await.unwrap();
    let file_changes = node
        .get_collection(gents_protocol::schemas::TOOL_FILE_CHANGE_NAME)
        .unwrap()
        .expect("the missing baseline collection is registered");
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
    ensure_runtime(&node).await.unwrap();
    assert_eq!(
        node.get_collection(gents_protocol::schemas::TOOL_FILE_CHANGE_NAME)
            .unwrap()
            .unwrap(),
        file_changes
    );
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn removed_node_memory_is_not_registered_or_deleted() {
    let node = fresh_node().await;
    ensure_runtime(&node).await.unwrap();
    assert!(node.get_collection("NodeMemory").unwrap().is_none());

    node.add_collection("type NodeMemory { marker: String }")
        .await
        .unwrap();
    let created = node
        .execute(r#"mutation { create_NodeMemory(input: {marker: "preserved"}) { _docID } }"#)
        .await;
    assert!(!created.has_errors(), "{created:?}");
    ensure_runtime(&node).await.unwrap();
    let result = node.execute("{ NodeMemory { marker } }").await;
    assert!(!result.has_errors(), "{result:?}");
    assert_eq!(result.data.unwrap()["NodeMemory"][0]["marker"], "preserved");
    node.shutdown().await.expect("shut down test node");
}

/// `AgentMessage` is client-authored, so a fresh store and a retained store share its current
/// version, and a retained store's messages are left exactly as written (the provider request
/// id travels inside `publication`, not as a column).
#[tokio::test]
async fn agent_message_keeps_its_current_schema_and_rows() {
    assert!(!schemas::AGENT_MESSAGE.contains("native_request_id"));
    let canonical = baseline(gents_protocol::schemas::HomePolicyIds::node_store())
        .iter()
        .find(|entry| entry.name == "AgentMessage")
        .and_then(|entry| entry.expected_version)
        .unwrap();

    let node = fresh_node().await;
    node.add_collection(
        &gents_protocol::schemas::HomePolicyIds::node_store().render(schemas::AGENT_MESSAGE),
    )
    .await
    .unwrap();
    let legacy = node.get_collection("AgentMessage").unwrap().unwrap();
    assert_eq!(legacy.version_id, canonical);
    let created = node
        .execute(
            r#"mutation {
        add_AgentMessage(input: {
            message_key: "provider-turn" session_id: "session" node_did: "did:key:node"
            publication: "{\"kind\":\"request_execution\",\"native_request_id\":\"req_1\"}"
            outcome: "complete" sequence: 1
            role: "assistant" native_id: "msg_1" blocks: "[]" created_at: "2026-10-21T00:00:00Z"
        }) {_docID}
    }"#,
        )
        .await;
    assert!(!created.has_errors(), "seed legacy message: {created:?}");

    ensure_runtime(node.as_ref()).await.unwrap();
    let after = node.get_collection("AgentMessage").unwrap().unwrap();
    assert_eq!(after.version_id, canonical);
    for field in &legacy.fields {
        let current = after.fields.iter().find(|f| f.name == field.name);
        assert_eq!(
            current.map(|f| &f.id),
            Some(&field.id),
            "AgentMessage.{} field ID changed",
            field.name
        );
    }
    let read = node
        .execute(r#"{ AgentMessage { native_id publication } }"#)
        .await;
    assert!(!read.has_errors(), "{read:?}");
    let rows = &read.data.as_ref().unwrap()["AgentMessage"];
    assert_eq!(rows[0]["native_id"], "msg_1");
    ensure_runtime(node.as_ref()).await.unwrap();
    assert_eq!(
        node.get_collection("AgentMessage")
            .unwrap()
            .unwrap()
            .version_id,
        canonical
    );
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn runtime_locality_rejects_branchable_metadata_even_when_schema_cid_matches() {
    let node = fresh_node().await;
    let sdl = gents_protocol::schemas::HomePolicyIds::node_store()
        .render(schemas::AGENT)
        .replace("type Agent ", "type Agent @branchable ");
    assert_ne!(sdl, schemas::AGENT);
    node.add_collection(&gents_protocol::schemas::HomePolicyIds::node_store().render(&sdl))
        .await
        .unwrap();
    let before = node.get_collection(schemas::AGENT_NAME).unwrap().unwrap();
    let expected = baseline(gents_protocol::schemas::HomePolicyIds::node_store())
        .iter()
        .find(|entry| entry.name == schemas::AGENT_NAME)
        .unwrap();
    assert_eq!(before.version_id, expected.expected_version.unwrap());
    assert!(before.is_branchable);
    let error = ensure_runtime(&node).await.unwrap_err();
    assert!(
        matches!(error, SchemaError::RuntimeLocalityMismatch { collection } if collection == schemas::AGENT_NAME)
    );
    assert!(node.get_collection(schemas::NODE_NAME).unwrap().is_none());
    let after = node.get_collection(schemas::AGENT_NAME).unwrap().unwrap();
    assert_eq!(before, after);
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn existing_schema_from_another_version_is_refused_by_type_without_writing() {
    let node = fresh_node().await;
    validate_existing_schema(&node, gents_protocol::schemas::HomePolicyIds::node_store())
        .await
        .expect("an empty store is this build's to take");
    let branchable = schemas::HomePolicyIds::node_store()
        .render(schemas::AGENT)
        .replace("type Agent ", "type Agent @branchable ");
    node.add_collection(&branchable).await.unwrap();
    let error =
        validate_existing_schema(&node, gents_protocol::schemas::HomePolicyIds::node_store())
            .await
            .unwrap_err();
    assert!(
        matches!(error, SchemaError::RuntimeLocalityMismatch { ref collection } if collection == schemas::AGENT_NAME),
        "{error}"
    );
    assert!(node.get_collection(schemas::NODE_NAME).unwrap().is_none());
    node.shutdown().await.unwrap();

    let node = fresh_node().await;
    let directive = format!(
        "@policy(id: \"{}\", resource: \"{}\")",
        schemas::HomePolicyIds::node_store().id(schemas::BuiltInPolicy::ClientJournal),
        schemas::CLIENT_JOURNAL_POLICY_RESOURCE
    );
    node.add_collection(
        &schemas::HomePolicyIds::node_store()
            .render(schemas::PENDING_CHAT_SUBMISSION)
            .replace(&directive, ""),
    )
    .await
    .unwrap();
    let error =
        validate_existing_schema(&node, gents_protocol::schemas::HomePolicyIds::node_store())
            .await
            .unwrap_err();
    assert!(
        matches!(error, SchemaError::IncompatibleIdentityPolicy { ref collection } if collection == schemas::PENDING_CHAT_SUBMISSION_NAME),
        "{error}"
    );
    assert!(node.get_collection(schemas::NODE_NAME).unwrap().is_none());
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn unbound_client_journal_is_rejected_without_changing_data() {
    let node = fresh_node().await;
    let directive = format!(
        "@policy(id: \"{}\", resource: \"{}\")",
        schemas::HomePolicyIds::node_store().id(schemas::BuiltInPolicy::ClientJournal),
        schemas::CLIENT_JOURNAL_POLICY_RESOURCE
    );
    let unbound = gents_protocol::schemas::HomePolicyIds::node_store()
        .render(schemas::PENDING_CHAT_SUBMISSION)
        .replace(&directive, "");
    node.add_collection(&gents_protocol::schemas::HomePolicyIds::node_store().render(&unbound))
        .await
        .unwrap();
    let created = node.execute("mutation { create_PendingChatSubmission(input: { request_id: \"retained\", payload: \"data\" }) { _docID } }").await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let before = node
        .execute("{ PendingChatSubmission { _docID request_id payload } }")
        .await;
    let error = ensure_runtime(&node).await.unwrap_err();
    assert!(
        matches!(error, SchemaError::IncompatibleIdentityPolicy { ref collection } if collection == schemas::PENDING_CHAT_SUBMISSION_NAME)
    );
    let after = node
        .execute("{ PendingChatSubmission { _docID request_id payload } }")
        .await;
    assert!(!after.has_errors(), "{:?}", after.errors);
    assert_eq!(after.data, before.data);
    assert!(node.get_collection(schemas::NODE_NAME).unwrap().is_none());
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn unbound_mailbox_is_rejected_without_changing_data() {
    let node = fresh_node().await;
    let directive = format!(
        "@policy(id: \"{}\", resource: \"{}\")",
        schemas::HomePolicyIds::node_store().id(schemas::BuiltInPolicy::Mailbox),
        schemas::MAILBOX_POLICY_RESOURCE
    );
    let unbound = gents_protocol::schemas::HomePolicyIds::node_store()
        .render(schemas::MAILBOX_ITEM)
        .replace(&directive, "");
    node.add_collection(&gents_protocol::schemas::HomePolicyIds::node_store().render(&unbound))
        .await
        .unwrap();
    let created = node.execute("mutation { create_MailboxItem(input: { item_key: \"retained\", title: \"data\" }) { _docID } }").await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let query = "{ MailboxItem { _docID item_key title } }";
    let before = node.execute(query).await;
    let error = ensure_runtime(&node).await.unwrap_err();
    assert!(
        matches!(error, SchemaError::IncompatibleIdentityPolicy { ref collection } if collection == schemas::MAILBOX_ITEM_NAME)
    );
    let after = node.execute(query).await;
    assert!(!after.has_errors(), "{:?}", after.errors);
    assert_eq!(after.data, before.data);
    assert!(node.get_collection(schemas::NODE_NAME).unwrap().is_none());
    node.shutdown().await.unwrap();
}

/// Premise evidence for Lean `Identity.Household.catalog_restriction_keeps_policy_id`
/// and `registerPolicy`: the earlier Household policy, which typed
/// `agent_manager` as any actor, has the present policy's ID, and a store that
/// holds it refuses the present policy at that ID.
#[tokio::test]
async fn a_store_holding_the_earlier_household_policy_refuses_the_restricted_one() {
    let restricted = "      - name: agent_manager\n        types: [nodes->administrator]\n";
    let earlier = gents_protocol::schemas::AGENT_OWNERSHIP_POLICY.replacen(
        restricted,
        "      - name: agent_manager\n        types: [actor]\n",
        1,
    );
    assert_ne!(earlier, gents_protocol::schemas::AGENT_OWNERSHIP_POLICY);
    let dir = tempfile::tempdir().expect("tempdir");
    let node = defra_node::Node::builder()
        .data_path(dir.path())
        .with_node_identity_did(common::node_identity())
        .build()
        .await
        .expect("build node");
    let earlier_id = node
        .add_dac_policy_with_counter_as(&common::node_context(&node), &earlier, 0)
        .await
        .expect("earlier policy");
    assert_eq!(
        earlier_id,
        gents_protocol::schemas::HomePolicyIds::node_store()
            .id(gents_protocol::schemas::BuiltInPolicy::Household)
    );
    let error = ensure_runtime(&node)
        .await
        .expect_err("the restricted policy does not replace the earlier one");
    let error = format!("{error:#}");
    assert!(
        error.contains(
            gents_protocol::schemas::HomePolicyIds::node_store()
                .id(gents_protocol::schemas::BuiltInPolicy::Household)
        ),
        "{error}"
    );
    std::mem::forget(dir);
}
