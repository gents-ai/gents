use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;

use gents_pack::pack::DriftPolicy;
use gents_store::config_client::ConfigAccess;

#[tokio::test]
async fn locally_installed_pack_agent_holds_the_projected_baseline() {
    use gents_store::identity::NodeIdentity as _;
    let (db, user, user_access) =
        crate::test_support::root::signed_test_node_without_agent("local-pack-baseline").await;
    let runtime = db.node_identity.clone();
    let (_guard, dir) = gents_pack::test_support::fixture_pack_copy("dependee_fixture", &json!({}));
    let (bytes, _) = gents_pack::pack::archive::pack_dir(&dir).unwrap();
    let archive = gents_pack::pack::archive::PackArchive::from_bytes(&bytes).unwrap();
    let local = ConfigAccess::local_as(db.node.clone(), identity::Did::new(user.did()).unwrap());
    let backend = json!({"node_did":runtime.did(),"backend_id":"backend","name":"Test","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:1/v1","auth":{"kind":"unauthenticated"}});
    let profile = json!({"node_did":runtime.did(),"profile_id":"profile","backend_id":"backend","model_name":"test-model"});
    let plan = gents_documents::DesiredStateApplyPlan::new(
        [
            (
                gents_store::collection::Collection::InferenceBackend,
                backend,
            ),
            (
                gents_store::collection::Collection::InferenceProfile,
                profile,
            ),
        ]
        .into_iter()
        .map(
            |(collection, value)| gents_documents::DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            },
        )
        .collect(),
    )
    .unwrap();
    user_access
        .transact_write("test.local_pack.profile", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
    let requested = BTreeMap::from([("worker".to_owned(), "profile".to_owned())]);
    let prepared = gents_pack::pack::prepare_document_pack_archive_with_runtime(
        &local,
        runtime.did(),
        &archive,
        &requested,
        &[],
        None,
        &|_| None,
        runtime.as_ref(),
        &Default::default(),
    )
    .await
    .unwrap();
    let agent = prepared
        .agent_bindings
        .values()
        .next()
        .expect("the pack provisions one Agent")
        .agent_did
        .clone();
    let agent = identity::Did::new(&agent).unwrap();
    let held = |node: Arc<defra_node::Node>, agent: identity::Did| async move {
        let mut held = Vec::new();
        for permission in gents_store::identity::GrantSet::AgentBaseline.permissions() {
            if node
                .nac_manager()
                .check_permission(&agent, *permission)
                .await
                .unwrap()
            {
                held.push(permission.as_str().to_owned());
            }
        }
        held
    };
    assert!(held(db.node.clone(), agent.clone()).await.is_empty());
    gents_pack::pack::install_prepared_document_graph(
        &user_access,
        runtime.did(),
        &archive,
        &prepared,
        DriftPolicy::Refuse,
        None,
        false,
        true,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
    .unwrap();
    let modeled = gents_lean_vocab::lean_contract_snapshot()
        .identity_agent_baseline_cases
        .iter()
        .find(|case| case.actor == "enrolled-user")
        .expect("modeled enrolled-user lifecycle")
        .steps
        .iter()
        .find(|step| step.action == "publish")
        .expect("modeled publication")
        .installed
        .clone();
    assert_eq!(held(db.node.clone(), agent).await, modeled);
    db.node.shutdown().await.unwrap();
}

/// Lean `Identity.PackPlacementReaders`: installing the pack applies exactly
/// the readers its RepositoryPlacement declares, an undeclared slot Agent stays
/// denied, a reinstall does not restore a revoked reader, and removal revokes
/// the declared readers and removes the placement.
#[tokio::test]
async fn pack_install_applies_exactly_the_declared_placement_readers() {
    use gents_store::identity::{GrantSet, NodeIdentity as _};
    let (db, user, user_access) =
        crate::test_support::root::signed_test_node_without_agent("pack-placement-readers").await;
    let runtime = db.node_identity.clone();
    let root = tempfile::tempdir().unwrap();
    let host_path = root.path().to_string_lossy().into_owned();
    let (_guard, dir) =
        gents_pack::test_support::fixture_pack_copy("placement_fixture", &json!({}));
    let (bytes, _) = gents_pack::pack::archive::pack_dir(&dir).unwrap();
    let archive = gents_pack::pack::archive::PackArchive::from_bytes(&bytes).unwrap();
    let local = ConfigAccess::local_as(db.node.clone(), identity::Did::new(user.did()).unwrap());
    let backend = json!({"node_did":runtime.did(),"backend_id":"backend","name":"Test","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:1/v1","auth":{"kind":"unauthenticated"}});
    let profile = json!({"node_did":runtime.did(),"profile_id":"profile","backend_id":"backend","model_name":"test-model"});
    let plan = gents_documents::DesiredStateApplyPlan::new(
        [
            (
                gents_store::collection::Collection::InferenceBackend,
                backend,
            ),
            (
                gents_store::collection::Collection::InferenceProfile,
                profile,
            ),
        ]
        .into_iter()
        .map(
            |(collection, value)| gents_documents::DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            },
        )
        .collect(),
    )
    .unwrap();
    user_access
        .transact_write("test.placement_readers.setup", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
    let requested = BTreeMap::from([("worker".to_owned(), "profile".to_owned())]);
    let environment =
        move |name: &str| (name == "PLACEMENT_FIXTURE_ROOT").then(|| host_path.clone());
    let install = || async {
        let prepared = gents_pack::pack::prepare_document_pack_archive_with_runtime(
            &local,
            runtime.did(),
            &archive,
            &requested,
            &[],
            None,
            &environment,
            runtime.as_ref(),
            &Default::default(),
        )
        .await
        .unwrap();
        gents_pack::pack::install_prepared_document_graph(
            &user_access,
            runtime.did(),
            &archive,
            &prepared,
            DriftPolicy::Refuse,
            None,
            false,
            true,
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await
        .unwrap();
        prepared.agent_bindings
    };
    let bindings = install().await;
    assert_eq!(bindings.len(), 3, "{bindings:?}");
    let admin = ConfigAccess::local_as(db.node.clone(), identity::Did::new(runtime.did()).unwrap());
    let other = gents_store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
    GrantSet::AgentBaseline
        .grant(&admin, &identity::Did::new(&other.agent_did).unwrap())
        .await
        .unwrap();
    let agent_access = |binding: &gents_protocol::agent_identity::AgentIdentityBinding| {
        let key = gents_store::identity::resolve_agent_identity(runtime.as_ref(), binding).unwrap();
        ConfigAccess::local_managed_agent(
            db.node.clone(),
            identity::Did::new(&binding.agent_did).unwrap(),
            key.as_ref(),
        )
        .unwrap()
    };
    let slot = |name: &str| &bindings[&format!("fixture-{name}")];
    let actors = BTreeMap::from([
        ("user", user_access.clone()),
        ("writer", agent_access(slot("writer"))),
        ("reviewer", agent_access(slot("reviewer"))),
        ("integrator", agent_access(slot("integrator"))),
        ("other", agent_access(&other)),
    ]);
    let readable = |actor: &'static str, access: ConfigAccess| async move {
        let rows = access
            .execute("{ RepositoryPlacement { repository_id } }")
            .await
            .unwrap();
        assert!(rows.get("errors").is_none(), "{actor}: {rows}");
        // A removed Agent's actor sees no collection at all: a native denial.
        rows["data"]["RepositoryPlacement"]
            .as_array()
            .is_some_and(|rows| {
                rows.iter()
                    .any(|row| row["repository_id"] == "placement-fixture-repository")
            })
    };
    let cases = &gents_lean_vocab::lean_contract_snapshot().pack_placement_reader_cases;
    assert_eq!(cases.len(), 3);
    for case in cases {
        match case.state.as_str() {
            "installed" => {}
            "reinstalled_after_revoke" => {
                let (doc, _) = user_access
                    .transact_readonly("test.placement_readers.read", |txn| {
                        let owner = runtime.did();
                        Box::pin(async move {
                            gents_documents::read_desired_state_record_in_txn(
                                txn,
                                gents_store::collection::Collection::RepositoryPlacement,
                                owner,
                                "placement-fixture-repository",
                            )
                            .await
                        })
                    })
                    .await
                    .unwrap()
                    .unwrap();
                user_access
                    .delete_actor_relationship(
                        "RepositoryPlacement",
                        &doc,
                        "reader",
                        &identity::Did::new(&slot("reviewer").agent_did).unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(install().await, bindings);
            }
            "removed" => {
                gents_pack::pack::remove_pack(
                    &user_access,
                    runtime.did(),
                    "fixture/placement_fixture",
                    DriftPolicy::Refuse,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
                .unwrap();
            }
            state => panic!("unmodeled placement reader state {state}"),
        }
        for expected in &case.readers {
            let actor = *actors
                .keys()
                .find(|known| **known == expected.actor.as_str())
                .unwrap();
            assert_eq!(
                readable(actor, actors[actor].clone()).await,
                expected.read,
                "{} {actor}",
                case.state
            );
        }
    }
    db.node.shutdown().await.unwrap();
}
