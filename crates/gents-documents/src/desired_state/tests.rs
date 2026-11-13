use super::plan::{stale_expectation, DesiredStateApplyDocument, DesiredStateExpectation};
use super::projection::{config_projection, desired_state_document_digest};
use super::read::{
    count_desired_state_read_page_queries, expect_existing_documents_unchanged,
    read_desired_state_document_in_txn, read_record, read_records, read_records_with_page_size,
};
use super::validate::{
    validate_desired_state_plan, validate_event_trigger_document_fields, OutcomeSourceSchemaError,
};
use super::*;
use crate::apply_desired_state_plan;
use crate::test_support::desired_state_fixtures::{
    apply, apply_with_counts, backend, config, document, register_config_schemas,
    register_config_schemas_dropping_unique_indexes, runtime_access,
};
use anyhow::Context;
use defra_node::Node as EmbeddedNode;
use gents_store::config_client::schema_introspection::SchemaField;
use gents_store::config_client::ConfigAccess;
use serde_json::json;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tracing_subscriber::layer::{Context as LayerContext, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

/// Fixture identities. Node and Agent documents are keyed by DIDs, which the
/// identity crate validates on creation, so fixtures use real `did:key` values.
const CODING_AGENT_DID: &str = "did:key:z6MkwUZdxNwTrfGMmUBJxWHoG7KjwfKHSimZMXYhJHCYtRAU";
const REVIEW_AGENT_DID: &str = "did:key:z6Mksr2NdS5Pt3RRf6azhx52KG5XCbijRezUGhoc9TVd9PQU";
const DEFAULT_REPLACEMENT_OWNER_DIDS: [&str; 6] = [
    "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
    "did:key:z6MkfXG2FkNy3u7Eg3jm8e2YQpGz7Z1JqWgHDAP1hLk9r2bR",
    "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK",
    "did:key:z6MkfU66Fr2mkFNav8t5hQVbTHbbFL8hkPhoaoHegSNThyRh",
    "did:key:z6MkeXBLjYiSvqnhFb6D7sHm8yKm4jV45wwBFRaatf1cfZ76",
    "did:key:z6MknPW5yoqtbf72nEAqce5Rb6ubgcZVwFweMvPdQbYjNLv6",
];

#[test]
fn modeled_removal_requires_the_exact_returned_document() {
    let cases = &gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .expect("configuration contract")
        .configuration_removal_cases;
    assert!(!cases.is_empty());
    for case in cases {
        let target = case["target"].as_str().unwrap();
        let rows: Vec<_> = case["returned"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| json!({"_docID": id}))
            .collect();
        let response = json!({"data": {"delete_Skill": rows}});
        assert_eq!(
            confirm_removal(&response, "Skill", target).is_ok(),
            case["expected"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
    }
    assert!(confirm_removal(&json!({"data": {"delete_Skill": null}}), "Skill", "doc").is_err());
}

#[tokio::test]
async fn modeled_callback_configuration_publication_and_revocation() -> Result<()> {
    use gents_lean_vocab::{lean_identity_household_cases, LeanIdentitySubject};
    use gents_store::identity::NodeIdentity as _;
    let case = lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let modeled_user = &case
        .objects
        .iter()
        .find(|object| object.id == "home")
        .unwrap()
        .owner;
    let modeled_agent = &case
        .objects
        .iter()
        .find(|object| object.id == "agent-created")
        .unwrap()
        .owner;
    let modeled_runtime = case
        .relations
        .iter()
        .find_map(|relation| match &relation.subject {
            LeanIdentitySubject::Actor { actor }
                if relation.resource == "nodes" && relation.relation == "runtime_reader" =>
            {
                Some(actor)
            }
            _ => None,
        })
        .unwrap();
    // Modeled DIDs carry no keys. Each modeled actor is played by a held
    // identity so every write is signed by its real actor.
    let keys = tempfile::tempdir()?;
    let held = |name: &str| {
        gents_store::identity::KeyIdentity::load_or_create(keys.path().join(name), None)
    };
    let (user_key, runtime_key, agent_key) = (held("user")?, held("runtime")?, held("agent")?);
    let mut actors = BTreeMap::new();
    for observation in &case.callback_configuration_cases {
        let modeled = observation["actor"].as_str().unwrap().to_owned();
        if !actors.contains_key(&modeled) {
            let real = if modeled == *modeled_runtime {
                runtime_key.did().to_owned()
            } else if modeled == *modeled_agent {
                agent_key.did().to_owned()
            } else {
                held(&format!("actor-{}", actors.len()))?.did().to_owned()
            };
            actors.insert(modeled, real);
        }
    }
    let (user, agent, runtime) = (user_key.did(), agent_key.did(), runtime_key.did());
    assert_ne!(modeled_user, modeled_agent);
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    // The Node is the runtime's identity; the user is the configuration actor.
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(runtime)
            .with_node_acp_owner(user.to_owned())
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    let owner = ConfigAccess::Graphql(
        gents_store::config_client::GraphqlEndpoint::as_principal(
            format!("http://{address}/api/v0/graphql"),
            user,
        )
        .with_client_signing_capability(user_key.client_signing_capability()?)?,
    );
    let context = defra_core::RequestContext::default().with_identity(Some(user.to_owned()));
    node.add_dac_policy_with_counter_as(&context, &case.policy_yaml, 0)
        .await?;
    register_config_schemas(&node).await?;
    for actor in actors.values() {
        for permission in [
            acp::nac::NodePermission::DocumentRead,
            acp::nac::NodePermission::CollectionGet,
        ] {
            node.add_nac_actor_relationship_as(&context, permission.as_str(), actor)
                .await?;
        }
    }
    node.add_collection_as(&context, gents_protocol::schemas::EVENT_SOURCE_CURSOR)
        .await?;
    node.add_collection_as(&context, "type CallbackTestEvent { label: String }")
        .await?;
    let created = owner
        .write(
            "test.callback.node",
            &format!(
                "mutation {{ create_Node(input: {{node_did: \"{}\", enabled: true}}) {{_docID}} }}",
                escape_graphql_string(runtime)
            ),
        )
        .await?;
    let node_doc = gents_store::graphql::created_doc_id(&created, "Node")?;
    owner
        .add_actor_relationship(
            "Node",
            &node_doc,
            "runtime_reader",
            &identity::Did::new(runtime)?,
        )
        .await?;
    owner.write("test.callback.dependencies", &format!(
        "mutation {{ create_Agent(input: {{node_did: \"{runtime}\", agent_did: \"{agent}\", creation_nonce: \"nonce\", inference_profile_id: \"profile\"}}) {{_docID}} create_EventSource(input: {{node_did: \"{runtime}\", event_source_id: \"source\", source_collection: \"CallbackTestEvent\", event_kind: \"created\"}}) {{_docID}} }}",
        runtime = escape_graphql_string(runtime), agent = escape_graphql_string(agent),
    )).await?;
    let docs = [
        (Collection::Callback, json!({"node_did":runtime,"callback_id":"callback","handler":{"kind":"built_in","emitter":"create_workspace"}})),
        (Collection::CallbackModule, json!({"node_did":runtime,"module_id":"module"})),
        (Collection::CallbackBinding, json!({"node_did":runtime,"binding_id":"binding","agent_did":agent,"callback_id":"callback","event_source_id":"source"})),
    ].into_iter().map(|(collection, value)| DesiredStateApplyDocument {
        collection, add: value.clone(), update: value,
    }).collect::<Vec<_>>();
    apply(&owner, docs.clone()).await?;
    let binding = owner.execute("{ CallbackBinding { _docID } }").await?;
    let binding_doc = binding["data"]["CallbackBinding"][0]["_docID"]
        .as_str()
        .unwrap();
    let mut revoked = false;
    for observation in &case.callback_configuration_cases {
        if observation["revoked"] == true && !revoked {
            owner
                .delete_actor_relationship(
                    "CallbackBinding",
                    binding_doc,
                    "reader",
                    &identity::Did::new(agent)?,
                )
                .await?;
            apply(&owner, docs.clone()).await?;
            revoked = true;
        }
        let actor = &actors[observation["actor"].as_str().unwrap()];
        let id = observation["id"].as_str().unwrap();
        let (collection, field) = match id {
            "binding" => ("CallbackBinding", "binding_id"),
            "callback" => ("Callback", "callback_id"),
            "module" => ("CallbackModule", "module_id"),
            _ => unreachable!(),
        };
        let reader = ConfigAccess::local_as(node.clone(), identity::Did::new(actor)?);
        let response = reader
            .execute(&format!(
                "{{ {collection}(filter: {{{field}: {{_eq: \"{}\"}}}}) {{_docID}} }}",
                escape_graphql_string(id)
            ))
            .await?;
        assert_eq!(
            !response["data"][collection].as_array().unwrap().is_empty(),
            observation["expected"].as_bool().unwrap(),
            "{} {id}",
            observation["actor"]
        );
    }
    node.shutdown().await?;
    Ok(())
}

#[derive(Clone, Default)]
struct IgnoredWireWarningCapture(Arc<Mutex<Vec<tracing::Level>>>);

impl<S> Layer<S> for IgnoredWireWarningCapture
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _: LayerContext<'_, S>) {
        if event.metadata().target() == "gents_loop::openai_wire" {
            self.0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(*event.metadata().level());
        }
    }
}

/// An out-of-band row for an InferenceBackend identity. Backends are stored as
/// `InferenceBackendMetadata` (their credential lives apart), so an injected
/// duplicate or drift targets that physical collection.
fn backend_metadata_mutation(owner: &str, id: &str, name: &str) -> String {
    format!(
        r#"mutation {{ create_InferenceBackendMetadata(input: {{node_did: "{}", backend_id: "{}", name: "{}", provider_kind: "OpenAiCompatible", endpoint: "http://127.0.0.1:8000/v1", auth_kind: "unauthenticated", enabled: true}}) {{ _docID }} }}"#,
        escape_graphql_string(owner),
        escape_graphql_string(id),
        escape_graphql_string(name),
    )
}

#[tokio::test]
async fn ignored_wire_api_warns_once_per_changed_backend_through_common_apply_owner() -> Result<()>
{
    let node = Arc::new(gents_store::test_support::node_builder().build().await?);
    register_config_schemas(&node).await?;
    let access = runtime_access(&node)?;
    let capture = IgnoredWireWarningCapture::default();
    let warnings = Arc::clone(&capture.0);
    let subscriber = tracing::Dispatch::new(Registry::default().with(capture));
    let _subscriber_guard = tracing::dispatcher::set_default(&subscriber);

    let mut backend = backend(
        "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
        "ignored-wire",
    );
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
    node.shutdown().await.expect("node shutdown");
    Ok(())
}

#[tokio::test]
async fn unchanged_desired_state_replay_does_not_add_document_commit() -> Result<()> {
    let node = Arc::new(gents_store::test_support::node_builder().build().await?);
    register_config_schemas(&node).await?;
    let access = runtime_access(&node)?;
    let owner = "did:key:z6MknPW5yoqtbf72nEAqce5Rb6ubgcZVwFweMvPdQbYjNLv6";
    let backend_id = "unchanged-backend";
    let document = document(backend(owner, backend_id));

    let created = apply_with_counts(&access, vec![document.clone()]).await?;
    assert_eq!(created.get(Collection::InferenceBackend), 1);
    let query = format!(
        r#"{{ InferenceBackendMetadata(filter: {{ node_did: {{ _eq: "{}" }}, backend_id: {{ _eq: "{}" }} }}, limit: 1) {{ _docID }} }}"#,
        gents_store::graphql::escape_graphql_string(owner),
        gents_store::graphql::escape_graphql_string(backend_id),
    );
    let response = access.execute(&query).await?;
    let doc_id = response["data"]["InferenceBackendMetadata"][0]["_docID"]
        .as_str()
        .map(ToOwned::to_owned)
        .context("created backend is missing _docID")?;
    let before = gents_store::graphql::composite_commits(
        node.as_ref(),
        &doc_id,
        "test.desired_state_replay.before",
    )
    .await?;
    assert!(!before.is_empty(), "create must publish a document commit");

    let replayed = apply_with_counts(&access, vec![document]).await?;
    assert_eq!(
        replayed.get(Collection::InferenceBackend),
        1,
        "counts continue to report each planned document"
    );
    let after = gents_store::graphql::composite_commits(
        node.as_ref(),
        &doc_id,
        "test.desired_state_replay.after",
    )
    .await?;
    assert_eq!(
        after, before,
        "an identical desired-state replay must not add a whole-document commit"
    );

    node.shutdown().await.expect("node shutdown");
    Ok(())
}

#[test]
fn commitments_do_not_reinterpret_authored_strings_as_json() {
    assert_ne!(
        desired_state_document_digest(&json!({"value":["{\"a\":1}"]})).unwrap(),
        desired_state_document_digest(&json!({"value":[{"a":1}]})).unwrap()
    );
}

#[test]
fn plan_admission_matches_lean() {
    use gents_lean_vocab::families::desired_state_plan_admission as plan_family;
    let owner = |index: u64| DEFAULT_REPLACEMENT_OWNER_DIDS[usize::try_from(index).unwrap()];
    gents_lean_vocab::families::desired_state_plan_admission::replay(|plan: &plan_family::Plan| {
        DesiredStateApplyPlan::new(
            plan.documents
                .iter()
                .map(|planned| {
                    let mut value =
                        backend(owner(planned.owner), &format!("backend-{}", planned.id));
                    if planned.undeclared_field {
                        value["probe_status"] = "healthy".into();
                    }
                    let mut doc = document(value);
                    if planned.sparse_replacement {
                        doc.update = json!({"enabled": false});
                    }
                    if let Some(replacement_owner) = planned.replacement_owner {
                        doc.update["node_did"] = owner(replacement_owner).into();
                    }
                    doc
                })
                .collect(),
        )
        .is_ok()
    });
}

#[tokio::test]
async fn replacement_resets_defaults_and_preserves_backend_observations() -> Result<()> {
    let node = Arc::new(gents_store::test_support::node_builder().build().await?);
    register_config_schemas(&node).await?;
    let access = runtime_access(&node)?;
    let mut initial = backend(
        "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
        "local",
    );
    initial["max_concurrent"] = 7.into();
    initial["enabled"] = false.into();
    initial["tags"] = json!(["old"]);
    apply(
        &access,
        vec![
            document(initial),
            document(backend(
                "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK",
                "local",
            )),
        ],
    )
    .await?;
    access.write("test.desired.observe", r#"mutation { update_InferenceBackend(filter:{node_did:{_eq:"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7"},backend_id:{_eq:"local"}},input:{probe_status:"healthy",catalogs:{entries:[{node_did:null,observed_at:"2026-01-01T00:00:00Z",models:null}]}}){_docID} }"#).await?;
    let before = node
        .execute("{InferenceBackend{_docID node_did catalogs probe_status}}")
        .await;
    assert!(!before.has_errors());
    let before = before.data.unwrap();
    apply(
        &access,
        vec![document(backend(
            "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
            "local",
        ))],
    )
    .await?;
    let after = node
        .execute(
            "{InferenceBackend{_docID node_did max_concurrent enabled tags catalogs probe_status}}",
        )
        .await;
    assert!(!after.has_errors());
    let after = after.data.unwrap();
    for row in after["InferenceBackend"].as_array().unwrap() {
        let prior = before["InferenceBackend"]
            .as_array()
            .unwrap()
            .iter()
            .find(|prior| prior["node_did"] == row["node_did"])
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
    let node = Arc::new(gents_store::test_support::node_builder().build().await?);
    register_config_schemas_dropping_unique_indexes(&node, &[Collection::InferenceBackend]).await?;
    let access = runtime_access(&node)?;
    apply(
        &access,
        vec![document(backend(
            "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
            "z_duplicate",
        ))],
    )
    .await?;
    access
        .write(
            "test.desired.duplicate",
            &backend_metadata_mutation(
                "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
                "z_duplicate",
                "Distinct document",
            ),
        )
        .await?;
    assert!(apply(
        &access,
        vec![
            document(backend(
                "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
                "a_staged"
            )),
            document(backend(
                "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",
                "z_duplicate"
            ))
        ]
    )
    .await
    .is_err());
    let rows = access
        .execute("{InferenceBackendMetadata{backend_id name}}")
        .await?;
    let rows = rows["data"]["InferenceBackendMetadata"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row["backend_id"] == "z_duplicate"));
    assert!(rows.iter().any(|row| row["name"] == "Distinct document"));
    Ok(())
}

#[tokio::test]
async fn read_records_pages_across_batches_and_omits_missing_ids() -> Result<()> {
    let node = Arc::new(gents_store::test_support::node_builder().build().await?);
    register_config_schemas(&node).await?;
    let access = runtime_access(&node)?;
    let owner = "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7";
    let ids = ["a", "b", "c", "d", "e"];
    for id in ids {
        apply(&access, vec![document(backend(owner, id))]).await?;
    }
    let records = access
        .transact_write("test.read_records.paged", |txn| {
            Box::pin(async move {
                read_records_with_page_size(
                    txn,
                    Collection::InferenceBackend,
                    owner,
                    &["a", "b", "c", "d", "e", "missing"],
                    2,
                )
                .await
            })
        })
        .await?;
    assert_eq!(records.len(), ids.len(), "{records:?}");
    assert!(!records.contains_key("missing"));
    for id in ids {
        let (doc_id, value) = records.get(id).expect("present");
        assert_eq!(value["backend_id"], id);
        assert!(!doc_id.is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn desired_state_apply_batches_identity_reads_by_collection_and_owner() -> Result<()> {
    let node = Arc::new(gents_store::test_support::node_builder().build().await?);
    register_config_schemas(&node).await?;
    let access = runtime_access(&node)?;
    let owner = "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7";
    let docs = ["backend-a", "backend-b", "backend-c"]
        .into_iter()
        .map(|id| document(backend(owner, id)))
        .collect::<Vec<_>>();
    apply(&access, docs.clone()).await?;
    let expected_backend_writes = docs.len();

    let plan = DesiredStateApplyPlan::new(docs)?;
    let expected = plan
        .documents()
        .iter()
        .map(|doc| {
            let (owner, id) = document_identity(doc.collection, &doc.add)?;
            Ok(DesiredStateExpectation {
                collection: doc.collection,
                owner: owner.to_owned(),
                id: id.to_owned(),
                digest: Some(desired_state_document_digest(&doc.add)?),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let plan = plan.with_expected(expected)?;
    let (result, read_queries) = count_desired_state_read_page_queries(access.transact_write(
        "test.desired.apply.batched_reads",
        |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(txn, plan, &crate::test_support::AdmitAllProfiles).await
            })
        },
    ))
    .await;
    let counts = result?;

    assert_eq!(
        counts.get(Collection::InferenceBackend),
        expected_backend_writes
    );
    assert_eq!(
        read_queries, 2,
        "one page reads all expectations and one reads all planned documents"
    );
    node.shutdown().await.expect("node shutdown");
    Ok(())
}

#[tokio::test]
async fn read_records_rejects_duplicate_live_documents_naming_them() -> Result<()> {
    let node = Arc::new(gents_store::test_support::node_builder().build().await?);
    register_config_schemas_dropping_unique_indexes(&node, &[Collection::InferenceBackend]).await?;
    let access = runtime_access(&node)?;
    let owner = "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7";
    apply(&access, vec![document(backend(owner, "dup"))]).await?;
    access
        .write(
            "test.read_records.duplicate",
            &backend_metadata_mutation(owner, "dup", "Second"),
        )
        .await?;
    let error = access
        .transact_write("test.read_records.duplicate.read", |txn| {
            Box::pin(async move {
                read_records(txn, Collection::InferenceBackend, owner, &["dup"]).await
            })
        })
        .await
        .unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("multiple live InferenceBackendMetadata documents"),
        "{message}"
    );
    assert!(message.contains("dup"), "{message}");
    Ok(())
}

/// Agent DIDs are globally unique (`Agent.agent_did` is a unique index), so each
/// owner's cycle names its own Agent.
fn cyclic_configuration(owner: &str, agent_did: &str) -> Vec<DesiredStateApplyDocument> {
    vec![
        document(backend(owner, "backend")),
        config(
            Collection::InferenceProfile,
            json!({"node_did":owner,"profile_id":"profile","backend_id":"backend","model_name":"model"}),
        ),
        config(
            Collection::Agent,
            json!({"node_did":owner,"agent_did":agent_did,"creation_nonce":"nonce","inference_profile_id":"profile","context_id":"context"}),
        ),
        config(
            Collection::AgentContext,
            json!({"node_did":owner,"context_id":"context","tools_id":"tools"}),
        ),
        config(
            Collection::Tools,
            json!({"node_did":owner,"tools_id":"tools","agents":{"target_ids":["target"]}}),
        ),
        config(
            Collection::AgentTarget,
            json!({"node_did":owner,"target_id":"target","target_node_did":owner,"agent_did":agent_did,"name":"worker"}),
        ),
    ]
}

#[tokio::test]
async fn generated_default_replacements_preserve_runtime_startup_selection() -> Result<()> {
    use gents_lean_vocab::families::configuration_scope_default_replacement as family;

    let cases = family::cases();
    // The Lean cases name agents by label; Agent and Node identities are DIDs.
    let agent_did = |label: &Option<String>| -> Value {
        match label.as_deref() {
            None => Value::Null,
            Some("coding") => json!(CODING_AGENT_DID),
            Some("review") => json!(REVIEW_AGENT_DID),
            Some(other) => panic!("unmodeled agent label {other}"),
        }
    };
    for (index, case) in cases.iter().enumerate() {
        // Agent DIDs are globally unique, so each case gets its own store.
        let node = Arc::new(gents_store::test_support::node_builder().build().await?);
        register_config_schemas(&node).await?;
        let access = runtime_access(&node)?;
        let owner = DEFAULT_REPLACEMENT_OWNER_DIDS[index];
        let mut documents = vec![
            document(backend(owner, "backend")),
            config(
                Collection::InferenceProfile,
                json!({"node_did":owner,"profile_id":"profile","backend_id":"backend","model_name":"model"}),
            ),
            config(
                Collection::Node,
                json!({"node_did":owner,"display_name":"Original"}),
            ),
            config(
                Collection::DefaultAgentSelection,
                json!({"node_did":owner,"default_agent_did":agent_did(&case.input.current)}),
            ),
        ];
        for agent in [CODING_AGENT_DID, REVIEW_AGENT_DID] {
            documents.push(config(
                Collection::Agent,
                json!({"node_did":owner,"agent_did":agent,"creation_nonce":"nonce","inference_profile_id":"profile"}),
            ));
        }
        apply(&access, documents).await?;
        let mut requested = json!({
            "node": {"node_did":owner,"display_name":"Changed"},
            "default_agent_selection": {"node_did":owner}
        });
        if case.input.candidate.is_some() {
            requested["default_agent_selection"]["default_agent_did"] =
                agent_did(&case.input.candidate);
        }
        let pack: crate::PackConfig = serde_json::from_value(requested)?;
        let plan = DesiredStateApplyPlan::from_pack_config(&pack)?;
        let (before_node, before_selection) = read_principal_and_default(&access, owner).await?;
        let preview = access
            .transact_write("test.default.preview", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    validate_desired_state_plan(txn, plan, &crate::test_support::AdmitAllProfiles)
                        .await
                })
            })
            .await;
        let published = access
            .transact_write("test.default.publish", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan, &crate::test_support::AdmitAllProfiles)
                        .await
                })
            })
            .await;
        let allowed = published.is_ok();
        assert_eq!(preview.is_ok(), allowed, "case {index}: {preview:?}");
        case.check(allowed);
        let (after_node, after_selection) = read_principal_and_default(&access, owner).await?;
        if allowed {
            let (_, after_node) = after_node.expect("published principal");
            assert_eq!(after_node["display_name"], "Changed");
            let (_, after_selection) = after_selection.expect("published default selection");
            assert_eq!(
                after_selection["default_agent_did"],
                agent_did(&case.input.candidate)
            );
        } else {
            assert_eq!(
                after_node, before_node,
                "refused replacement changed the principal"
            );
            assert_eq!(
                after_selection, before_selection,
                "refused replacement changed the default selection"
            );
            let message = format!("{:#}", published.unwrap_err());
            assert!(
                message.contains("DefaultAgentSelection replacement would clear default_agent_did"),
                "{message}"
            );
            let current = agent_did(&case.input.current);
            assert!(
                message.contains(current.as_str().expect("refused default")),
                "{message}"
            );
        }
        node.shutdown().await.expect("node shutdown");
    }
    Ok(())
}

async fn read_principal_and_default(
    access: &ConfigAccess,
    owner: &str,
) -> Result<(Option<(String, Value)>, Option<(String, Value)>)> {
    access
        .transact_write("test.default.read", |txn| {
            let owner = &owner;
            Box::pin(async move {
                let node = read_record(txn, Collection::Node, owner, owner).await?;
                let selection =
                    read_record(txn, Collection::DefaultAgentSelection, owner, owner).await?;
                Ok((node, selection))
            })
        })
        .await
}

#[tokio::test]
async fn retained_inbound_references_and_cycles_share_atomic_publication() -> Result<()> {
    let node = Arc::new(gents_store::test_support::node_builder().build().await?);
    register_config_schemas(&node).await?;
    let access = runtime_access(&node)?;
    let owner = "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7";
    let mut documents = cyclic_configuration(owner, CODING_AGENT_DID);
    documents.extend(cyclic_configuration(
        "did:key:z6MkfXG2FkNy3u7Eg3jm8e2YQpGz7Z1JqWgHDAP1hLk9r2bR",
        REVIEW_AGENT_DID,
    ));
    let seeded = apply_with_counts(&access, documents).await?;
    for collection in cyclic_configuration(owner, CODING_AGENT_DID)
        .iter()
        .map(|doc| doc.collection)
    {
        assert_eq!(seeded.get(collection), 2, "{collection:?}");
    }
    access.write("test.observe", r#"mutation { update_InferenceBackend(filter:{node_did:{_eq:"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7"}},input:{probe_status:"healthy"}){_docID} }"#).await?;
    let before = node.execute("{InferenceBackend{_docID node_did name probe_status} AgentContext{_docID node_did context_id}}").await.data;

    let mut drifted_replacement = cyclic_configuration(owner, CODING_AGENT_DID)
        .into_iter()
        .find(|doc| doc.collection == Collection::Agent)
        .unwrap();
    drifted_replacement.update["context_id"] = "absent".into();
    let preview_plan = DesiredStateApplyPlan::new(vec![drifted_replacement])?;
    assert!(access
        .transact_write("test.preview.replacement", |txn| {
            let plan = &preview_plan;
            Box::pin(async move {
                validate_desired_state_plan(txn, plan, &crate::test_support::AdmitAllProfiles).await
            })
        })
        .await
        .is_err());
    let preview_removal = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
        Collection::AgentContext,
        owner.into(),
        "context".into(),
    )])?;
    assert!(access
        .transact_write("test.preview.removal", |txn| {
            let plan = &preview_removal;
            Box::pin(async move {
                validate_desired_state_plan(txn, plan, &crate::test_support::AdmitAllProfiles).await
            })
        })
        .await
        .is_err());
    let preview_addition =
        DesiredStateApplyPlan::new(vec![document(backend(owner, "preview-only"))])?;
    access
        .transact_write("test.preview.addition", |txn| {
            let plan = &preview_addition;
            Box::pin(async move {
                validate_desired_state_plan(txn, plan, &crate::test_support::AdmitAllProfiles).await
            })
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

    let mut edited = backend(owner, "backend");
    edited["name"] = "must roll back".into();
    let plan = DesiredStateApplyPlan::new(vec![document(edited)])?.with_removals(vec![(
        Collection::AgentContext,
        owner.into(),
        "context".into(),
    )])?;
    let error = access
        .transact_write("test.reject.removal", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(txn, plan, &crate::test_support::AdmitAllProfiles).await
            })
        })
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("references missing AgentContext"));
    let after = node.execute("{InferenceBackend{_docID node_did name probe_status} AgentContext{_docID node_did context_id}}").await.data;
    assert_eq!(
        before, after,
        "failed publication preserves desired fields and observations"
    );

    let removals = cyclic_configuration(owner, CODING_AGENT_DID)
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
        .transact_write("test.remove.cycle", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(txn, plan, &crate::test_support::AdmitAllProfiles).await
            })
        })
        .await?;
    let remaining = access
        .execute("{Agent{node_did} InferenceBackendMetadata{node_did}}")
        .await?;
    for collection in ["Agent", "InferenceBackendMetadata"] {
        let rows = remaining["data"][collection].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["node_did"],
            "did:key:z6MkfXG2FkNy3u7Eg3jm8e2YQpGz7Z1JqWgHDAP1hLk9r2bR"
        );
    }
    Ok(())
}

#[tokio::test]
async fn replacement_checks_actual_update_and_unchanged_duplicate_rows() -> Result<()> {
    let node = Arc::new(gents_store::test_support::node_builder().build().await?);
    register_config_schemas_dropping_unique_indexes(&node, &[Collection::AgentContext]).await?;
    let access = runtime_access(&node)?;
    let owner = "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7";
    apply(&access, cyclic_configuration(owner, CODING_AGENT_DID)).await?;
    let mut replacement = cyclic_configuration(owner, CODING_AGENT_DID)
        .into_iter()
        .find(|doc| doc.collection == Collection::Agent)
        .unwrap();
    replacement.update["context_id"] = "absent".into();
    assert!(apply(&access, vec![replacement]).await.is_err());
    let row = node.execute("{Agent{context_id}}").await;
    assert_eq!(row.data.unwrap()["Agent"][0]["context_id"], "context");

    access.write("test.duplicate.context", r#"mutation { create_AgentContext(input:{node_did:"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7",context_id:"context",description:"duplicate"}){_docID} }"#).await?;
    let error = apply(
        &access,
        vec![document(backend(owner, "unrelated-new-backend"))],
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("multiple live AgentContext"));
    let rows = access
        .execute("{InferenceBackendMetadata{backend_id}}")
        .await?;
    assert_eq!(
        rows["data"]["InferenceBackendMetadata"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn expect_existing_documents_unchanged_rejects_drifted_live_rows() -> Result<()> {
    let node = Arc::new(gents_store::test_support::node_builder().build().await?);
    register_config_schemas(&node).await?;
    let access = runtime_access(&node)?;
    let owner = "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7";
    let packaged = ["packaged", "packaged-two", "packaged-three"]
        .into_iter()
        .map(|id| document(backend(owner, id)))
        .collect::<Vec<_>>();
    apply(&access, packaged.clone()).await?;

    let matching = DesiredStateApplyPlan::new(packaged)?;
    let (matching_result, read_queries) = count_desired_state_read_page_queries(
        access.transact_write("test.verify.matching", |txn| {
            let plan = &matching;
            Box::pin(async move { expect_existing_documents_unchanged(txn, plan).await })
        }),
    )
    .await;
    matching_result?;
    assert_eq!(
        read_queries, 2,
        "capture and validation each read all three identities in one page"
    );

    access.write("test.drift", r#"mutation { update_InferenceBackendMetadata(filter:{node_did:{_eq:"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7"},backend_id:{_eq:"packaged"}},input:{name:"out-of-band"}){_docID} }"#).await?;
    let error = access
        .transact_write("test.verify.drifted", |txn| {
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

fn expectation(owner: &str, id: &str, digest: Option<String>) -> DesiredStateExpectation {
    DesiredStateExpectation {
        collection: Collection::InferenceBackend,
        owner: owner.to_owned(),
        id: id.to_owned(),
        digest,
    }
}

const CAS_OWNER_DID: &str = "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7";

async fn cas_node() -> Result<(ConfigAccess, &'static str)> {
    let node = Arc::new(gents_store::test_support::node_builder().build().await?);
    register_config_schemas(&node).await?;
    Ok((runtime_access(&node)?, CAS_OWNER_DID))
}

/// The Lean publish-if scenarios name their owner `did:owner`, which is not a DID.
/// Map it to the fixture node so the Gents owner validates real identities.
fn modeled_owner(lean_owner: &str) -> &'static str {
    assert_eq!(lean_owner, "did:owner", "unmodeled Lean owner");
    CAS_OWNER_DID
}

async fn live_digest(access: &ConfigAccess, owner: &str, id: &str) -> Result<Option<String>> {
    let (owner, id) = (owner.to_owned(), id.to_owned());
    access
        .transact_write("test.cas.read", |txn| {
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
    apply(&access, vec![document(renamed(owner, "other", "Edited"))]).await?;
    let before = live_digest(&access, owner, "target").await?;

    let plan = DesiredStateApplyPlan::new(vec![document(renamed(owner, "target", "Promoted"))])?
        .with_expected(vec![
            expectation(owner, "target", frozen_target),
            expectation(owner, "other", frozen_other.clone()),
        ])?;
    let error = access
        .transact_write("test.cas.stale", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(txn, plan, &crate::test_support::AdmitAllProfiles).await
            })
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
        .transact_write("test.cas.absent_ok", |txn| {
            let plan = &must_be_absent;
            Box::pin(async move {
                apply_desired_state_plan(txn, plan, &crate::test_support::AdmitAllProfiles).await
            })
        })
        .await?;

    let wrongly_absent = DesiredStateApplyPlan::new(vec![document(backend(owner, "again"))])?
        .with_expected(vec![expectation(owner, "present", None)])?;
    let error = access
        .transact_write("test.cas.absent_stale", |txn| {
            let plan = &wrongly_absent;
            Box::pin(async move {
                apply_desired_state_plan(txn, plan, &crate::test_support::AdmitAllProfiles).await
            })
        })
        .await
        .unwrap_err();
    assert!(stale_expectation(&error).is_some());

    let wrongly_present =
        DesiredStateApplyPlan::new(vec![document(backend(owner, "third"))])?.with_expected(
            vec![expectation(owner, "missing", Some("sha256:00".into()))],
        )?;
    let error = access
        .transact_write("test.cas.present_stale", |txn| {
            let plan = &wrongly_present;
            Box::pin(async move {
                apply_desired_state_plan(txn, plan, &crate::test_support::AdmitAllProfiles).await
            })
        })
        .await
        .unwrap_err();
    let stale = stale_expectation(&error).expect("typed StaleExpectation");
    assert_eq!(stale.drifted[0].found, None);
    Ok(())
}

#[tokio::test]
async fn guarded_publication_matches_lean_publish_if_cases() -> Result<()> {
    use gents_lean_vocab::families::apply_reconcile_contract_cases_publish_if_cases as family;

    type DocRef = family::DocRef<String>;

    fn doc_for(doc: &DocRef, content: &str) -> Value {
        renamed(modeled_owner(&doc.node_did), &doc.id, content)
    }

    fn plan_of(rows: &[family::Desired<String>]) -> Result<DesiredStateApplyPlan> {
        DesiredStateApplyPlan::new(
            rows.iter()
                .map(|row| document(doc_for(&row.doc, &row.content)))
                .collect(),
        )
    }

    async fn publish(access: &ConfigAccess, plan: &DesiredStateApplyPlan) -> bool {
        let outcome = access
            .transact_write("test.cas.lean", |txn| {
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan, &crate::test_support::AdmitAllProfiles)
                        .await
                })
            })
            .await;
        if let Err(error) = &outcome {
            assert!(stale_expectation(error).is_some(), "{error:#}");
        }
        outcome.is_ok()
    }

    async fn observe(
        access: &ConfigAccess,
        docs: &[DocRef],
    ) -> Result<Vec<family::Content<String>>> {
        let mut observed = Vec::new();
        for doc in docs {
            let (owner, id) = (modeled_owner(&doc.node_did), doc.id.clone());
            let content = access
                .transact_write("test.cas.read", |txn| {
                    let id = id.clone();
                    Box::pin(async move {
                        anyhow::Ok(
                            read_desired_state_document_in_txn(
                                txn,
                                Collection::InferenceBackend,
                                owner,
                                &id,
                            )
                            .await?
                            .map(|live| live["name"].as_str().expect("backend name").to_owned()),
                        )
                    })
                })
                .await?;
            observed.push(family::Content {
                doc: doc.clone(),
                content,
            });
        }
        Ok(observed)
    }

    for case in family::cases::<String>() {
        let input = &case.input;
        let (access, _) = cas_node().await?;
        apply(
            &access,
            input
                .pre_desired
                .iter()
                .map(|row| document(doc_for(&row.doc, &row.content)))
                .collect(),
        )
        .await?;

        let expected = input
            .expected
            .iter()
            .map(|row| {
                let digest = row
                    .fields
                    .as_ref()
                    .map(|fields| {
                        let (_, projected) = config_projection(
                            Collection::InferenceBackend,
                            Some(&doc_for(&row.doc, &fields.content)),
                        )?;
                        desired_state_document_digest(&projected.context("projection")?)
                    })
                    .transpose()?;
                Ok(expectation(
                    modeled_owner(&row.doc.node_did),
                    &row.doc.id,
                    digest,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let plan = plan_of(&input.candidate)?.with_expected(expected)?;

        let applied = publish(&access, &plan).await;
        let after = observe(&access, &input.observe).await?;
        let replay_applied = publish(&access, &plan).await;
        let after_replay = observe(&access, &input.observe).await?;
        case.check(family::PublishIfOutput {
            applied,
            after,
            replay_applied,
            after_replay,
        });
    }
    Ok(())
}

fn rule_documents(
    prompt_template: &str,
    goal_objective_template: Option<&str>,
    emit_outcome: bool,
    group: Option<Value>,
    source_collection: &str,
) -> Result<(crate::Trigger, crate::Task, crate::EventSource)> {
    let owner = "did:key:template-rule";
    let mut source = json!({
        "node_did": owner, "event_source_id": "watcher",
        "source_collection": source_collection, "event_kind": "created",
    });
    if let Some(group) = group {
        source["group"] = group;
        source["correlation_field"] = json!("message");
    }
    let mut task = json!({
        "node_did": owner, "task_id": "work", "agent_did": "did:key:z6MkwUZdxNwTrfGMmUBJxWHoG7KjwfKHSimZMXYhJHCYtRAU",
        "prompt_template": prompt_template, "emit_outcome": emit_outcome,
    });
    if let Some(goal_objective_template) = goal_objective_template {
        task["goal_objective_template"] = json!(goal_objective_template);
    }
    Ok((
        serde_json::from_value(json!({
            "node_did": owner, "trigger_id": "on-doc", "task_id": "work",
            "source": {"kind": "event", "event_source_id": "watcher"},
        }))?,
        serde_json::from_value(task)?,
        serde_json::from_value(source)?,
    ))
}

fn obligation_surface(
    owner: &str,
    surface_id: &str,
    collection: &str,
    count_field: &str,
) -> DesiredStateApplyDocument {
    let surface = json!({
        "surface_id": surface_id,
        "node_did": owner,
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
    let surface: crate::DatastoreToolSurfaceDocument =
        serde_json::from_value(surface).expect("canonical surface");
    let value = serde_json::to_value(surface).expect("canonical surface value");
    DesiredStateApplyDocument {
        collection: Collection::DatastoreToolSurface,
        add: value.clone(),
        update: value,
    }
}

async fn obligation_node() -> Result<Arc<EmbeddedNode>> {
    let node = Arc::new(gents_store::test_support::node_builder().build().await?);
    gents_store::schema::ensure_runtime_schemas(&node).await?;
    node.add_collection(
        "type ObligationOutcome { text: String result: String expected_total: String total: Int ratio: Float32 amount: Float64 payload: JSON observed_at: DateTime required_observed_at: DateTime! attachment: Blob marker: ID flag: Boolean tags: [String] required_total: Int! required_label: String! required_tags: [String!] }",
    )
    .await?;
    Ok(node)
}

#[tokio::test]
async fn output_obligation_count_field_must_hold_a_count_on_the_target_collection() -> Result<()> {
    let node = obligation_node().await?;
    let access = runtime_access(&node)?;
    let owner = "did:key:obligation-owner";

    for case in gents_lean_vocab::families::count_carrier_obligation_field::cases() {
        let reported_type = case.input.schema.as_str();
        let count_fields: &[&str] = match reported_type {
            "Boolean" => &["flag"],
            "LIST" => &["tags", "required_tags"],
            "DateTime" => &["observed_at"],
            "DateTime!" => &["required_observed_at"],
            "ID" => &["marker"],
            other => panic!("the obligation fixture declares no {other} field"),
        };
        let mut refusals = Vec::new();
        for count_field in count_fields {
            let outcome = apply(
                &access,
                vec![obligation_surface(
                    owner,
                    "outcomes",
                    "ObligationOutcome",
                    count_field,
                )],
            )
            .await;
            refusals.push(outcome.is_err_and(|refused| {
                let diagnostic = format!("{refused:#}");
                diagnostic.contains("expected_count_field") && diagnostic.contains(reported_type)
            }));
        }
        assert!(
            refusals.windows(2).all(|pair| pair[0] == pair[1]),
            "{reported_type}: {refusals:?}"
        );
        case.check(refusals[0]);
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
    .expect_err("the count field must exist on the target collection");
    assert!(
        format!("{absent_field:#}").contains("does not exist on ObligationOutcome"),
        "{absent_field:#}"
    );

    let plan = DesiredStateApplyPlan::new(vec![obligation_surface(
        owner,
        "outcomes",
        "ObligationOutcome",
        "flag",
    )])?;
    let previewed = access
        .transact_write("test.obligation.preview", |txn| {
            let plan = &plan;
            Box::pin(async move {
                validate_desired_state_plan(txn, plan, &crate::test_support::AdmitAllProfiles).await
            })
        })
        .await
        .expect_err("preflight refuses the same obligation");
    assert!(
        format!("{previewed:#}").contains("expected_count_field"),
        "{previewed:#}"
    );

    assert!(
        crate::list_datastore_tool_surfaces(&ConfigAccess::local(node.clone()), owner)
            .await?
            .is_empty(),
        "a refused obligation publishes no surface"
    );
    node.shutdown().await.expect("node shutdown");
    Ok(())
}

async fn event_source_node() -> Result<Arc<EmbeddedNode>> {
    let node = Arc::new(gents_store::test_support::node_builder().build().await?);
    register_config_schemas(&node).await?;
    node.add_collection(
        "type EventProbeSource { batch: String required_batch: String! result: String flag: Boolean tags: [String] total: Int expected_total: Int! ratio: Float32 amount: Float64 payload: JSON observed_at: DateTime required_observed_at: DateTime! attachment: Blob marker: ID }",
    )
    .await?;
    Ok(node)
}

fn event_source_document(
    owner: &str,
    id: &str,
    collection: &str,
    correlation_field: Option<&str>,
    count_field: Option<&str>,
) -> DesiredStateApplyDocument {
    let mut source = json!({
        "node_did": owner,
        "event_source_id": id,
        "source_collection": collection,
    });
    if let Some(field) = correlation_field {
        source["correlation_field"] = json!(field);
    }
    if let Some(field) = count_field {
        source["group"] = json!({"expected_count": {"source_field": field}});
    }
    DesiredStateApplyDocument {
        collection: Collection::EventSource,
        add: source.clone(),
        update: source,
    }
}

fn grouped_event_source(
    owner: &str,
    id: &str,
    collection: &str,
    count_field: &str,
) -> DesiredStateApplyDocument {
    event_source_document(owner, id, collection, Some("batch"), Some(count_field))
}

fn correlated_event_source(
    owner: &str,
    id: &str,
    collection: &str,
    correlation_field: &str,
) -> DesiredStateApplyDocument {
    event_source_document(owner, id, collection, Some(correlation_field), None)
}

async fn published_event_sources(node: &EmbeddedNode) -> Result<Vec<Value>> {
    let rows = node
        .execute("{ EventSource { event_source_id source_collection correlation_field group } }")
        .await;
    assert!(!rows.has_errors());
    Ok(rows.data.expect("EventSource rows")["EventSource"]
        .as_array()
        .expect("EventSource array")
        .clone())
}

#[tokio::test]
async fn event_source_count_field_must_hold_a_count_on_the_source_collection() -> Result<()> {
    let node = event_source_node().await?;
    let access = runtime_access(&node)?;
    let owner = "did:key:event-source-owner";

    for case in gents_lean_vocab::families::count_carrier_event_source_field::cases() {
        let reported_type = case.input.schema.as_str();
        let count_field = match reported_type {
            "Boolean" => "flag",
            "LIST" => "tags",
            "DateTime" => "observed_at",
            "DateTime!" => "required_observed_at",
            "ID" => "marker",
            other => panic!("the event source fixture declares no {other} field"),
        };
        let outcome = apply(
            &access,
            vec![grouped_event_source(
                owner,
                "grouped",
                "EventProbeSource",
                count_field,
            )],
        )
        .await;
        case.check(outcome.is_err_and(|refused| {
            let diagnostic = format!("{refused:#}");
            diagnostic.contains("expected_count_field") && diagnostic.contains(reported_type)
        }));
    }

    let absent_field = apply(
        &access,
        vec![grouped_event_source(
            owner,
            "grouped",
            "EventProbeSource",
            "missing_total",
        )],
    )
    .await
    .expect_err("the count field must exist on the source collection");
    assert!(
        format!("{absent_field:#}").contains("does not exist on EventProbeSource"),
        "{absent_field:#}"
    );

    let plan = DesiredStateApplyPlan::new(vec![grouped_event_source(
        owner,
        "grouped",
        "EventProbeSource",
        "flag",
    )])?;
    let previewed = access
        .transact_write("test.event_source.preview", |txn| {
            let plan = &plan;
            Box::pin(async move {
                validate_desired_state_plan(txn, plan, &crate::test_support::AdmitAllProfiles).await
            })
        })
        .await
        .expect_err("preflight refuses the same event source");
    assert!(
        format!("{previewed:#}").contains("expected_count_field"),
        "{previewed:#}"
    );

    assert!(
        published_event_sources(&node).await?.is_empty(),
        "a refused count field publishes no event source"
    );
    node.shutdown().await.expect("node shutdown");
    Ok(())
}

#[tokio::test]
async fn event_source_correlation_field_must_be_a_string() -> Result<()> {
    let node = event_source_node().await?;
    let access = runtime_access(&node)?;
    let owner = "did:key:event-source-owner";

    let mistyped = apply(
        &access,
        vec![correlated_event_source(
            owner,
            "correlated",
            "EventProbeSource",
            "expected_total",
        )],
    )
    .await
    .expect_err("a non-String correlation can never correlate");
    let diagnostic = format!("{mistyped:#}");
    assert!(
        diagnostic.contains("correlation_field \"expected_total\" must be String, found Int"),
        "{diagnostic}"
    );

    let absent = apply(
        &access,
        vec![correlated_event_source(
            owner,
            "correlated",
            "EventProbeSource",
            "missing",
        )],
    )
    .await
    .expect_err("the correlation field must exist on the source collection");
    let diagnostic = format!("{absent:#}");
    assert!(
        diagnostic.contains("correlation_field \"missing\" does not exist on EventProbeSource"),
        "{diagnostic}"
    );

    assert!(
        published_event_sources(&node).await?.is_empty(),
        "a refused correlation field publishes no event source"
    );
    node.shutdown().await.expect("node shutdown");
    Ok(())
}

#[tokio::test]
async fn schema_obligation_publication_matches_lean() -> Result<()> {
    use gents_lean_vocab::families::schema_obligations_publication as family;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Document {
        Surface,
        EventSource,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Scalar {
        Int,
        Float32,
        Float64,
        String,
        Id,
        Blob,
        Json,
        DateTime,
        Boolean,
        StringList,
    }

    fn sdl(name: &str, schema: &[family::FieldDecl<Scalar>]) -> String {
        let fields: Vec<String> = schema
            .iter()
            .map(|declaration| {
                let scalar = match declaration.r#type.scalar {
                    Scalar::Int => "Int",
                    Scalar::Float32 => "Float32",
                    Scalar::Float64 => "Float64",
                    Scalar::String => "String",
                    Scalar::Id => "ID",
                    Scalar::Blob => "Blob",
                    Scalar::Json => "JSON",
                    Scalar::DateTime => "DateTime",
                    Scalar::Boolean => "Boolean",
                    Scalar::StringList => "[String]",
                };
                let required = if declaration.r#type.required { "!" } else { "" };
                format!("{}: {scalar}{required}", declaration.name)
            })
            .collect();
        format!("type {name} {{ {} }}", fields.join(" "))
    }

    fn rendered(
        document: Document,
        owner: &str,
        id: &str,
        collection: &str,
        payload: &family::Payload,
    ) -> DesiredStateApplyDocument {
        match document {
            Document::Surface => obligation_surface(
                owner,
                id,
                collection,
                payload.count.as_deref().expect("a surface names a count"),
            ),
            Document::EventSource => event_source_document(
                owner,
                id,
                collection,
                payload.correlation.as_deref(),
                payload.count.as_deref(),
            ),
        }
    }

    async fn live(
        node: &Arc<EmbeddedNode>,
        document: Document,
        owner: &str,
        id: &str,
        probe: &str,
    ) -> Result<Option<family::Payload>> {
        match document {
            Document::Surface => {
                let Some(surface) =
                    crate::list_datastore_tool_surfaces(&ConfigAccess::local(node.clone()), owner)
                        .await?
                        .into_iter()
                        .find(|surface| surface.surface_id == id)
                else {
                    return Ok(None);
                };
                let Some(crate::SurfaceToolDecl::Create(decl)) =
                    surface.entries.unwrap_or_default().into_iter().next()
                else {
                    panic!("surface publishes its create tool");
                };
                Ok(Some(family::Payload {
                    registered: decl.collection == probe,
                    count: decl
                        .output_obligation
                        .and_then(|obligation| obligation.expected_count_field),
                    correlation: None,
                }))
            }
            Document::EventSource => Ok(published_event_sources(node)
                .await?
                .into_iter()
                .find(|source| source["event_source_id"] == json!(id))
                .map(|source| family::Payload {
                    registered: source["source_collection"] == json!(probe),
                    count: source["group"]["expected_count"]["source_field"]
                        .as_str()
                        .map(str::to_owned),
                    correlation: source["correlation_field"].as_str().map(str::to_owned),
                })),
        }
    }

    let node = Arc::new(gents_store::test_support::node_builder().build().await?);
    register_config_schemas(&node).await?;
    let access = runtime_access(&node)?;
    let owner = "did:key:obligation-owner";
    let mut probes: BTreeMap<String, String> = BTreeMap::new();

    for (index, case) in family::cases::<Document, Scalar>().into_iter().enumerate() {
        let input = &case.input;
        let shape = serde_json::to_string(&input.schema)?;
        if !probes.contains_key(&shape) {
            let name = format!("ObligationProbe{}", probes.len());
            node.add_collection(&sdl(&name, &input.schema)).await?;
            probes.insert(shape.clone(), name);
        }
        let probe = probes[&shape].as_str();
        let unregistered = format!("{probe}Later");
        let id = format!("case-{index}");
        let named = |payload: &family::Payload| {
            let collection = if payload.registered {
                probe
            } else {
                unregistered.as_str()
            };
            rendered(input.document, owner, &id, collection, payload)
        };

        let mut steps = Vec::new();
        for step in &input.steps {
            let document = DesiredStateApplyDocument {
                collection: match input.document {
                    Document::Surface => Collection::DatastoreToolSurface,
                    Document::EventSource => Collection::EventSource,
                },
                add: named(&step.create).add,
                update: named(&step.replacement).update,
            };
            crate::test_support::desired_state_fixtures::ensure_owner_nodes(
                &access,
                std::slice::from_ref(&document),
            )
            .await?;
            let plan = DesiredStateApplyPlan::new(vec![document.clone()])?;
            let preflight = access
                .transact_write("test.obligation.preflight", |txn| {
                    let plan = &plan;
                    Box::pin(async move {
                        validate_desired_state_plan(
                            txn,
                            plan,
                            &crate::test_support::AdmitAllProfiles,
                        )
                        .await
                    })
                })
                .await
                .is_ok();
            let accepted = apply(&access, vec![document]).await.is_ok();
            steps.push(family::StepOutput {
                preflight,
                accepted,
                live: live(&node, input.document, owner, &id, probe).await?,
            });
        }
        case.check(family::Output { steps });
    }
    node.shutdown().await.expect("node shutdown");
    Ok(())
}

#[tokio::test]
async fn outcome_delivery_requires_a_string_handoff_id_on_its_source_collection() -> Result<()> {
    let (builder, runtime) = gents_store::test_support::keyed_node_builder();
    let node = Arc::new(builder.with_node_acp_enabled().build().await?);
    gents_store::schema::ensure_runtime_schemas(&node).await?;
    node.add_collection(
        "type PlainPing { message: String } type HandoffPing { handoff_id: String message: String } type NumberedPing { handoff_id: Int }",
    )
    .await?;
    let access = ConfigAccess::local_runtime(node.clone(), runtime.clone())?;
    let owner = gents_store::identity::NodeIdentity::did(runtime.as_ref());
    crate::test_support::bootstrap_signed_test_node(&access, owner).await;
    let agent = gents_store::identity::provision_agent_identity(runtime.as_ref())?;
    let mut configuration = cyclic_configuration(owner, &agent.agent_did);
    for document in configuration
        .iter_mut()
        .filter(|document| document.collection == Collection::Agent)
    {
        document.add["creation_nonce"] = json!(agent.creation_nonce);
        document.update["creation_nonce"] = json!(agent.creation_nonce);
    }
    apply(&access, configuration).await?;
    let source = |collection: &str| {
        config(
            Collection::EventSource,
            json!({"node_did":owner,"event_source_id":"watcher","source_collection":collection,"event_kind":"created"}),
        )
    };
    let task = |emit_outcome: bool| {
        config(
            Collection::Task,
            json!({"node_did":owner,"task_id":"handle","agent_did":agent.agent_did,"prompt_template":"Handle {{ doc.message }}","emit_outcome":emit_outcome}),
        )
    };
    let trigger = config(
        Collection::Trigger,
        json!({"node_did":owner,"trigger_id":"on-ping","task_id":"handle","source":{"kind":"event","event_source_id":"watcher"},"concurrency":"parallel"}),
    );
    let triggers = || async {
        let rows = node.execute("{ Trigger { trigger_id } }").await;
        assert!(!rows.has_errors());
        rows.data.unwrap()["Trigger"].as_array().unwrap().len()
    };

    let refused = apply(
        &access,
        vec![source("PlainPing"), task(true), trigger.clone()],
    )
    .await
    .expect_err("a collection without handoff_id can never carry an outcome");
    let diagnostic = format!("{refused:#}");
    let schema = refused.downcast_ref::<OutcomeSourceSchemaError>().unwrap();
    assert_eq!(schema.collection, "PlainPing");
    assert!(schema.missing_handoff);
    assert!(
        diagnostic.contains("Trigger on-ping delivers PlainPing to Task handle")
            && diagnostic.contains("has no handoff_id field"),
        "{diagnostic}"
    );
    let mistyped = apply(
        &access,
        vec![source("NumberedPing"), task(true), trigger.clone()],
    )
    .await
    .expect_err("a non-String handoff_id is not a handoff identity");
    let schema = mistyped.downcast_ref::<OutcomeSourceSchemaError>().unwrap();
    assert_eq!(schema.collection, "NumberedPing");
    assert!(!schema.missing_handoff);
    assert!(
        format!("{mistyped:#}").contains("NumberedPing.handoff_id is Int, not String"),
        "{mistyped:#}"
    );
    assert_eq!(triggers().await, 0, "a refused delivery publishes nothing");

    apply(
        &access,
        vec![source("PlainPing"), task(false), trigger.clone()],
    )
    .await?;
    let opted_in = apply(&access, vec![task(true)])
        .await
        .expect_err("opting the retained delivery's Task in is the same refusal");
    assert!(
        format!("{opted_in:#}").contains("has no handoff_id field"),
        "{opted_in:#}"
    );
    apply(&access, vec![source("HandoffPing"), task(true)]).await?;
    let rerouted = apply(&access, vec![source("PlainPing")])
        .await
        .expect_err("rerouting the retained opted-in delivery is the same refusal");
    assert!(
        format!("{rerouted:#}").contains("has no handoff_id field"),
        "{rerouted:#}"
    );
    apply(&access, vec![source("LaterPing")]).await?;
    assert_eq!(triggers().await, 1);
    node.shutdown().await.expect("node shutdown");
    Ok(())
}

fn schema_fields(names: &[&str]) -> BTreeMap<String, SchemaField> {
    names
        .iter()
        .map(|name| {
            (
                (*name).to_owned(),
                SchemaField {
                    name: (*name).to_owned(),
                    type_name: "String".to_owned(),
                },
            )
        })
        .collect()
}

#[test]
fn template_doc_fields_follow_the_declared_schema_vocabulary() -> Result<()> {
    let declared = schema_fields(&["message", "reply_session_id", "_internal"]);
    for prompt in [
        "Work {{ doc.message }}",
        "Work {{ doc.reply_session_id }}",
        "Work {{ doc._internal }}",
        "Work {{ doc.attempt | default('none') }}",
    ] {
        let (trigger, task, source) = rule_documents(prompt, None, false, None, "GapInput")?;
        validate_event_trigger_document_fields(&trigger, &task, &source, &declared)
            .unwrap_or_else(|error| panic!("{prompt} resolves against the schema: {error:#}"));
    }
    let (trigger, task, source) =
        rule_documents("Work {{ doc.not_a_field }}", None, false, None, "GapInput")?;
    let message = format!(
        "{:#}",
        validate_event_trigger_document_fields(&trigger, &task, &source, &declared).unwrap_err()
    );
    assert!(
        message.contains("trigger on-doc prompt_template references doc.not_a_field")
            && message.contains("GapInput has no field \"not_a_field\"")
            && message.contains("its fields are message, reply_session_id"),
        "{message}"
    );
    let (trigger, task, source) =
        rule_documents("Work {{ doc.COUNT }}", None, false, None, "GapInput")?;
    let message = format!(
        "{:#}",
        validate_event_trigger_document_fields(&trigger, &task, &source, &declared).unwrap_err()
    );
    assert!(
        message.contains("prompt_template references doc.COUNT"),
        "{message}"
    );
    Ok(())
}

#[test]
fn every_fire_template_is_judged_with_its_field_named() -> Result<()> {
    let (trigger, task, source) = rule_documents("Work", None, false, None, "GapInput")?;
    let mut session = trigger.clone();
    session.session_id_template = Some("Resume {{ doc.session }}".to_owned());
    let message = format!(
        "{:#}",
        validate_event_trigger_document_fields(
            &session,
            &task,
            &source,
            &schema_fields(&["message"])
        )
        .unwrap_err()
    );
    assert!(
        message.contains("session_id_template references doc.session"),
        "{message}"
    );
    let (trigger, task, source) = rule_documents(
        "Work",
        Some("Reach {{ doc.objective }}"),
        false,
        None,
        "GapInput",
    )?;
    let message = format!(
        "{:#}",
        validate_event_trigger_document_fields(
            &trigger,
            &task,
            &source,
            &schema_fields(&["message"])
        )
        .unwrap_err()
    );
    assert!(
        message.contains("goal_objective_template references doc.objective"),
        "{message}"
    );
    Ok(())
}

#[tokio::test]
async fn publication_applies_native_route_template_fields_the_schema_does_not_declare() -> Result<()>
{
    let node = Arc::new(gents_store::test_support::node_builder().build().await?);
    gents_store::schema::ensure_runtime_schemas(&node).await?;
    let access = runtime_access(&node)?;
    let owner = "did:key:native-route-owner";
    apply(&access, cyclic_configuration(owner, CODING_AGENT_DID)).await?;
    let documents = vec![
        config(
            Collection::EventSource,
            json!({"node_did":owner,"event_source_id":"watcher","source_collection":"WorkspaceReceipt","event_kind":"created"}),
        ),
        config(
            Collection::Task,
            json!({"node_did":owner,"task_id":"review","agent_did":"did:key:z6MkwUZdxNwTrfGMmUBJxWHoG7KjwfKHSimZMXYhJHCYtRAU","prompt_template":"Review {{ doc.work_unit_id }} after attempt {{ doc.attempt }}","goal_objective_template":"Seal {{ doc.attempt }}","emit_outcome":true}),
        ),
        config(
            Collection::Trigger,
            json!({"node_did":owner,"trigger_id":"on-receipt","task_id":"review","source":{"kind":"event","event_source_id":"watcher"},"session_id_template":"{{ doc.reply_session_id }}","concurrency":"parallel"}),
        ),
    ];
    let plan = DesiredStateApplyPlan::new(documents.clone())?;
    access
        .transact_write("test.native_route.preview", |txn| {
            let plan = &plan;
            Box::pin(async move {
                validate_desired_state_plan(txn, plan, &crate::test_support::AdmitAllProfiles).await
            })
        })
        .await?;
    apply(&access, documents).await?;
    let rows = node.execute("{ Trigger { trigger_id } }").await;
    assert!(!rows.has_errors());
    assert_eq!(
        rows.data.unwrap()["Trigger"].as_array().unwrap().len(),
        1,
        "an admitted trigger publishes"
    );
    node.shutdown().await.expect("node shutdown");
    Ok(())
}

#[tokio::test]
async fn modeled_node_binding_uses_runtime_visibility_without_changing_agent_actor() -> Result<()> {
    use gents_lean_vocab::{
        lean_identity_household_cases, LeanIdentityAction, LeanIdentitySubject,
    };
    let case = lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .expect("configuration contract");
    let modeled_agent = &case
        .objects
        .iter()
        .find(|object| object.id == "agent-created")
        .unwrap()
        .owner;
    let _modeled_runtime = case
        .relations
        .iter()
        .find_map(|relation| match &relation.subject {
            LeanIdentitySubject::Actor { actor }
                if relation.resource == "nodes" && relation.relation == "runtime_reader" =>
            {
                Some(actor)
            }
            _ => None,
        })
        .expect("modeled runtime");
    assert!(case.actions.iter().any(
        |action| matches!(action, LeanIdentityAction::Check { actor, .. } if actor == modeled_agent)
    ));
    // Modeled DIDs carry no keys; held identities play the modeled User,
    // runtime and Agent so the User's configuration writes are signed.
    use gents_store::identity::NodeIdentity as _;
    let keys = tempfile::tempdir()?;
    let held = |name: &str| {
        gents_store::identity::KeyIdentity::load_or_create(keys.path().join(name), None)
    };
    let (user_key, runtime_key, agent_key) = (held("user")?, held("runtime")?, held("agent")?);
    let (user, runtime, agent) = (
        user_key.did().to_owned(),
        runtime_key.did(),
        agent_key.did(),
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let node = Arc::new(
        gents_store::test_support::node_builder()
            .enable_signing()
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    let user_access = ConfigAccess::Graphql(
        gents_store::config_client::GraphqlEndpoint::as_principal(
            format!("http://{address}/api/v0/graphql"),
            &user,
        )
        .with_client_signing_capability(user_key.client_signing_capability()?)?,
    );
    let runtime_did = identity::Did::new(runtime)?;
    let agent_access = ConfigAccess::local_as(node.clone(), identity::Did::new(agent)?);
    let ctx = defra_core::RequestContext::default().with_identity(Some(user.clone()));
    node.add_dac_policy_with_counter_as(&ctx, &case.policy_yaml, 0)
        .await?;
    node.add_collection_as(&ctx, gents_protocol::schemas::NODE)
        .await?;
    let response = user_access
        .write(
            "test.configuration.node_binding",
            &format!(
                "mutation {{ create_Node(input: {{node_did: \"{}\"}}) {{_docID}} }}",
                escape_graphql_string(runtime),
            ),
        )
        .await?;
    let doc_id = gents_store::graphql::created_doc_id(&response, "Node")?;
    for observation in &case.configuration_node_binding_cases {
        let visible = observation["visible"].as_bool().unwrap();
        if visible {
            user_access
                .add_actor_relationship("Node", &doc_id, "runtime_reader", &runtime_did)
                .await?;
        } else {
            user_access
                .delete_actor_relationship("Node", &doc_id, "runtime_reader", &runtime_did)
                .await?;
        }
        let binding = ConfigurationNodeBinding {
            enabled: true,
            default_agent_did: None,
            node_did: if observation["same_node"] == true {
                runtime_did.clone()
            } else {
                identity::Did::new(agent)?
            },
            doc_id: if observation["has_doc"] == true {
                doc_id.clone()
            } else {
                String::new()
            },
        };
        let result = agent_access
            .transact_readonly("test.configuration.node_binding_observe", |txn| {
                let binding = &binding;
                Box::pin(async move {
                    let hidden = txn.execute("{Node {_docID}}").await?;
                    anyhow::ensure!(
                        hidden["data"]["Node"].as_array().unwrap().is_empty(),
                        "agent unexpectedly reads Node"
                    );
                    anyhow::ensure!(txn.actor_did() == Some(agent), "transaction actor changed");
                    configuration_node_doc(txn, runtime, Some(binding)).await
                })
            })
            .await;
        assert_eq!(
            result.is_ok(),
            observation["expected"].as_bool().unwrap(),
            "modeled binding: {result:?}"
        );
    }
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn projection_binding_publication_follows_modeled_native_ownership() -> Result<()> {
    use gents_lean_vocab::lean_identity_household_cases;
    use gents_store::config_client::{GraphqlEndpoint, NodePermission};
    use gents_store::identity::NodeIdentity;

    let model = lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .context("missing configuration model")?;
    let dir = tempfile::tempdir()?;
    let runtime =
        gents_store::identity::KeyIdentity::load_or_create(dir.path().join("runtime"), None)?;
    let user = gents_store::identity::KeyIdentity::load_or_create(dir.path().join("user"), None)?;
    let creator =
        gents_store::identity::KeyIdentity::load_or_create(dir.path().join("creator"), None)?;
    let agent = gents_store::identity::KeyIdentity::load_or_create(dir.path().join("agent"), None)?;
    let sibling =
        gents_store::identity::KeyIdentity::load_or_create(dir.path().join("sibling"), None)?;
    let outsider =
        gents_store::identity::KeyIdentity::load_or_create(dir.path().join("outsider"), None)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(runtime.did())
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .with_storage_backend(defra_node::StorageBackend::Regolith)
            .data_path(dir.path().join("database"))
            .build()
            .await?,
    );
    node.add_dac_policy_with_counter_as(
        &defra_core::RequestContext::default().with_identity(Some(runtime.did().to_owned())),
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
        0,
    )
    .await?;
    register_config_schemas(&node).await?;
    let runtime_access = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
    for actor in [&user, &creator, &agent, &sibling, &outsider] {
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
            NodePermission::DacRelationDelete,
            NodePermission::CollectionGet,
        ] {
            runtime_access
                .grant_node_permission(&identity::Did::new(actor.did())?, permission)
                .await?;
        }
    }
    let access = |actor: &gents_store::identity::KeyIdentity,
                  signed: bool|
     -> Result<ConfigAccess> {
        let endpoint =
            GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), actor.did())
                .with_client_signing_capability(actor.client_signing_capability()?)?;
        Ok(ConfigAccess::Graphql(if signed {
            endpoint.with_client_signed_agent_creates()
        } else {
            endpoint
        }))
    };
    let user_access = access(&user, false)?;
    let parent = user_access
        .write(
            "test.projection.node",
            &format!(
                "mutation {{ create_Node(input: {{node_did: \"{}\"}}) {{_docID}} }}",
                escape_graphql_string(runtime.did())
            ),
        )
        .await?;
    let parent = gents_protocol::graphql::extract_mutation_doc_id(&parent, "Node")?;
    for actor in [&runtime, &creator] {
        user_access
            .add_actor_relationship(
                "Node",
                &parent,
                "runtime_reader",
                &identity::Did::new(actor.did())?,
            )
            .await?;
    }
    let creator_access = access(&creator, true)?;
    let input = json!({"node_did":runtime.did(), "binding_id":"projection", "policy_id":"policy"});
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::ProjectionAcpBinding,
        add: input.clone(),
        update: input,
    }])?;
    creator_access
        .transact_write("test.projection.create", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(txn, plan, &crate::test_support::AdmitAllProfiles)
                    .await
                    .map(|_| ())
            })
        })
        .await?;
    let query = "{ ProjectionAcpBinding { _docID policy_id } }";
    let result = creator_access.execute(query).await?;
    let doc = result["data"]["ProjectionAcpBinding"][0]["_docID"]
        .as_str()
        .context("projection ID")?
        .to_owned();
    for revoked in [false, true] {
        if revoked {
            creator_access
                .delete_resource_relationship(
                    "ProjectionAcpBinding",
                    &doc,
                    "parent",
                    "nodes",
                    &parent,
                )
                .await?;
        }
        for case in model
            .projection_binding_cases
            .iter()
            .filter(|case| case["revoked"] == revoked)
        {
            let actor = match case["actor"].as_str().context("modeled actor")? {
                "user" => &user,
                "creator" => &creator,
                "runtime" => &runtime,
                "agent" => &agent,
                "sibling" => &sibling,
                "outsider" => &outsider,
                _ => unreachable!(),
            };
            let access = access(actor, true)?;
            let expected = case["expected"].as_bool().context("modeled expectation")?;
            let actual = match case["permission"].as_str().context("modeled permission")? {
                "read" => !gents_protocol::graphql::graphql_rows_from_response(
                    &access.execute(query).await?,
                    "ProjectionAcpBinding",
                )
                .is_empty(),
                "update" => {
                    let mutation = "mutation($id: ID!, $input: ProjectionAcpBindingMutationInputArg!) { update_ProjectionAcpBinding(docID: $id, input: $input) { _docID _version { cid } } }";
                    let value = format!("updated-{}-{revoked}", case["actor"].as_str().unwrap());
                    if !expected {
                        let before = creator_access.execute(query).await?;
                        let raw_access = ConfigAccess::Graphql(
                            GraphqlEndpoint::as_principal(
                                format!("http://{address}/api/v0/graphql"),
                                actor.did(),
                            )
                            .with_client_signing_capability(actor.client_signing_capability()?)?,
                        );
                        let response = raw_access.write("test.projection.denied", &format!(
                            "mutation {{ update_ProjectionAcpBinding(docID: \"{}\", input: {{policy_id: \"{}\"}}) {{_docID}} }}",
                            escape_graphql_string(&doc), escape_graphql_string(&value),
                        )).await;
                        let updated = match response {
                            Ok(value) => !gents_protocol::graphql::graphql_rows_from_response(
                                &value,
                                "update_ProjectionAcpBinding",
                            )
                            .is_empty(),
                            Err(error)
                                if error.is_permission_denied() || error.is_graphql_rejection() =>
                            {
                                false
                            }
                            Err(error) => return Err(error.into()),
                        };
                        assert_eq!(
                            creator_access.execute(query).await?,
                            before,
                            "denied mutation changed the document"
                        );
                        updated
                    } else {
                        let response = access
                            .transact_write("test.projection.update", |txn| {
                                let doc = &doc;
                                let value = &value;
                                Box::pin(async move {
                                    txn.execute_desired_state_mutation(
                                        mutation,
                                        &json!({"id": doc, "input": {"policy_id": value}}),
                                        Collection::ProjectionAcpBinding,
                                        false,
                                    )
                                    .await
                                })
                            })
                            .await;
                        !gents_protocol::graphql::graphql_rows_from_response(
                            &response?,
                            "update_ProjectionAcpBinding",
                        )
                        .is_empty()
                    }
                }
                _ => unreachable!(),
            };
            assert_eq!(actual, expected, "{case}");
        }
    }
    drop(creator_access);
    drop(user_access);
    drop(runtime_access);
    node.shutdown().await?;
    drop(node);
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let restarted = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(runtime.did())
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .with_storage_backend(defra_node::StorageBackend::Regolith)
            .data_path(dir.path().join("database"))
            .build()
            .await?,
    );
    for (actor, role) in [
        (&creator, "creator"),
        (&user, "user"),
        (&sibling, "sibling"),
    ] {
        let expected = model
            .projection_binding_cases
            .iter()
            .find(|case| {
                case["revoked"] == true && case["actor"] == role && case["permission"] == "read"
            })
            .and_then(|case| case["expected"].as_bool())
            .context("modeled restart expectation")?;
        let access = ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), actor.did())
                .with_client_signing_capability(actor.client_signing_capability()?)?,
        );
        let response = access.execute(query).await?;
        assert_eq!(
            !gents_protocol::graphql::graphql_rows_from_response(&response, "ProjectionAcpBinding")
                .is_empty(),
            expected
        );
    }
    restarted.shutdown().await?;
    Ok(())
}
