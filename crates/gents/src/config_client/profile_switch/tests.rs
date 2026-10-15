use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::Utc;
use defra_node::EmbeddedNode;
use gents_loop::account_usage::{UsageReport, UsageSource, UsageWindow};
use serde_json::{json, Value};

use super::*;
use crate::claude_oauth::CLAUDE_OAUTH_PROVIDER;
use crate::config_client::{
    apply_desired_state_plan, list_inference_backends_in_txn, read_desired_state_record_in_txn,
    write_inference_backend_document, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::document_config::AdvertisedModel;
use crate::oauth_credential::{
    preset_account_backend, remove_account_in_txn, set_account_enabled, store_sign_in,
    OAuthCredential,
};
use crate::usage_observation::{record_usage, UsageAccount};
use crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;
use crate::{Collection, InferenceBackend};

const DID: &str = "did:key:z6MkTestSwitch";

fn claude_sign_in(did: &str, who: &str) -> OAuthCredential {
    crate::claude_oauth::credential_from_login_tokens(
        did,
        CLAUDE_OAUTH_PROVIDER,
        &crate::claude_oauth::ClaudeLoginTokens {
            access_token: format!("access-SECRET-{who}"),
            refresh_token: format!("refresh-{who}"),
            expires_in: Some(3600),
            scope: None,
            account_id: Some("IDENTITY".into()),
            organization_uuid: Some("org-1".into()),
            account_uuid: Some(format!("account-{who}")),
        },
        Utc::now(),
    )
}

/// Claude accounts A (original, `label-a`), B and D added, C added and
/// disabled, and a Grok backend G. Profiles `p`, `summ` and `q` on A; behavior
/// `x` on `p` with `summ` as its compaction profile, `y` on `p`. A, B, C and G
/// list their models; D's catalog was never read.
struct Fixture {
    node: Arc<EmbeddedNode>,
    access: ConfigAccess,
    did: String,
    credentials: BTreeMap<&'static str, OAuthCredential>,
    backends: BTreeMap<&'static str, String>,
}

impl Fixture {
    async fn new(did: &str) -> Self {
        Self::build(did, &["a", "b", "c", "d"]).await
    }

    async fn build(did: &str, accounts: &[&'static str]) -> Self {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        crate::ensure_node(&node, did).await.unwrap();
        let access = ConfigAccess::Local(node.clone());
        for (provider, name) in [
            (CLAUDE_OAUTH_PROVIDER, "Claude"),
            (XAI_OAUTH_PROVIDER, "Grok"),
        ] {
            let backend = preset_account_backend(did, provider, None, name.to_owned()).unwrap();
            write_inference_backend_document(&access, &backend)
                .await
                .unwrap();
        }
        let mut credentials = BTreeMap::new();
        let mut backends = BTreeMap::from([("g", XAI_OAUTH_PROVIDER.to_owned())]);
        for who in accounts {
            let label = format!("label-{who}");
            let signed = store_sign_in(&access, claude_sign_in(did, who), Some(&label))
                .await
                .unwrap()
                .credential;
            let backend_id = match signed.account_ref.as_deref() {
                Some(account_ref) => format!("{CLAUDE_OAUTH_PROVIDER}-{account_ref}"),
                None => CLAUDE_OAUTH_PROVIDER.to_owned(),
            };
            backends.insert(*who, backend_id);
            credentials.insert(*who, signed);
        }
        if let Some(c) = credentials.get("c") {
            set_account_enabled(&access, did, &c.credential_id, false)
                .await
                .unwrap();
        }
        let fixture = Self {
            node,
            access,
            did: did.to_owned(),
            credentials,
            backends,
        };
        for (who, models) in [
            ("a", &["model-x", "model-s"][..]),
            ("b", &["model-x", "model-s"]),
            ("c", &["model-x"]),
            ("g", &["model-x"]),
        ] {
            if fixture.backends.contains_key(who) {
                fixture.catalog(who, models).await;
            }
        }
        let a = fixture.backends["a"].clone();
        fixture
            .apply(vec![
                (
                    Collection::InferenceProfile,
                    json!({"node_did": did, "profile_id": "p", "backend_id": a, "model_name": "model-x"}),
                ),
                (
                    Collection::InferenceProfile,
                    json!({"node_did": did, "profile_id": "summ", "backend_id": a, "model_name": "model-s"}),
                ),
                (
                    Collection::InferenceProfile,
                    json!({"node_did": did, "profile_id": "q", "backend_id": a, "model_name": "model-x"}),
                ),
                (
                    Collection::Compaction,
                    json!({"node_did": did, "compaction_id": "compaction-x", "inference_profile_id": "summ"}),
                ),
                (
                    Collection::AgentContext,
                    json!({"node_did": did, "context_id": "context-x", "compaction_id": "compaction-x"}),
                ),
                (
                    Collection::Agent,
                    json!({"node_did": did, "agent_id": "x", "context_id": "context-x", "inference_profile_id": "p"}),
                ),
                (
                    Collection::Agent,
                    json!({"node_did": did, "agent_id": "y", "inference_profile_id": "p"}),
                ),
            ])
            .await;
        fixture
    }

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
            .transact("test.switch.apply", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
            })
            .await
            .unwrap();
    }

    async fn backend(&self, who: &str) -> InferenceBackend {
        let did = self.did.as_str();
        let backends = self
            .access
            .transact("test.switch.backends", |txn| {
                Box::pin(async move { list_inference_backends_in_txn(txn, did).await })
            })
            .await
            .unwrap();
        backends
            .into_iter()
            .find(|backend| backend.backend_id == self.backends[who])
            .unwrap()
    }

    async fn catalog(&self, who: &str, models: &[&str]) {
        let backend = self.backend(who).await;
        let models = models
            .iter()
            .map(|model| AdvertisedModel {
                model_name: (*model).to_owned(),
                display_name: None,
                context_window: None,
                max_context_window: None,
                max_output_tokens: None,
                reasoning_efforts: None,
            })
            .collect();
        crate::backend_registry::record_discovered_catalog_on(&self.access, &backend, models)
            .await
            .unwrap();
    }

    /// Every profile document as stored.
    async fn profiles(&self) -> BTreeMap<String, Value> {
        let did = self.did.as_str();
        self.access
            .transact("test.switch.profiles", |txn| {
                Box::pin(async move {
                    let mut records = BTreeMap::new();
                    for id in ["p", "summ", "q", "k", "m", "o"] {
                        if let Some((_, value)) = read_desired_state_record_in_txn(
                            txn,
                            Collection::InferenceProfile,
                            did,
                            id,
                        )
                        .await?
                        {
                            records.insert(id.to_owned(), value);
                        }
                    }
                    Ok(records)
                })
            })
            .await
            .unwrap()
    }

    async fn backend_of(&self, profile: &str) -> String {
        self.profiles().await[profile]["backend_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn plan(&self) -> Result<SwitchPlan> {
        switch_candidates(&self.access, &self.did, "p", &[], Utc::now()).await
    }

    async fn switch(&self, profile: &str, to: &str, companions: bool) -> Result<SwitchReceipt> {
        switch_profile_account(
            &self.access,
            &self.did,
            profile,
            &self.backends[to],
            companions,
            &[],
        )
        .await
    }
}

#[tokio::test]
async fn candidates_list_other_enabled_accounts_with_usage() {
    let fixture = Fixture::new(DID).await;
    let observed_at = Utc::now() - chrono::Duration::minutes(5);
    record_usage(
        &fixture.node,
        &UsageAccount::for_credential(&fixture.credentials["b"]),
        UsageReport {
            windows: vec![UsageWindow {
                label: "five_hour".into(),
                window_minutes: Some(300),
                used_pct: 100.0,
                resets_at: Some(Utc::now() + chrono::Duration::hours(2)),
                source: UsageSource::Header,
                observed_at,
            }],
            ..UsageReport::default()
        },
    )
    .await
    .unwrap();

    let plan = fixture.plan().await.unwrap();

    assert_eq!(plan.profile, "p");
    assert_eq!(plan.account.label, "label-a");
    assert_eq!(plan.agents, ["x", "y"]);
    assert_eq!(plan.companions, ["summ"]);
    assert_eq!(plan.cost, SWITCH_COST);
    let listed: Vec<_> = plan
        .candidates
        .iter()
        .map(|candidate| {
            (
                candidate.label.as_str(),
                candidate.backend_id.as_str(),
                candidate.models,
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            ("label-b", fixture.backends["b"].as_str(), "offered"),
            ("label-d", fixture.backends["d"].as_str(), "not read yet"),
        ],
        "C is disabled, G is another provider, A is the profile's own"
    );
    let exhausted = &plan.candidates[0].usage.windows;
    assert_eq!(exhausted.len(), 1, "an exhausted account stays listed");
    assert_eq!(exhausted[0].used_pct, 100.0);
    assert_eq!(exhausted[0].source, UsageSource::Header);
    assert!(exhausted[0].age_secs >= 300, "{}", exhausted[0].age_secs);
    assert_eq!(plan.candidates[1].usage.note, Some("unknown"));
}

#[tokio::test]
async fn switch_moves_only_the_profile() {
    let fixture = Fixture::new(DID).await;
    let before = fixture.profiles().await;

    let receipt = fixture.switch("p", "b", false).await.unwrap();

    assert_eq!(
        receipt.headline,
        "Move profile p to label-b (used by 2 agents)"
    );
    assert_eq!(receipt.account.label, "label-b");
    assert_eq!(receipt.backend_id, fixture.backends["b"]);
    assert_eq!(receipt.agents, ["x", "y"]);
    assert_eq!(receipt.cost, SWITCH_COST);
    assert_eq!(receipt.companions_offered, ["summ"]);
    assert!(receipt.companions_moved.is_empty());
    let mut after = fixture.profiles().await;
    assert_eq!(after["p"]["backend_id"], json!(fixture.backends["b"]));
    let mut moved = after.remove("p").unwrap();
    let mut was = before["p"].clone();
    moved["backend_id"] = Value::Null;
    was["backend_id"] = Value::Null;
    assert_eq!(moved, was, "only backend_id changes");
    let mut others = before.clone();
    others.remove("p");
    assert_eq!(after, others, "q and summ are untouched");
}

#[tokio::test]
async fn switch_with_companions_moves_both_in_one_transaction() {
    let fixture = Fixture::new(DID).await;

    let receipt = fixture.switch("p", "b", true).await.unwrap();

    assert_eq!(receipt.companions_moved, ["summ"]);
    assert!(receipt.companions_offered.is_empty());
    assert_eq!(fixture.backend_of("p").await, fixture.backends["b"]);
    assert_eq!(fixture.backend_of("summ").await, fixture.backends["b"]);
    assert_eq!(fixture.backend_of("q").await, fixture.backends["a"]);

    let fixture = Fixture::new(DID).await;
    fixture.catalog("b", &["model-x"]).await;
    let receipt = fixture.switch("p", "b", true).await.unwrap();
    assert!(
        receipt.companions_moved.is_empty() && receipt.companions_offered.is_empty(),
        "B does not offer summ's model: {receipt:?}"
    );
    assert_eq!(fixture.backend_of("p").await, fixture.backends["b"]);
    assert_eq!(fixture.backend_of("summ").await, fixture.backends["a"]);
}

#[tokio::test]
async fn next_turns_use_the_target_credentials() {
    use crate::agent::PendingAgent;
    use crate::identity::{KeyIdentity, NodeIdentity as _};
    use crate::oauth_credential::BearerSource as _;
    let key = std::env::temp_dir().join(format!("switch-{}.key", uuid::Uuid::new_v4()));
    let did = KeyIdentity::load_or_create(&key, None)
        .unwrap()
        .did()
        .to_string();
    let fixture = Fixture::new(&did).await;

    fixture.switch("p", "b", false).await.unwrap();

    let did_ref = did.as_str();
    let references = fixture
        .access
        .transact("test.switch.references", |txn| {
            Box::pin(async move { crate::ConfigReferences::load_in_txn(txn, did_ref).await })
        })
        .await
        .unwrap();
    for agent_id in ["x", "y"] {
        let profile = &references.agent_profiles(agent_id)[0];
        let (_, backend) = references.profile_with_backend(profile).unwrap().unwrap();
        let fields = backend.backend_fields();
        assert_eq!(
            fields.backend_id.as_deref(),
            Some(fixture.backends["b"].as_str()),
            "{agent_id}"
        );
        let mut behavior = PendingAgent::new(agent_id)
            .build_with_identity_for_test(KeyIdentity::load_or_create(&key, None).unwrap());
        behavior.backend_id = fields.backend_id;
        behavior.backend_provider_kind = fields.backend_provider_kind;
        behavior.openai_wire_api = fields.openai_wire_api;
        behavior.backend_endpoint = fields.backend_endpoint;
        behavior.backend_auth = fields.backend_auth;
        crate::llm::backend_client::build_backend_client(
            fixture.node.clone(),
            &behavior,
            "key",
            std::time::Duration::from_secs(5),
        )
        .await
        .unwrap_or_else(|error| panic!("{agent_id}: {error:#}"));
        let bearer = crate::oauth_credential::test_support::bound_bearer(
            &fixture.credentials["b"].credential_id,
        )
        .unwrap_or_else(|| panic!("{agent_id} did not bind B"));
        assert_eq!(bearer.current_bearer().await.unwrap(), "access-SECRET-b");
    }
    assert!(
        crate::oauth_credential::test_support::bound_bearer(
            &fixture.credentials["a"].credential_id
        )
        .is_none(),
        "no behavior bound A"
    );
}

#[tokio::test]
async fn companions_are_the_other_profiles_of_the_same_behaviors_on_one_account() {
    let fixture = Fixture::new(DID).await;
    let (a, b) = (&fixture.backends["a"], &fixture.backends["b"]);
    let did = fixture.did.as_str();
    // `y` compacts with `o` on B; `z` runs on `m` on A and compacts with `p`.
    fixture
        .apply(vec![
            (
                Collection::InferenceProfile,
                json!({"node_did": did, "profile_id": "m", "backend_id": a, "model_name": "model-x"}),
            ),
            (
                Collection::InferenceProfile,
                json!({"node_did": did, "profile_id": "o", "backend_id": b, "model_name": "model-x"}),
            ),
            (
                Collection::Compaction,
                json!({"node_did": did, "compaction_id": "compaction-y", "inference_profile_id": "o"}),
            ),
            (
                Collection::AgentContext,
                json!({"node_did": did, "context_id": "context-y", "compaction_id": "compaction-y"}),
            ),
            (
                Collection::Agent,
                json!({"node_did": did, "agent_id": "y", "context_id": "context-y", "inference_profile_id": "p"}),
            ),
            (
                Collection::Compaction,
                json!({"node_did": did, "compaction_id": "compaction-z", "inference_profile_id": "p"}),
            ),
            (
                Collection::AgentContext,
                json!({"node_did": did, "context_id": "context-z", "compaction_id": "compaction-z"}),
            ),
            (
                Collection::Agent,
                json!({"node_did": did, "agent_id": "z", "context_id": "context-z", "inference_profile_id": "m"}),
            ),
        ])
        .await;

    let plan = fixture.plan().await.unwrap();
    assert_eq!(plan.agents, ["x", "y", "z"]);
    assert_eq!(plan.companions, ["m", "summ"], "o is on another account");
    let summ = switch_candidates(&fixture.access, did, "summ", &[], Utc::now())
        .await
        .unwrap();
    assert_eq!(
        summ.companions,
        ["p"],
        "the main profile of summ's behavior"
    );

    let receipt = fixture.switch("summ", "d", true).await.unwrap();
    assert_eq!(receipt.companions_moved, ["p"]);
    for (profile, on) in [("summ", "d"), ("p", "d"), ("m", "a"), ("o", "b")] {
        assert_eq!(
            fixture.backend_of(profile).await,
            fixture.backends[on],
            "{profile}"
        );
    }
}

#[tokio::test]
async fn an_account_without_the_model_is_not_listed() {
    let fixture = Fixture::new(DID).await;
    fixture.catalog("d", &["model-s"]).await;
    let plan = fixture.plan().await.unwrap();
    let labels: Vec<_> = plan
        .candidates
        .iter()
        .map(|candidate| candidate.label.as_str())
        .collect();
    assert_eq!(labels, ["label-b"], "D does not offer model-x");
}

async fn assert_refused(fixture: &Fixture, to: &str, message: &str) {
    let before = fixture.profiles().await;
    let error = fixture.switch("p", to, true).await.unwrap_err();
    assert!(format!("{error:#}").contains(message), "{error:#}");
    assert_eq!(fixture.profiles().await, before, "nothing written");
}

#[tokio::test]
async fn refusals_write_nothing() {
    let fixture = Fixture::new(DID).await;
    assert_refused(
        &fixture,
        "c",
        r#"account "label-c" is disabled; enabled Claude accounts: "#,
    )
    .await;
    assert_refused(
        &fixture,
        "g",
        r#"account "Grok" belongs to another provider; profile "p" moves only to another Claude account"#,
    )
    .await;
    assert_refused(
        &fixture,
        "a",
        r#"profile "p" is already on account "label-a""#,
    )
    .await;
    fixture.catalog("b", &["model-s"]).await;
    assert_refused(&fixture, "b", r#"account "label-b" does not offer model-x"#).await;

    let did = fixture.did.as_str();
    let b = fixture.credentials["b"].credential_id.clone();
    fixture
        .access
        .transact("test.switch.remove", |txn| {
            let b = b.clone();
            Box::pin(async move { remove_account_in_txn(txn, did, &b).await.map(|_| ()) })
        })
        .await
        .unwrap();
    assert_refused(
        &fixture,
        "b",
        r#"account "label-b" is account not on this node"#,
    )
    .await;
}

#[tokio::test]
async fn no_candidate_says_how_to_add_one() {
    let fixture = Fixture::build(DID, &["a"]).await;
    let error = fixture.plan().await.unwrap_err();
    assert_eq!(
        format!("{error:#}"),
        "no other Claude account offers model-x; add one with `gents claude-login --label <label>`"
    );
}

#[tokio::test]
async fn the_plan_holds_no_secret() {
    let fixture = Fixture::new(DID).await;
    let plan = serde_json::to_string(&fixture.plan().await.unwrap()).unwrap();
    let receipt = serde_json::to_string(&fixture.switch("p", "b", false).await.unwrap()).unwrap();
    for text in [plan, receipt] {
        assert!(text.contains("label-b"), "{text}");
        for secret in ["SECRET", "IDENTITY", "account-b", "refresh-"] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
        for credential in fixture.credentials.values() {
            assert!(!text.contains(&credential.credential_id), "{text}");
        }
    }
}

#[tokio::test]
async fn a_bound_plugin_slot_counts_and_keeps_serving_after_the_switch() {
    use crate::plugin::model_calls::{AccessModels, ModelBinding, ModelResolver};
    use crate::plugin::store;
    let did = "did:key:z6MkTestSwitchSlot";
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    crate::ensure_node(&node, did).await.unwrap();
    let fixture = Fixture {
        node: node.clone(),
        access: ConfigAccess::Local(node),
        did: did.to_owned(),
        credentials: BTreeMap::new(),
        backends: BTreeMap::from([("k1", "k1".to_owned()), ("k2", "k2".to_owned())]),
    };
    for (id, port) in [("k1", 1), ("k2", 2)] {
        let backend: InferenceBackend = serde_json::from_value(json!({
            "node_did": did, "backend_id": id, "name": format!("name-{id}"),
            "provider_kind": "OpenAiCompatible", "endpoint": format!("http://127.0.0.1:{port}/v1"),
            "auth": {"kind": "api_key", "key": "KEY-SENTINEL"},
        }))
        .unwrap();
        write_inference_backend_document(&fixture.access, &backend)
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
                json!({"node_did": did, "agent_id": "reader", "inference_profile_id": "k"}),
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
    let plan = switch_candidates(&fixture.access, did, "k", &slots, Utc::now())
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
        .resolve(&binding)
        .await
        .unwrap();
    assert_eq!(endpoint.url, "http://127.0.0.1:2/v1/chat/completions");
}
