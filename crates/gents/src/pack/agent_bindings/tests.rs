use super::*;
use crate::lean_vocab_test::{lean_pack_identity_cases, LeanPackIdentityBinding};
use crate::store::identity::{KeyIdentity, NodeIdentity};
use serde_json::json;
use std::sync::Arc;

fn decode_bindings(rows: &[LeanPackIdentityBinding]) -> Result<PackAgentBindings> {
    let mut bindings = PackAgentBindings::new();
    for row in rows {
        anyhow::ensure!(
            bindings
                .insert(
                    row.slot.clone(),
                    AgentIdentityBinding {
                        creation_nonce: row.creation_nonce.clone(),
                        agent_did: row.agent_did.clone(),
                    }
                )
                .is_none(),
            "duplicate binding slot"
        );
    }
    Ok(bindings)
}

#[test]
fn binding_plan_matches_lean() {
    for case in lean_pack_identity_cases() {
        let actual = decode_bindings(&case.recorded).and_then(|recorded| {
            let provisioned = decode_bindings(&case.provisioned)?;
            plan_agent_bindings(&recorded, &case.requested, &provisioned)
        });
        assert_eq!(actual.is_ok(), case.accepted, "{}", case.name);
        if let Ok(actual) = actual {
            assert_eq!(
                actual.selected,
                decode_bindings(&case.selected).unwrap(),
                "{}",
                case.name
            );
            assert_eq!(
                actual.recorded,
                decode_bindings(&case.next_recorded).unwrap(),
                "{}",
                case.name
            );
        }
    }
}

fn manifest(name: &str) -> PackManifest {
    serde_json::from_value(json!({
        "manifest_version":1,"name":name,"namespace":"fixture","version":"1.0.0",
        "description":"Identity fixture","kind":"documents","authors":["Fixture"],
        "assets":["README.md","config.json"],"config":"config.json",
        "inference_slots":[{"name":"model","description":"Model","agents":["worker"]}]
    }))
    .unwrap()
}

fn config(label: &str) -> Value {
    json!({"node":{},"agents":[{
        "agent_slot":"worker","display_name":label,"inference_profile_id":"gents:inference-slot:model"
    }],"tasks":[{"task_id":"work","agent_did":"gents:agent-slot:worker","prompt_template":"Work"}],
    "agent_targets":[{"target_id":"worker-target","name":"Worker","target_node_did":"${GENTS_PACK_NODE_DID}","agent_did":"gents:agent-slot:worker"}]})
}

#[test]
fn offline_authoring_keeps_slots_out_of_canonical_agents() {
    let raw = config("Worker");
    let preview = load_pack_authoring_config(
        &manifest("one"),
        &PackInstallOptions {
            node_did: "did:test:node".into(),
        },
        &|_| Ok(serde_json::to_vec(&raw)?),
        &|_| None,
    )
    .unwrap();
    assert!(preview.documents.agents.is_empty());
    assert_eq!(preview.value["agents"][0]["agent_slot"], "worker");
    assert!(preview.value["agents"][0].get("agent_did").is_none());
    for invalid in [
        json!({"agent_slot":"worker","inference_profile_id":"gents:inference-slot:model","agent_did":"did:test:forged"}),
        json!({"agent_slot":"worker","inference_profile_id":"gents:inference-slot:model","unexpected":true}),
    ] {
        let mut value = raw.clone();
        value["agents"][0] = invalid;
        assert!(load_pack_authoring_config(
            &manifest("one"),
            &PackInstallOptions {
                node_did: "did:test:node".into()
            },
            &|_| Ok(serde_json::to_vec(&value)?),
            &|_| None
        )
        .is_err());
    }
}

async fn load(
    access: &ConfigAccess,
    node: &Arc<defra_node::Node>,
    root: &KeyIdentity,
    name: &str,
    label: &str,
) -> BoundPackConfig {
    let value = config(label);
    let actor = access.actor_did().unwrap();
    let local = ConfigAccess::local_as(node.clone(), identity::Did::new(&actor).unwrap());
    let provisioner = AgentProvisioner {
        node,
        runtime: root,
        actor: None,
        selected_agent_did: None,
        node_binding: None,
    };
    load_pack_install_config_with_provisioner(
        &local,
        &manifest(name),
        &PackInstallOptions {
            node_did: root.did().to_owned(),
        },
        &|_| Ok(serde_json::to_vec(&value)?),
        &|_| None,
        InstalledVia::Operator,
        Some(&provisioner),
    )
    .await
    .unwrap()
}

async fn install(
    access: &ConfigAccess,
    owner: &str,
    loaded: &BoundPackConfig,
) -> Result<super::super::InstallReport> {
    let mut identity =
        super::super::PackIdentity::new(&manifest("one"), "sha256:identity-fixture", vec![]);
    identity.installation_doc_id = loaded.installation_doc_id.clone();
    identity.agent_bindings = loaded.agent_bindings.clone();
    let bound_manifest = manifest_with_agent_bindings(&manifest("one"), &loaded.agent_bindings);
    let bound = super::super::bind_pack_install_config(
        &bound_manifest,
        &loaded.config,
        &BTreeMap::from([("model".into(), "profile".into())]),
    )?;
    super::super::install_pack_documents(
        access,
        owner,
        &identity,
        &bound,
        super::super::DriftPolicy::Refuse,
    )
    .await
}

pub(in crate::pack) async fn fixture() -> (
    tempfile::TempDir,
    KeyIdentity,
    Arc<defra_node::Node>,
    ConfigAccess,
) {
    let directory = tempfile::tempdir().unwrap();
    let root = KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap();
    let user = KeyIdentity::load_or_create(directory.path().join("user.key"), None).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let node = Arc::new(
        crate::test_support::node_builder()
            .with_node_identity_did(root.did())
            .with_node_acp_owner(root.did().to_owned())
            .with_http(defra_node::HttpConfig::with_listener(listener).unwrap())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let runtime_access =
        ConfigAccess::local_as(node.clone(), identity::Did::new(root.did()).unwrap());
    let user_did = identity::Did::new(user.did()).unwrap();
    crate::store::identity::GrantSet::OperatorUser
        .grant(&runtime_access, &user_did)
        .await
        .unwrap();
    let access = ConfigAccess::Graphql(
        crate::store::config_client::GraphqlEndpoint::as_principal(
            format!("http://{address}/api/v0/graphql"),
            user.did(),
        )
        .with_client_signing_capability(user.client_signing_capability().unwrap())
        .unwrap()
        .with_client_signed_agent_creates(),
    );
    let seed = crate::store::config_client::DesiredStateApplyPlan::new(vec![
        (crate::Collection::Node, json!({"node_did":root.did()})),
        (crate::Collection::InferenceBackend, json!({"node_did":root.did(),"backend_id":"backend","name":"Local","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:8000/v1","auth":{"kind":"unauthenticated"}})),
        (crate::Collection::InferenceProfile, json!({"node_did":root.did(),"profile_id":"profile","backend_id":"backend","model_name":"model"})),
    ].into_iter().map(|(collection, value)| crate::store::config_client::DesiredStateApplyDocument { collection, add:value.clone(), update:value }).collect()).unwrap();
    access
        .transact_write("test.pack_identity.seed", |txn| {
            let seed = &seed;
            Box::pin(async move { crate::documents::apply_desired_state_plan(txn, seed).await })
        })
        .await
        .unwrap();
    (directory, root, node, access)
}

#[tokio::test]
async fn installed_agents_reuse_bindings_and_reject_stale_preparation() {
    let (_directory, root, node, access) = fixture().await;
    let first = load(&access, &node, &root, "one", "Worker").await;
    let competing = load(&access, &node, &root, "one", "Worker").await;
    assert_ne!(first.agent_bindings, competing.agent_bindings);
    let report = install(&access, root.did(), &first).await.unwrap();
    assert_eq!(
        report.agents.get("worker"),
        Some(&first.agent_bindings["worker"].agent_did)
    );
    assert!(install(&access, root.did(), &competing).await.is_err());
    let upgraded = load(&access, &node, &root, "one", "Renamed").await;
    assert_eq!(first.agent_bindings, upgraded.agent_bindings);
    install(&access, root.did(), &upgraded).await.unwrap();
    let another_install = load(&access, &node, &root, "two", "Renamed").await;
    assert_ne!(first.agent_bindings, another_install.agent_bindings);
    let expected = &first.agent_bindings["worker"];
    let actual = access.execute("{ Agent { agent_did creation_nonce display_name } Task { agent_did } AgentTarget { agent_did } PackInstallation { agent_bindings } }").await.unwrap();
    assert_eq!(actual["data"]["Agent"].as_array().unwrap().len(), 1);
    assert_eq!(actual["data"]["Agent"][0]["agent_did"], expected.agent_did);
    assert_eq!(
        actual["data"]["Agent"][0]["creation_nonce"],
        expected.creation_nonce
    );
    assert_eq!(actual["data"]["Agent"][0]["display_name"], "Renamed");
    assert_eq!(actual["data"]["Task"][0]["agent_did"], expected.agent_did);
    assert_eq!(
        actual["data"]["AgentTarget"][0]["agent_did"],
        expected.agent_did
    );
    assert_eq!(
        actual["data"]["PackInstallation"][0]["agent_bindings"],
        serde_json::to_value(&first.agent_bindings).unwrap()
    );
    let stale = load(&access, &node, &root, "one", "Stale").await;
    super::super::remove_pack(
        &access,
        root.did(),
        "fixture/one",
        super::super::DriftPolicy::Refuse,
    )
    .await
    .unwrap();
    assert!(install(&access, root.did(), &stale).await.is_err());
    node.shutdown().await.expect("node shutdown");
}

#[test]
fn dependency_reference_matches_lean() {
    for case in crate::lean_vocab_test::lean_pack_identity_reference_cases() {
        let actual = (|| -> Result<AgentIdentityBinding> {
            let mut available = BTreeMap::new();
            for dependency in &case.available {
                anyhow::ensure!(
                    available
                        .insert(
                            dependency.coordinate.clone(),
                            decode_bindings(&dependency.bindings)?
                        )
                        .is_none(),
                    "duplicate dependency coordinate"
                );
            }
            resolve_dependency_binding(&case.declared, &available, &case.dependency, &case.slot)
                .cloned()
        })();
        assert_eq!(actual.is_ok(), case.expected.is_some(), "{}", case.name);
        if let Some(expected) = &case.expected {
            assert_eq!(
                actual.unwrap(),
                AgentIdentityBinding {
                    creation_nonce: expected.creation_nonce.clone(),
                    agent_did: expected.agent_did.clone()
                },
                "{}",
                case.name
            );
        }
    }
}

fn archive(manifest: &PackManifest, config: &Value) -> super::super::archive::PackArchive {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("manifest.json"),
        serde_json::to_vec(manifest).unwrap(),
    )
    .unwrap();
    std::fs::write(
        directory.path().join("config.json"),
        serde_json::to_vec(config).unwrap(),
    )
    .unwrap();
    std::fs::write(
        directory.path().join("README.md"),
        "# Fixture\n\n## Usage\nFixture",
    )
    .unwrap();
    let (bytes, _) = super::super::archive::pack_dir(directory.path()).unwrap();
    super::super::archive::PackArchive::from_bytes(&bytes).unwrap()
}

#[tokio::test]
async fn dependency_slot_is_provisioned_once_and_owned_by_its_installation() {
    let (_directory, root, node, access) = fixture().await;
    let dependency = archive(&manifest("dependency"), &config("Dependency worker"));
    let mut root_manifest = manifest("scenario");
    root_manifest.metadata.inference_slots.clear();
    root_manifest.metadata.dependencies = vec!["fixture/dependency".into()];
    let raw = json!({"node":{},"tasks":[{"task_id":"dependent","agent_did":"gents:agent-slot:fixture/dependency#worker","prompt_template":"Execute"}]});
    let root_archive = archive(&root_manifest, &raw);
    let local = ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(access.actor_did().unwrap()).unwrap(),
    );
    let provisioner = AgentProvisioner {
        node: &node,
        runtime: &root,
        actor: None,
        selected_agent_did: None,
        node_binding: None,
    };
    let prepared = super::super::prepare_document_pack_archive_with_provisioner(
        &local,
        root.did(),
        &root_archive,
        &BTreeMap::from([("model".into(), "profile".into())]),
        &[&dependency],
        InstalledVia::Operator,
        None,
        &|_| None,
        Some(&provisioner),
    )
    .await
    .unwrap();
    let expected = &prepared.dependencies[0].agent_bindings["worker"];
    assert!(prepared.agent_bindings.is_empty());
    assert_eq!(prepared.config.tasks[0].agent_did, expected.agent_did);
    let expected_did = expected.agent_did.clone();
    let (report, _) = super::super::install_prepared_document_graph(
        &access,
        root.did(),
        &root_archive,
        &prepared,
        super::super::DriftPolicy::Refuse,
        None,
        false,
        true,
    )
    .await
    .unwrap();
    assert_eq!(
        report.agents.get("fixture/dependency#worker"),
        Some(&expected_did)
    );
    let rows = access.execute("{ Agent { agent_did } Task(filter: { task_id: { _eq: \"dependent\" } }) { agent_did } PackInstallation { coordinate agent_bindings } }").await.unwrap();
    assert_eq!(rows["data"]["Agent"].as_array().unwrap().len(), 1);
    assert_eq!(rows["data"]["Agent"][0]["agent_did"], expected_did);
    assert_eq!(rows["data"]["Task"][0]["agent_did"], expected_did);
    let records = rows["data"]["PackInstallation"].as_array().unwrap();
    let own = records
        .iter()
        .find(|row| row["coordinate"] == "fixture/scenario")
        .unwrap();
    let dependency = records
        .iter()
        .find(|row| row["coordinate"] == "fixture/dependency")
        .unwrap();
    assert!(own["agent_bindings"].is_null());
    assert_eq!(
        dependency["agent_bindings"]["worker"]["agent_did"],
        expected_did
    );
    node.shutdown().await.expect("node shutdown");
}

#[test]
fn workspace_callback_agent_slots_bind_writer_integrator_and_readers() {
    let bindings = PackAgentBindings::from([
        (
            "writer".into(),
            AgentIdentityBinding {
                agent_did: "did:key:writer".into(),
                creation_nonce: "writer-nonce".into(),
            },
        ),
        (
            "integrator".into(),
            AgentIdentityBinding {
                agent_did: "did:key:integrator".into(),
                creation_nonce: "integrator-nonce".into(),
            },
        ),
        (
            "reviewer".into(),
            AgentIdentityBinding {
                agent_did: "did:key:reviewer".into(),
                creation_nonce: "reviewer-nonce".into(),
            },
        ),
    ]);
    let mut callback = json!({"workspace_agents":{
        "writer_agent_did":"gents:agent-slot:writer",
        "integrator_agent_did":"gents:agent-slot:integrator",
        "readers":[{"agent_did":"gents:agent-slot:reviewer"}]
    }});
    rewrite_references(&mut callback, &bindings).unwrap();
    assert_eq!(
        callback["workspace_agents"]["writer_agent_did"],
        bindings["writer"].agent_did
    );
    assert_eq!(
        callback["workspace_agents"]["integrator_agent_did"],
        bindings["integrator"].agent_did
    );
    assert_eq!(
        callback["workspace_agents"]["readers"][0]["agent_did"],
        bindings["reviewer"].agent_did
    );
    let mut invalid = json!({"unknown":"gents:agent-slot:writer"});
    assert!(rewrite_references(&mut invalid, &bindings).is_err());
}
