use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::Utc;
use gents_documents::{apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents_inference::profile_switch::{switch_candidates, switch_profile_account};
use gents_inference::resolved::ProviderProfilePolicy;
use gents_protocol::agent_identity::AgentIdentityBinding;
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;
use serde_json::{json, Value};

fn provision_agents(
    did: &str,
    names: &[&'static str],
) -> BTreeMap<&'static str, AgentIdentityBinding> {
    let root = gents_store::test_support::held_runtime_identity(did);
    names
        .iter()
        .map(|name| {
            let binding = gents_store::identity::provision_agent_identity(&root).unwrap();
            (*name, binding)
        })
        .collect()
}

struct Fixture {
    node: Arc<defra_node::Node>,
    access: ConfigAccess,
}

impl Fixture {
    async fn apply(&self, documents: Vec<(Collection, Value)>) {
        let plan = DesiredStateApplyPlan::new(
            documents
                .into_iter()
                .map(|(collection, value)| DesiredStateApplyDocument {
                    collection,
                    add: value.clone(),
                    update: value,
                })
                .collect(),
        )
        .unwrap();
        self.access
            .transact_write("test.switch.apply", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan, &ProviderProfilePolicy)
                        .await
                        .map(|_| ())
                })
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn a_bound_plugin_slot_counts_and_keeps_serving_after_the_switch() {
    use crate::plugin::model_binding::{AccessModels, ModelBinding, ModelResolver};
    use crate::plugin::store;
    let node = Arc::new(
        gents_store::test_support::node_builder()
            .build()
            .await
            .unwrap(),
    );
    let did_owned = node.node_identity_did().unwrap().to_owned();
    let did = did_owned.as_str();
    gents_store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let access = ConfigAccess::local_as(node.clone(), identity::Did::new(did).unwrap());
    gents_documents::test_support::bootstrap_signed_test_node(&access, did).await;
    let agents = provision_agents(did, &["reader"]);
    let fixture = Fixture {
        node: node.clone(),
        access,
    };
    for (id, port) in [("k1", 1), ("k2", 2)] {
        let backend: gents_documents::InferenceBackend = serde_json::from_value(json!({
            "node_did": did, "backend_id": id, "name": format!("name-{id}"),
            "provider_kind": "OpenAiCompatible", "endpoint": format!("http://127.0.0.1:{port}/v1"),
            "auth": {"kind": "api_key", "key": "KEY-SENTINEL"},
        }))
        .unwrap();
        gents_documents::write_inference_backend_document(
            &fixture.access,
            &backend,
            &ProviderProfilePolicy,
        )
        .await
        .unwrap();
    }
    fixture
        .apply(vec![
            (
                Collection::InferenceProfile,
                json!({"node_did": did, "profile_id": "k", "backend_id": "k1", "model_name": "model-k"}),
            ),
            (
                Collection::Agent,
                json!({"node_did": did, "agent_did": agents["reader"].agent_did, "creation_nonce": agents["reader"].creation_nonce, "inference_profile_id": "k"}),
            ),
        ])
        .await;
    let home = tempfile::tempdir().unwrap();
    for (name, owner, profile) in [
        ("ocr", did, "k"),
        ("other-profile", did, "q"),
        ("other-agent", "did:key:z6MkTestSwitchOther", "k"),
        ("unbound", did, ""),
    ] {
        let record: store::InstalledPlugin = serde_json::from_value(json!({
            "namespace": "team", "name": name, "version": "1.0.0",
            "digest": format!("sha256:{}", "0".repeat(64)), "language": "rust",
            "declaration": {
                "name": name, "description": "reads pages", "artifact": format!("plugins/{name}.afb"),
                "language": "rust", "input_schema": {"type": "object"},
                "model_slot": "remote_ocr",
            },
            "model_binding": (!profile.is_empty())
                .then(|| json!({"node_did": owner, "profile_id": profile})),
        }))
        .unwrap();
        store::write_record(home.path(), &record).unwrap();
    }

    let slots = store::bound_to_profile(home.path(), did, "k").unwrap();
    assert_eq!(slots, ["team/ocr"]);
    let plan = switch_candidates(&fixture.access, did, "k", &slots, Utc::now(), &[])
        .await
        .unwrap();
    assert_eq!(plan.plugin_slots, ["team/ocr"]);
    assert_eq!(plan.candidates[0].label, "name-k2");
    let receipt = switch_profile_account(&fixture.access, did, "k", "k2", false, &slots)
        .await
        .unwrap();
    assert_eq!(
        receipt.headline,
        "Move profile k to name-k2 (used by 1 agent and 1 plugin slot)"
    );
    assert_eq!(
        serde_json::to_value(&receipt).unwrap()["plugin_slots"],
        json!(["team/ocr"])
    );

    let binding = store::read_record(home.path(), "team", "ocr")
        .unwrap()
        .model_binding
        .unwrap();
    assert_eq!(
        binding,
        ModelBinding {
            node_did: did.to_owned(),
            profile_id: "k".to_owned(),
        },
        "the binding names the profile, so the move needs no binding write"
    );
    let endpoint = AccessModels(&fixture.access)
        .resolve(
            &binding,
            Some(&identity::Did::new(fixture.node.node_identity_did().unwrap()).unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(endpoint.url, "http://127.0.0.1:2/v1/chat/completions");
}
