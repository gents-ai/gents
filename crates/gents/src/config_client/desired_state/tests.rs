use super::*;
use crate::config_client::ConfigAccess;
use defra_node::EmbeddedNode;
use serde_json::json;
use std::sync::{Arc, Mutex};
use tracing_subscriber::layer::{Context as LayerContext, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

#[derive(Clone, Default)]
struct IgnoredWireWarningCapture(Arc<Mutex<Vec<tracing::Level>>>);

impl<S> Layer<S> for IgnoredWireWarningCapture
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _: LayerContext<'_, S>) {
        // The warning owner lives in gents-loop; gents::openai_wire only
        // re-exports its type and does not change the event's module target.
        if event.metadata().target() == "gents_loop::openai_wire" {
            self.0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(*event.metadata().level());
        }
    }
}

fn backend(owner: &str, id: &str) -> Value {
    json!({"agent_did":owner,"backend_id":id,"name":"Local","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:8000/v1","auth":{"kind":"unauthenticated"}})
}
fn document(value: Value) -> DesiredStateApplyDocument {
    DesiredStateApplyDocument {
        collection: Collection::InferenceBackend,
        add: value.clone(),
        update: value,
    }
}

#[test]
fn plan_decode_errors_identify_the_document_and_operation() {
    let valid = document(backend("did:key:owner", "local"));
    let incomplete = json!({"agent_did":"did:key:owner","system_prompt":"Inspect the host"});
    let error = DesiredStateApplyPlan::new(vec![
        valid,
        DesiredStateApplyDocument {
            collection: Collection::AgentContext,
            add: incomplete.clone(),
            update: incomplete,
        },
    ])
    .err()
    .expect("context ID is required");
    let diagnostic = format!("{error:#}");
    assert!(
        diagnostic.contains("documents[1] AgentContext create"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("context_id"), "{diagnostic}");

    let mut invalid_update = document(backend("did:key:owner", "local"));
    invalid_update.update["name"] = json!(42);
    let error = DesiredStateApplyPlan::new(vec![invalid_update])
        .err()
        .expect("name must be a string");
    let diagnostic = format!("{error:#}");
    assert!(
        diagnostic.contains("documents[0] InferenceBackend replacement"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("invalid type"), "{diagnostic}");
}
async fn register_config_schemas(node: &EmbeddedNode) -> Result<()> {
    register_config_schemas_dropping_unique_indexes(node, &[]).await
}

// Simulate a pre-index/replicated malformed identity set in duplicate rejection
// tests. Ordinary publication tests retain the production unique indexes.
async fn register_config_schemas_dropping_unique_indexes(
    node: &EmbeddedNode,
    duplicate_collections: &[Collection],
) -> Result<()> {
    for (name, schema) in gents_protocol::schemas::RUNTIME_COLLECTION_NAMES
        .iter()
        .zip(gents_protocol::schemas::RUNTIME_ALL)
        .chain(
            gents_protocol::schemas::ALL_COLLECTION_NAMES
                .iter()
                .zip(gents_protocol::schemas::ALL),
        )
    {
        if Collection::ALL
            .iter()
            .any(|collection| collection.graphql_type() == *name)
        {
            let mut schema = (*schema).to_owned();
            for collection in duplicate_collections {
                if collection.graphql_type() == *name {
                    schema = schema.replace(
                        &format!(
                            "@index(fields: [\"agent_did\", \"{}\"], unique: true)",
                            collection.unique_field()
                        ),
                        "",
                    );
                }
            }
            node.add_schema(&schema).await?;
        }
    }
    Ok(())
}

async fn apply(access: &ConfigAccess, docs: Vec<DesiredStateApplyDocument>) -> Result<()> {
    apply_with_counts(access, docs).await.map(|_| ())
}

async fn apply_with_counts(
    access: &ConfigAccess,
    docs: Vec<DesiredStateApplyDocument>,
) -> Result<DesiredStateApplyCounts> {
    let plan = DesiredStateApplyPlan::new(docs)?;
    access
        .transact("test.desired.apply", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await
}

#[tokio::test]
async fn ignored_wire_api_warns_once_per_changed_backend_through_common_apply_owner() -> Result<()>
{
    let node = Arc::new(EmbeddedNode::builder().build().await?);
    register_config_schemas(&node).await?;
    let access = ConfigAccess::Local(node.clone());
    let capture = IgnoredWireWarningCapture::default();
    let warnings = Arc::clone(&capture.0);
    let subscriber = tracing::Dispatch::new(Registry::default().with(capture));
    let _subscriber_guard = tracing::dispatcher::set_default(&subscriber);

    let mut backend = backend("did:key:owner", "ignored-wire");
    backend["provider_kind"] = json!("OpenRouter");
    backend["openai_wire_api"] = json!("responses");
    apply(&access, vec![document(backend.clone())]).await?;
    apply(&access, vec![document(backend.clone())]).await?;
    backend["openai_wire_api"] = json!("chat_completions");
    apply(&access, vec![document(backend)]).await?;

    assert_eq!(
        *warnings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
        vec![tracing::Level::WARN, tracing::Level::WARN],
        "the generic desired-state owner warns for create/change, never an unchanged replay"
    );
    node.shutdown().await;
    Ok(())
}

#[test]
fn every_canonical_collection_uses_derived_owner_and_identity_fields() {
    for collection in Collection::ALL {
        let (fields, _) = config_projection(collection, None).unwrap();
        assert!(fields.contains(&"agent_did"), "{collection:?}");
        assert!(
            fields.contains(&collection.unique_field()),
            "{collection:?}"
        );
        assert!(!fields.contains(&"_docID"), "{collection:?}");
    }
}

#[test]
fn plans_reject_unknown_fields_sparse_updates_and_scope_changes() {
    let mut value = backend("did:key:owner", "local");
    value["probe_status"] = "healthy".into();
    assert!(DesiredStateApplyPlan::new(vec![document(value)]).is_err());
    let mut doc = document(backend("did:key:owner", "local"));
    doc.update = json!({"enabled":false});
    assert!(DesiredStateApplyPlan::new(vec![doc]).is_err());
    let mut doc = document(backend("did:key:owner", "local"));
    doc.update["agent_did"] = "did:key:other".into();
    assert!(DesiredStateApplyPlan::new(vec![doc]).is_err());
    assert!(DesiredStateApplyPlan::new(vec![
        document(backend("did:key:a", "same")),
        document(backend("did:key:b", "same"))
    ])
    .is_ok());
    assert!(DesiredStateApplyPlan::new(vec![
        document(backend("did:key:a", "same")),
        document(backend("did:key:a", "same"))
    ])
    .is_err());
}

#[test]
fn commitments_do_not_reinterpret_authored_strings_as_json() {
    assert_ne!(
        desired_state_document_digest(&json!({"value":["{\"a\":1}"]})).unwrap(),
        desired_state_document_digest(&json!({"value":[{"a":1}]})).unwrap()
    );
}

#[test]
fn task_projection_excludes_runtime_updated_at_and_normalizes_defaults() {
    let desired = json!({
        "agent_did": "did:key:owner",
        "task_id": "run-once",
        "behavior_id": "operator",
        "prompt_template": "Run once"
    });
    let mut observed = desired.clone();
    observed["updated_at"] = "2026-09-10T18:16:43.843636Z".into();
    observed["enabled"] = true.into();
    observed["hooks"] = Value::Null;
    observed["tags"] = Value::Null;

    let desired = config_projection(Collection::Task, Some(&desired))
        .unwrap()
        .1
        .unwrap();
    let observed = config_projection(Collection::Task, Some(&observed))
        .unwrap()
        .1
        .unwrap();

    assert_eq!(desired, observed);
    assert_eq!(desired["enabled"], true);
    assert!(desired.get("updated_at").is_none());
}

#[tokio::test]
async fn replacement_resets_defaults_and_preserves_backend_observations() -> Result<()> {
    let node = Arc::new(EmbeddedNode::builder().build().await?);
    register_config_schemas(&node).await?;
    let access = ConfigAccess::Local(node.clone());
    let mut initial = backend("did:key:owner", "local");
    initial["max_concurrent"] = 7.into();
    initial["enabled"] = false.into();
    initial["tags"] = json!(["old"]);
    apply(
        &access,
        vec![
            document(initial),
            document(backend("did:key:other", "local")),
        ],
    )
    .await?;
    access.write("test.desired.observe", r#"mutation { update_InferenceBackend(filter:{agent_did:{_eq:"did:key:owner"},backend_id:{_eq:"local"}},input:{probe_status:"healthy",catalogs:{entries:[{agent_did:null,observed_at:"2026-01-01T00:00:00Z",models:null}]}}){_docID} }"#).await?;
    let before = node
        .execute("{InferenceBackend{_docID agent_did catalogs probe_status}}")
        .await;
    assert!(!before.has_errors());
    let before = before.data.unwrap();
    apply(&access, vec![document(backend("did:key:owner", "local"))]).await?;
    let after = node.execute("{InferenceBackend{_docID agent_did max_concurrent enabled tags catalogs probe_status}}").await;
    assert!(!after.has_errors());
    let after = after.data.unwrap();
    for row in after["InferenceBackend"].as_array().unwrap() {
        let prior = before["InferenceBackend"]
            .as_array()
            .unwrap()
            .iter()
            .find(|prior| prior["agent_did"] == row["agent_did"])
            .unwrap();
        assert_eq!(row["_docID"], prior["_docID"]);
        assert_eq!(row["catalogs"], prior["catalogs"]);
        assert_eq!(row["probe_status"], prior["probe_status"]);
        assert_eq!(row["enabled"], true);
        assert!(row["max_concurrent"].is_null());
        assert!(row["tags"].is_null());
    }
    Ok(())
}

#[tokio::test]
async fn ambiguous_scope_aborts_transaction_including_prior_staged_writes() -> Result<()> {
    let node = Arc::new(EmbeddedNode::builder().build().await?);
    register_config_schemas_dropping_unique_indexes(&node, &[Collection::InferenceBackend]).await?;
    let access = ConfigAccess::Local(node.clone());
    apply(
        &access,
        vec![document(backend("did:key:owner", "z_duplicate"))],
    )
    .await?;
    let mut duplicate = backend("did:key:owner", "z_duplicate");
    duplicate["name"] = "Distinct document".into();
    access
        .write(
            "test.desired.duplicate",
            &format!(
                "mutation {{ create_InferenceBackend(input:{}){{_docID}} }}",
                gents_protocol::graphql::graphql_input_literal(&duplicate)?
            ),
        )
        .await?;
    assert!(apply(
        &access,
        vec![
            document(backend("did:key:owner", "a_staged")),
            document(backend("did:key:owner", "z_duplicate"))
        ]
    )
    .await
    .is_err());
    let response = node.execute("{InferenceBackend{backend_id name}}").await;
    assert!(!response.has_errors());
    let rows = response.data.unwrap();
    let rows = rows["InferenceBackend"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row["backend_id"] == "z_duplicate"));
    assert!(rows.iter().any(|row| row["name"] == "Distinct document"));
    Ok(())
}

fn config(collection: Collection, value: Value) -> DesiredStateApplyDocument {
    DesiredStateApplyDocument {
        collection,
        add: value.clone(),
        update: value,
    }
}

fn cyclic_configuration(owner: &str) -> Vec<DesiredStateApplyDocument> {
    vec![
        document(backend(owner, "backend")),
        config(
            Collection::InferenceProfile,
            json!({"agent_did":owner,"profile_id":"profile","backend_id":"backend","model_name":"model"}),
        ),
        config(
            Collection::AgentBehavior,
            json!({"agent_did":owner,"behavior_id":"behavior","inference_profile_id":"profile","context_id":"context"}),
        ),
        config(
            Collection::AgentContext,
            json!({"agent_did":owner,"context_id":"context","tools_id":"tools"}),
        ),
        config(
            Collection::Tools,
            json!({"agent_did":owner,"tools_id":"tools","subagents":{"target_ids":["target"]}}),
        ),
        config(
            Collection::SubagentTarget,
            json!({"agent_did":owner,"target_id":"target","target_agent_did":owner,"behavior_id":"behavior","name":"worker"}),
        ),
    ]
}

#[tokio::test]
async fn retained_inbound_references_and_cycles_share_atomic_publication() -> Result<()> {
    let node = Arc::new(EmbeddedNode::builder().build().await?);
    register_config_schemas(&node).await?;
    let access = ConfigAccess::Local(node.clone());
    let owner = "did:key:owner";
    let mut documents = cyclic_configuration(owner);
    documents.extend(cyclic_configuration("did:key:foreign"));
    let seeded = apply_with_counts(&access, documents).await?;
    // DesiredStateApplyCounts ownership: one document per collection per
    // owner cycles through as a staged write.
    for collection in cyclic_configuration(owner).iter().map(|doc| doc.collection) {
        assert_eq!(seeded.get(collection), 2, "{collection:?}");
    }
    access.write("test.observe", r#"mutation { update_InferenceBackend(filter:{agent_did:{_eq:"did:key:owner"}},input:{probe_status:"healthy"}){_docID} }"#).await?;
    let before = node.execute("{InferenceBackend{_docID agent_did name probe_status} AgentContext{_docID agent_did context_id}}").await.data;

    // The read-only preflight (`validate_desired_state_plan`) rejects the
    // same broken replacement the publication path rejects, on this fixture.
    let mut drifted_replacement = cyclic_configuration(owner)
        .into_iter()
        .find(|doc| doc.collection == Collection::AgentBehavior)
        .unwrap();
    drifted_replacement.update["context_id"] = "absent".into();
    let preview_plan = DesiredStateApplyPlan::new(vec![drifted_replacement])?;
    assert!(access
        .transact("test.preview.replacement", |txn| {
            let plan = &preview_plan;
            Box::pin(async move { validate_desired_state_plan(txn, plan).await })
        })
        .await
        .is_err());
    // ... and rejects a removal that would break a live inbound reference.
    let preview_removal = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
        Collection::AgentContext,
        owner.into(),
        "context".into(),
    )])?;
    assert!(access
        .transact("test.preview.removal", |txn| {
            let plan = &preview_removal;
            Box::pin(async move { validate_desired_state_plan(txn, plan).await })
        })
        .await
        .is_err());
    // ... while an addition validates AND commits zero writes.
    let preview_addition =
        DesiredStateApplyPlan::new(vec![document(backend(owner, "preview-only"))])?;
    access
        .transact("test.preview.addition", |txn| {
            let plan = &preview_addition;
            Box::pin(async move { validate_desired_state_plan(txn, plan).await })
        })
        .await?;
    let previewed = node.execute("{InferenceBackend{backend_id}}").await;
    assert!(!previewed.has_errors(), "{:?}", previewed.errors);
    assert!(
        !previewed.data.unwrap()["InferenceBackend"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["backend_id"] == "preview-only"),
        "validate_desired_state_plan must be read-only"
    );

    // The unchanged behavior still references context. The foreign same-label
    // context must not satisfy that link, and a staged backend edit rolls back.
    let mut edited = backend(owner, "backend");
    edited["name"] = "must roll back".into();
    let plan = DesiredStateApplyPlan::new(vec![document(edited)])?.with_removals(vec![(
        Collection::AgentContext,
        owner.into(),
        "context".into(),
    )])?;
    let error = access
        .transact("test.reject.removal", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("references missing AgentContext"));
    let after = node.execute("{InferenceBackend{_docID agent_did name probe_status} AgentContext{_docID agent_did context_id}}").await.data;
    assert_eq!(
        before, after,
        "failed publication preserves desired fields and observations"
    );

    // Removing the complete same-owner cycle is legal, regardless of order.
    let removals = cyclic_configuration(owner)
        .iter()
        .map(|doc| {
            (
                doc.collection,
                owner.to_owned(),
                doc.add[doc.collection.unique_field()]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            )
        })
        .collect();
    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(removals)?;
    access
        .transact("test.remove.cycle", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await?;
    let remaining = node
        .execute("{AgentBehavior{agent_did} InferenceBackend{agent_did}}")
        .await;
    assert!(!remaining.has_errors());
    for collection in ["AgentBehavior", "InferenceBackend"] {
        let rows = remaining.data.as_ref().unwrap()[collection]
            .as_array()
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["agent_did"], "did:key:foreign");
    }
    Ok(())
}

#[tokio::test]
async fn replacement_checks_actual_update_and_unchanged_duplicate_rows() -> Result<()> {
    let node = Arc::new(EmbeddedNode::builder().build().await?);
    register_config_schemas_dropping_unique_indexes(&node, &[Collection::AgentContext]).await?;
    let access = ConfigAccess::Local(node.clone());
    let owner = "did:key:owner";
    apply(&access, cyclic_configuration(owner)).await?;
    let mut replacement = cyclic_configuration(owner)
        .into_iter()
        .find(|doc| doc.collection == Collection::AgentBehavior)
        .unwrap();
    replacement.update["context_id"] = "absent".into();
    assert!(apply(&access, vec![replacement]).await.is_err());
    let row = node.execute("{AgentBehavior{context_id}}").await;
    assert_eq!(
        row.data.unwrap()["AgentBehavior"][0]["context_id"],
        "context"
    );

    access.write("test.duplicate.context", r#"mutation { create_AgentContext(input:{agent_did:"did:key:owner",context_id:"context",description:"duplicate"}){_docID} }"#).await?;
    let error = apply(
        &access,
        vec![document(backend(owner, "unrelated-new-backend"))],
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("multiple live AgentContext"));
    let rows = node
        .execute("{InferenceBackend{backend_id}}")
        .await
        .data
        .unwrap();
    assert_eq!(rows["InferenceBackend"].as_array().unwrap().len(), 1);
    Ok(())
}

#[tokio::test]
async fn expect_existing_documents_unchanged_rejects_drifted_live_rows() -> Result<()> {
    // Gap test: `expect_existing_documents_unchanged` guards graph-package
    // re-install against out-of-band mutation of live package documents.
    let node = Arc::new(EmbeddedNode::builder().build().await?);
    register_config_schemas(&node).await?;
    let access = ConfigAccess::Local(node.clone());
    let owner = "did:key:owner";
    apply(&access, vec![document(backend(owner, "packaged"))]).await?;

    // The matching plan (authored digest == live projection) verifies clean.
    let matching = DesiredStateApplyPlan::new(vec![document(backend(owner, "packaged"))])?;
    access
        .transact("test.verify.matching", |txn| {
            let plan = &matching;
            Box::pin(async move { expect_existing_documents_unchanged(txn, plan).await })
        })
        .await?;

    // Mutate a non-committed live field out of band; the same plan now drifts.
    access.write("test.drift", r#"mutation { update_InferenceBackend(filter:{agent_did:{_eq:"did:key:owner"},backend_id:{_eq:"packaged"}},input:{name:"out-of-band"}){_docID} }"#).await?;
    let error = access
        .transact("test.verify.drifted", |txn| {
            let plan = &matching;
            Box::pin(async move { expect_existing_documents_unchanged(txn, plan).await })
        })
        .await
        .unwrap_err();
    assert!(
        stale_expectation(&error).is_some(),
        "drifted live row must be rejected: {error:#}"
    );
    Ok(())
}

#[test]
fn removals_require_unique_scoped_identities() {
    assert!(DesiredStateApplyPlan::new(Vec::new())
        .unwrap()
        .with_removals(vec![(Collection::Tools, "".into(), "tools".into())])
        .is_err());
    assert!(
        DesiredStateApplyPlan::new(vec![document(backend("owner", "backend"))])
            .unwrap()
            .with_removals(vec![(
                Collection::InferenceBackend,
                "owner".into(),
                "backend".into()
            )])
            .is_err()
    );
    assert!(DesiredStateApplyPlan::new(Vec::new())
        .unwrap()
        .with_removals(vec![
            (Collection::Tools, "owner".into(), "tools".into()),
            (Collection::Tools, "owner".into(), "tools".into())
        ])
        .is_err());
}

fn expectation(owner: &str, id: &str, digest: Option<String>) -> DesiredStateExpectation {
    DesiredStateExpectation {
        collection: Collection::InferenceBackend,
        owner: owner.to_owned(),
        id: id.to_owned(),
        digest,
    }
}

async fn cas_node() -> Result<(ConfigAccess, &'static str)> {
    let node = Arc::new(EmbeddedNode::builder().build().await?);
    register_config_schemas(&node).await?;
    Ok((ConfigAccess::Local(node), "did:key:owner"))
}

async fn live_digest(access: &ConfigAccess, owner: &str, id: &str) -> Result<Option<String>> {
    let (owner, id) = (owner.to_owned(), id.to_owned());
    access
        .transact("test.cas.read", |txn| {
            let (owner, id) = (owner.clone(), id.clone());
            Box::pin(async move {
                read_desired_state_document_in_txn(txn, Collection::InferenceBackend, &owner, &id)
                    .await?
                    .map(|live| desired_state_document_digest(&live))
                    .transpose()
            })
        })
        .await
}

fn renamed(owner: &str, id: &str, name: &str) -> Value {
    let mut value = backend(owner, id);
    value["name"] = name.into();
    value
}

#[tokio::test]
async fn matching_expectation_applies_the_plan() -> Result<()> {
    let (access, owner) = cas_node().await?;
    apply(&access, vec![document(backend(owner, "target"))]).await?;
    let digest = live_digest(&access, owner, "target").await?;
    let plan = DesiredStateApplyPlan::new(vec![document(renamed(owner, "target", "Promoted"))])?
        .with_expected(vec![expectation(owner, "target", digest)])?;
    access
        .transact("test.cas.apply", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await?;
    assert_ne!(live_digest(&access, owner, "target").await?, None);
    Ok(())
}

#[tokio::test]
async fn drifted_expectation_refuses_and_writes_nothing() -> Result<()> {
    let (access, owner) = cas_node().await?;
    apply(
        &access,
        vec![
            document(backend(owner, "target")),
            document(backend(owner, "other")),
        ],
    )
    .await?;
    let frozen_other = live_digest(&access, owner, "other").await?;
    let frozen_target = live_digest(&access, owner, "target").await?;
    // An operator edits a closure document after the freeze.
    apply(&access, vec![document(renamed(owner, "other", "Edited"))]).await?;
    let before = live_digest(&access, owner, "target").await?;

    let plan = DesiredStateApplyPlan::new(vec![document(renamed(owner, "target", "Promoted"))])?
        .with_expected(vec![
            expectation(owner, "target", frozen_target),
            expectation(owner, "other", frozen_other.clone()),
        ])?;
    let error = access
        .transact("test.cas.stale", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap_err();

    let stale = stale_expectation(&error).expect("typed StaleExpectation");
    assert_eq!(stale.drifted.len(), 1);
    assert_eq!(stale.drifted[0].id, "other");
    assert_eq!(stale.drifted[0].expected, frozen_other);
    assert_eq!(
        live_digest(&access, owner, "target").await?,
        before,
        "target must be untouched"
    );
    Ok(())
}

#[tokio::test]
async fn absent_expectations_are_checked_in_both_directions() -> Result<()> {
    let (access, owner) = cas_node().await?;
    apply(&access, vec![document(backend(owner, "present"))]).await?;

    let must_be_absent = DesiredStateApplyPlan::new(vec![document(backend(owner, "fresh"))])?
        .with_expected(vec![expectation(owner, "fresh", None)])?;
    access
        .transact("test.cas.absent_ok", |txn| {
            let plan = &must_be_absent;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await?;

    let wrongly_absent = DesiredStateApplyPlan::new(vec![document(backend(owner, "again"))])?
        .with_expected(vec![expectation(owner, "present", None)])?;
    let error = access
        .transact("test.cas.absent_stale", |txn| {
            let plan = &wrongly_absent;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap_err();
    assert!(stale_expectation(&error).is_some());

    let wrongly_present =
        DesiredStateApplyPlan::new(vec![document(backend(owner, "third"))])?.with_expected(
            vec![expectation(owner, "missing", Some("sha256:00".into()))],
        )?;
    let error = access
        .transact("test.cas.present_stale", |txn| {
            let plan = &wrongly_present;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap_err();
    let stale = stale_expectation(&error).expect("typed StaleExpectation");
    assert_eq!(stale.drifted[0].found, None);
    Ok(())
}

/// `ApplyReconcile.publishIf` refinement: the Lean model compares desired
/// fields, Rust compares digests. Each Lean `content` string is mapped onto the
/// `name` field of an `InferenceBackend`, so two rows share a digest exactly
/// when they share a Lean content. The Lean collection is deliberately ignored
/// (ids are distinct across the scenario's collections) and the Lean `refs` are
/// not materialized: reference closure is a separate, already-tested gate and
/// these cases exercise only the expectation precondition.
#[tokio::test]
async fn guarded_publication_matches_lean_publish_if_cases() -> Result<()> {
    use crate::lean_vocab_test::lean_publish_if_cases;

    fn doc_for(owner: &str, id: &str, content: &str) -> Value {
        renamed(owner, id, content)
    }

    fn authored_digest(owner: &str, id: &str, content: &str) -> Result<String> {
        let (_, projected) = config_projection(
            Collection::InferenceBackend,
            Some(&doc_for(owner, id, content)),
        )?;
        desired_state_document_digest(&projected.context("projection")?)
    }

    let mut exercised = Vec::new();
    for case in lean_publish_if_cases() {
        let (access, _) = cas_node().await?;
        let prior = case
            .pre_desired
            .iter()
            .map(|row| document(doc_for(&row.target.agent_did, &row.target.id, &row.content)))
            .collect::<Vec<_>>();
        apply(&access, prior).await?;

        let expected = case
            .expected
            .iter()
            .map(|row| {
                let digest = row
                    .content
                    .as_deref()
                    .map(|content| authored_digest(&row.target.agent_did, &row.target.id, content))
                    .transpose()?;
                Ok(expectation(&row.target.agent_did, &row.target.id, digest))
            })
            .collect::<Result<Vec<_>>>()?;
        let plan = DesiredStateApplyPlan::new(
            case.candidate
                .iter()
                .map(|row| document(doc_for(&row.target.agent_did, &row.target.id, &row.content)))
                .collect(),
        )?
        .with_expected(expected)?;

        let outcome = access
            .transact("test.cas.lean", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await })
            })
            .await;
        assert_eq!(outcome.is_ok(), case.applied, "case {}", case.name);
        if let Err(error) = &outcome {
            assert!(
                stale_expectation(error).is_some(),
                "case {}: {error:#}",
                case.name
            );
        }

        let mut after_first = Vec::new();
        for row in &case.expected_after_desired {
            let live = live_digest(&access, &row.target.agent_did, &row.target.id).await?;
            let want = authored_digest(&row.target.agent_did, &row.target.id, &row.content)?;
            assert_eq!(
                live,
                Some(want),
                "case {} document {}",
                case.name,
                row.target.id
            );
            after_first.push(live);
        }

        // Witness for `publishIf_idempotent`: an expectation names the
        // pre-publication digest, so replaying an applied plan refuses once
        // the publication moved a document the scope names, and is a
        // legitimate no-op when it moved none (an empty scope, or a scope
        // naming only documents the candidate never writes). Decide which
        // from the emitted post-state, never from the case name; either way
        // the replay leaves every published digest exactly as it found it,
        // which is the shape promotion retry relies on.
        if case.applied {
            let replay_holds = case.expected.iter().all(|expectation| {
                case.expected_after_desired
                    .iter()
                    .find(|row| row.target == expectation.target)
                    .map(|row| row.content.as_str())
                    == expectation.content.as_deref()
            });
            let replay = access
                .transact("test.cas.lean.replay", |txn| {
                    let plan = &plan;
                    Box::pin(async move { apply_desired_state_plan(txn, plan).await })
                })
                .await;
            assert_eq!(
                replay.is_ok(),
                replay_holds,
                "case {} replay: {replay:?}",
                case.name
            );
            if let Err(error) = &replay {
                assert!(
                    stale_expectation(error).is_some(),
                    "case {} replay: {error:#}",
                    case.name
                );
            }
            for (row, first) in case.expected_after_desired.iter().zip(&after_first) {
                assert_eq!(
                    live_digest(&access, &row.target.agent_did, &row.target.id).await?,
                    *first,
                    "case {} document {} changed on replay",
                    case.name,
                    row.target.id
                );
            }
        }
        exercised.push(case.name.as_str());
    }
    // Every emitted scenario must be exercised: a case reaches this list only
    // after all of its assertions hold, so emitter drift fails loudly here.
    assert_eq!(
        exercised,
        [
            "all_expectations_match",
            "target_drifted",
            "closure_document_drifted",
            "expected_absent_but_present",
            "expected_absent_and_absent",
            "expected_present_but_absent",
            "empty_scope_is_publish",
        ]
    );
    Ok(())
}

#[test]
fn duplicate_expectations_are_rejected() {
    let plan = DesiredStateApplyPlan::new(Vec::new()).unwrap();
    let error = plan
        .with_expected(vec![
            expectation("did:key:owner", "a", None),
            expectation("did:key:owner", "a", None),
        ])
        .unwrap_err();
    assert!(format!("{error:#}").contains("duplicate expectation"));
}

#[tokio::test]
async fn canonical_replacement_preserves_revocation_and_attested_creation() {
    use crate::config_client::{apply_desired_state_plan, ConfigAccess, DesiredStateApplyPlan};
    use std::sync::Arc;
    let node = Arc::new(
        crate::defra_node::EmbeddedNode::builder()
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let access = ConfigAccess::Local(node.clone());
    let original: crate::document_config::PackConfig = serde_json::from_value(serde_json::json!({
        "agent_principal":{"agent_did":"binding-owner"},
        "chain_key_bindings":[{"agent_did":"binding-owner","binding_id":"signing","address":"0x1111111111111111111111111111111111111111","key_backend":"keyring","attestation":"signed-owner-binding","created_at":"2026-01-01T00:00:00Z","revoked_at":"2026-01-02T00:00:00Z"}]
    })).unwrap();
    let plan = DesiredStateApplyPlan::from_pack_config(&original).unwrap();
    access
        .transact("seed_revoked_binding", |txn| {
            let plan = plan.clone();
            Box::pin(async move { apply_desired_state_plan(txn, &plan).await.map(|_| ()) })
        })
        .await
        .unwrap();
    for proposed in [
        None,
        Some(serde_json::Value::Null),
        Some(serde_json::json!("")),
        Some(serde_json::json!(" \t ")),
    ] {
        let mut raw = serde_json::to_value(&original).unwrap();
        let binding = raw["chain_key_bindings"][0].as_object_mut().unwrap();
        binding.insert(
            "created_at".into(),
            serde_json::json!("2026-09-01T00:00:00Z"),
        );
        binding.insert("tags".into(), serde_json::json!(["updated"]));
        if let Some(proposed) = proposed {
            binding.insert("revoked_at".into(), proposed);
        } else {
            binding.remove("revoked_at");
        }
        let config = serde_json::from_value(raw).unwrap();
        let plan = DesiredStateApplyPlan::from_pack_config(&config).unwrap();
        access
            .transact("stale_binding_update", |txn| {
                let plan = plan.clone();
                Box::pin(async move { apply_desired_state_plan(txn, &plan).await.map(|_| ()) })
            })
            .await
            .unwrap();
        let read=node.execute(r#"{ChainKeyBinding(filter:{agent_did:{_eq:"binding-owner"},binding_id:{_eq:"signing"}}){created_at revoked_at tags}}"#).await;
        assert!(!read.has_errors(), "{:?}", read.errors);
        let rows = read.data.unwrap()["ChainKeyBinding"].clone();
        assert_eq!(
            rows,
            serde_json::json!([{"created_at":"2026-01-01T00:00:00Z","revoked_at":"2026-01-02T00:00:00Z","tags":["updated"]}])
        );
    }
    node.shutdown().await;
}

#[tokio::test]
async fn preview_rejects_a_context_window_above_the_advertised_maximum() -> Result<()> {
    let node = Arc::new(EmbeddedNode::builder().build().await?);
    register_config_schemas(&node).await?;
    let access = ConfigAccess::Local(node.clone());
    let owner = "did:key:owner";
    let profile = |window: Value| {
        config(
            Collection::InferenceProfile,
            json!({"agent_did":owner,"profile_id":"profile","backend_id":"backend","model_name":"model","context_window":window}),
        )
    };
    apply(
        &access,
        vec![document(backend(owner, "backend")), profile(Value::Null)],
    )
    .await?;
    let typed: crate::document_config::InferenceBackend =
        serde_json::from_value(backend(owner, "backend"))?;
    crate::backend_registry::record_model_catalog(
        &node,
        &typed,
        serde_json::from_value(json!({
            "agent_did": null,
            "observed_at": "2026-09-25T00:00:00Z",
            "models": [{"model_name":"model","context_window":272000,"max_context_window":872000}],
        }))?,
    )
    .await?;

    let rejected = preview_window(&access, &profile, 900_000)
        .await
        .expect_err("above the advertised maximum");
    assert!(
        format!("{rejected:#}").contains("exceeds model model advertised maximum 872000"),
        "{rejected:#}"
    );
    preview_window(&access, &profile, 500_000).await?;
    preview_window(&access, &profile, 872_000).await?;
    Ok(())
}

async fn preview_window(
    access: &ConfigAccess,
    profile: &dyn Fn(Value) -> DesiredStateApplyDocument,
    window: u64,
) -> Result<()> {
    let plan = DesiredStateApplyPlan::new(vec![profile(json!(window))])?;
    access
        .transact("test.preview.context_window", |txn| {
            let plan = &plan;
            Box::pin(async move { validate_desired_state_plan(txn, plan).await })
        })
        .await
}

fn obligation_surface(
    owner: &str,
    surface_id: &str,
    collection: &str,
    count_field: &str,
) -> DesiredStateApplyDocument {
    let surface = json!({
        "surface_id": surface_id,
        "agent_did": owner,
        "entries": {"entries": [{
            "tool_name": "write_outcome",
            "collection": collection,
            "description": "Write one outcome document.",
            "fields": [
                {"name": "text", "required": true},
                {"name": count_field, "required": true},
            ],
            "output_obligation": {
                "scope": "request",
                "minimum_writes": 1,
                "expected_count_field": count_field,
            },
        }]},
    });
    let surface: crate::document_config::DatastoreToolSurfaceDocument =
        serde_json::from_value(surface).expect("canonical surface");
    let value = serde_json::to_value(surface).expect("canonical surface value");
    DesiredStateApplyDocument {
        collection: Collection::DatastoreToolSurface,
        add: value.clone(),
        update: value,
    }
}

async fn obligation_node() -> Result<Arc<EmbeddedNode>> {
    let node = Arc::new(EmbeddedNode::builder().build().await?);
    register_config_schemas(&node).await?;
    node.add_schema(
        "type ObligationOutcome { text: String result: String expected_total: String total: Int ratio: Float32 amount: Float64 payload: JSON observed_at: DateTime attachment: Blob marker: ID flag: Boolean tags: [String] required_total: Int! required_label: String! required_tags: [String!] }",
    )
    .await?;
    Ok(node)
}

#[tokio::test]
async fn output_obligation_count_field_must_hold_a_count_on_the_target_collection() -> Result<()> {
    let node = obligation_node().await?;
    let access = ConfigAccess::Local(node.clone());
    let owner = "did:key:obligation-owner";

    for (count_field, reported_type) in [
        ("flag", "Boolean"),
        ("tags", "LIST"),
        ("required_tags", "LIST"),
    ] {
        let refused = apply(
            &access,
            vec![obligation_surface(
                owner,
                "outcomes",
                "ObligationOutcome",
                count_field,
            )],
        )
        .await
        .err()
        .expect("the field can never carry the expected count");
        let diagnostic = format!("{refused:#}");
        assert!(diagnostic.contains("expected_count_field"), "{diagnostic}");
        assert!(diagnostic.contains(reported_type), "{diagnostic}");
    }

    let absent_field = apply(
        &access,
        vec![obligation_surface(
            owner,
            "outcomes",
            "ObligationOutcome",
            "missing",
        )],
    )
    .await
    .err()
    .expect("the count field must exist on the target collection");
    assert!(
        format!("{absent_field:#}").contains("does not exist on ObligationOutcome"),
        "{absent_field:#}"
    );

    // The read-only preflight refuses the same configuration, so a self-config
    // preview reports it before anyone asks to publish.
    let plan = DesiredStateApplyPlan::new(vec![obligation_surface(
        owner,
        "outcomes",
        "ObligationOutcome",
        "flag",
    )])?;
    let previewed = access
        .transact("test.obligation.preview", |txn| {
            let plan = &plan;
            Box::pin(async move { validate_desired_state_plan(txn, plan).await })
        })
        .await
        .err()
        .expect("preflight refuses the same obligation");
    assert!(
        format!("{previewed:#}").contains("expected_count_field"),
        "{previewed:#}"
    );

    assert!(
        crate::list_datastore_tool_surfaces(&node, owner)
            .await?
            .is_empty(),
        "a refused obligation publishes no surface"
    );
    node.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn output_obligation_accepts_every_count_field_the_runtime_can_parse() -> Result<()> {
    let node = obligation_node().await?;
    let access = ConfigAccess::Local(node.clone());
    let owner = "did:key:obligation-owner";

    let mut refused = Vec::new();
    for (surface_id, count_field) in [
        ("int-count", "total"),
        ("string-count", "expected_total"),
        ("float32-count", "ratio"),
        ("float64-count", "amount"),
        ("json-count", "payload"),
        ("datetime-count", "observed_at"),
        ("blob-count", "attachment"),
        ("id-count", "marker"),
        ("non-null-int-count", "required_total"),
        ("non-null-string-count", "required_label"),
    ] {
        if let Err(error) = apply(
            &access,
            vec![obligation_surface(
                owner,
                surface_id,
                "ObligationOutcome",
                count_field,
            )],
        )
        .await
        {
            refused.push(format!("{count_field}: {error:#}"));
        }
    }
    assert!(refused.is_empty(), "{refused:#?}");
    // A collection that does not exist yet cannot refute the obligation.
    apply(
        &access,
        vec![obligation_surface(
            owner,
            "unregistered",
            "ObligationOutcomeLater",
            "total",
        )],
    )
    .await?;

    let surfaces = crate::list_datastore_tool_surfaces(&node, owner).await?;
    assert_eq!(surfaces.len(), 11, "{surfaces:?}");
    node.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn string_count_field_from_the_reported_failure_is_still_accepted() -> Result<()> {
    let node = obligation_node().await?;
    let access = ConfigAccess::Local(node.clone());
    let owner = "did:key:obligation-owner";

    // A String field's reported type cannot separate a canonical decimal
    // count from prose, so a String count field is accepted.
    apply(
        &access,
        vec![obligation_surface(
            owner,
            "reported",
            "ObligationOutcome",
            "result",
        )],
    )
    .await?;

    let surfaces = crate::list_datastore_tool_surfaces(&node, owner).await?;
    assert_eq!(surfaces.len(), 1, "{surfaces:?}");
    node.shutdown().await;
    Ok(())
}
