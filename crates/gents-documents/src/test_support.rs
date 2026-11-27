pub mod event_cursor;
mod signed_home;

pub use signed_home::SignedHome;

/// Admits every advertised profile: the provider rules live in the inference
/// owner, and documents-level fixtures publish documents without a provider.
pub struct AdmitAllProfiles;

#[async_trait::async_trait]
impl crate::inference_backend::AdvertisedProfilePolicy for AdmitAllProfiles {
    async fn validate_advertised_profile(
        &self,
        _txn: &gents_store::config_client::ConfigApplyTxn<'_>,
        _owner: &str,
        _backend: &crate::InferenceBackend,
        _profile: &crate::InferenceProfile,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TestNodeLockKey {
    Local(usize, String),
    Remote(String, String),
}

fn test_node_lock(key: TestNodeLockKey) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    use rapidhash::{HashMapExt, RapidHashMap};
    use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};

    static LOCKS: OnceLock<StdMutex<RapidHashMap<TestNodeLockKey, Weak<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| StdMutex::new(RapidHashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.retain(|_, lock| lock.strong_count() > 0);
    let entry = locks.entry(key).or_default();
    if let Some(lock) = entry.upgrade() {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    *entry = Arc::downgrade(&lock);
    lock
}

fn test_node_lock_key(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
) -> TestNodeLockKey {
    match access {
        gents_store::config_client::ConfigAccess::Local(local) => TestNodeLockKey::Local(
            std::sync::Arc::as_ptr(local.node()) as usize,
            node_did.to_owned(),
        ),
        gents_store::config_client::ConfigAccess::Graphql(endpoint) => {
            TestNodeLockKey::Remote(endpoint.url().to_owned(), node_did.to_owned())
        }
    }
}

async fn create_test_node_if_absent(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
) -> anyhow::Result<()> {
    use crate::{DesiredStateApplyDocument, DesiredStateApplyPlan};

    let node_input = serde_json::json!({"node_did": node_did});
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: gents_store::collection::Collection::Node,
        add: node_input.clone(),
        update: node_input,
    }])?;
    access
        .transact_write("test.signed_node", |txn| {
            let plan = &plan;
            Box::pin(async move {
                if crate::desired_state::find_node_doc_id(txn, node_did)
                    .await?
                    .is_some()
                {
                    return Ok(());
                }
                crate::apply_desired_state_plan(txn, plan, &AdmitAllProfiles)
                    .await
                    .map(|_| ())
            })
        })
        .await
}

/// Creates the fixture Node through the runtime owner's signed access.
/// `ensure_node` only loads; production Nodes are created by an authenticated client.
///
/// Serializes per `(node, node_did)` and treats an already-committed Node from a
/// concurrent writer as success without hiding store failures when no Node exists
/// (`tla/TestNodeEnsure.tla`).
pub async fn ensure_test_node(node: &std::sync::Arc<defra_node::Node>, node_did: &str) {
    if crate::load_node(node, node_did).await.unwrap().is_some() {
        return;
    }
    let lock = test_node_lock(TestNodeLockKey::Local(
        std::sync::Arc::as_ptr(node) as usize,
        node_did.to_owned(),
    ));
    let _guard = lock.lock().await;
    if crate::load_node(node, node_did).await.unwrap().is_some() {
        return;
    }
    let access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(node_did).unwrap(),
    );
    if let Err(error) = create_test_node_if_absent(&access, node_did).await {
        if crate::load_node(node, node_did).await.unwrap().is_some() {
            return;
        }
        panic!("ensuring fixture Node {node_did:?} failed: {error:#}");
    }
}

/// Enrols `user` as the runtime's operator and publishes the runtime's Node
/// through the User's client-signed `access`, as `gents init` does: the Node
/// is User configuration, and provisioning an Agent requires it.
pub async fn enroll_test_user(
    node: &std::sync::Arc<defra_node::Node>,
    runtime: &dyn gents_store::identity::NodeIdentity,
    user: &gents_store::identity::KeyIdentity,
    access: &gents_store::config_client::ConfigAccess,
) {
    gents_store::test_support::grant_operator_user(node, runtime, user).await;
    bootstrap_signed_test_node(access, runtime.did()).await;
}

pub async fn bootstrap_signed_test_node(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
) {
    let lock = test_node_lock(test_node_lock_key(access, node_did));
    let _guard = lock.lock().await;
    if let Err(error) = create_test_node_if_absent(access, node_did).await {
        let existing = access
            .transact_readonly("test.signed_node.recheck", |txn| {
                Box::pin(async move { crate::desired_state::find_node_doc_id(txn, node_did).await })
            })
            .await
            .ok()
            .flatten()
            .is_some();
        if existing {
            return;
        }
        panic!("bootstrapping fixture Node {node_did:?} failed: {error:#}");
    }
}

pub async fn set_test_default_agent_as(
    access: &gents_store::config_client::ConfigAccess,
    runtime_did: &str,
    agent_did: &str,
) {
    let selection =
        serde_json::to_value(crate::DefaultAgentSelection::new(runtime_did, agent_did)).unwrap();
    let plan = crate::DesiredStateApplyPlan::new(vec![crate::DesiredStateApplyDocument {
        collection: gents_store::collection::Collection::DefaultAgentSelection,
        add: selection.clone(),
        update: selection,
    }])
    .unwrap();
    access
        .transact_write("test.set_default_agent", |txn| {
            let plan = &plan;
            Box::pin(
                async move { crate::apply_desired_state_plan(txn, plan, &AdmitAllProfiles).await },
            )
        })
        .await
        .unwrap();
}

/// Replaces one field of the fixture Node through its configuring User's
/// signed desired-state owner; Node is configuration the User signs.
pub async fn set_test_node_field_as(
    access: &gents_store::config_client::ConfigAccess,
    runtime_did: &str,
    field: &str,
    value: serde_json::Value,
) {
    access
        .transact_write("test.set_node_field", |txn| {
            let value = value.clone();
            Box::pin(async move {
                let (_, mut node) = crate::read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Node,
                    runtime_did,
                    runtime_did,
                )
                .await?
                .expect("fixture user must own one Node for its runtime");
                node[field] = value;
                let plan =
                    crate::DesiredStateApplyPlan::new(vec![crate::DesiredStateApplyDocument {
                        collection: gents_store::collection::Collection::Node,
                        add: node.clone(),
                        update: node,
                    }])?;
                crate::apply_desired_state_plan(txn, &plan, &AdmitAllProfiles).await
            })
        })
        .await
        .unwrap();
}

pub fn test_agent_documents(
    owner: &str,
    display_name: &str,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
) -> Vec<(gents_store::collection::Collection, serde_json::Value)> {
    use gents_store::collection::Collection;
    use serde_json::json;
    let agent_did = &binding.agent_did;
    let context = format!("{agent_did}:context");
    let tools = format!("{agent_did}:tools");
    let profile = format!("{agent_did}:inference");
    let backend = format!("{agent_did}:backend");
    vec![
        (
            Collection::Agent,
            json!({"node_did":owner,"agent_did":agent_did,"creation_nonce":binding.creation_nonce,"display_name":display_name,"context_id":context,"inference_profile_id":profile}),
        ),
        (
            Collection::AgentContext,
            json!({"node_did":owner,"context_id":context,"tools_id":tools}),
        ),
        (
            Collection::Tools,
            json!({"node_did":owner,"tools_id":tools}),
        ),
        (
            Collection::InferenceProfile,
            json!({"node_did":owner,"profile_id":profile,"backend_id":backend,"model_name":"test-model"}),
        ),
        (
            Collection::InferenceBackend,
            json!({"node_did":owner,"backend_id":backend,"name":"Test inference","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:1/v1","auth":{"kind":"unauthenticated"}}),
        ),
    ]
}

/// Installs the fixture Agent's documents for `binding` through `access`'s
/// desired-state apply owner, admitting its profile with `profiles`.
pub async fn install_test_agent_as(
    access: &gents_store::config_client::ConfigAccess,
    owner: &str,
    display_name: &str,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    profiles: &dyn crate::inference_backend::AdvertisedProfilePolicy,
) {
    use crate::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    let documents = test_agent_documents(owner, display_name, binding)
        .into_iter()
        .map(|(collection, value)| DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        })
        .collect();
    let plan = DesiredStateApplyPlan::new(documents).unwrap();
    access
        .transact_write("test.install_agent", |txn| {
            let plan = &plan;
            Box::pin(async move { crate::apply_desired_state_plan(txn, plan, profiles).await })
        })
        .await
        .unwrap();
}

/// Provisions an Agent and installs its documents on `store` as `user`.
pub async fn create_agent_on(
    store: &gents_store::test_support::TestStore,
    user: &gents_store::identity::KeyIdentity,
    access: &gents_store::config_client::ConfigAccess,
    display_name: &str,
    profiles: &dyn crate::inference_backend::AdvertisedProfilePolicy,
) -> (
    std::sync::Arc<dyn gents_store::identity::NodeIdentity>,
    gents_protocol::agent_identity::AgentIdentityBinding,
) {
    use gents_store::identity::NodeIdentity as _;

    let runtime = store.node_identity.as_ref();
    let user_did = identity::Did::new(user.did()).unwrap();
    let binding =
        gents_store::identity::provision_agent_identity_authorized(&store.node, runtime, &user_did)
            .await
            .unwrap();
    install_test_agent_as(access, runtime.did(), display_name, &binding, profiles).await;
    let agent = gents_store::identity::resolve_agent_identity(runtime, &binding).unwrap();
    (agent, binding)
}

/// Desired-state fixtures shared by the documents tests and the tests of the
/// layers above that publish documents through the apply owner.
pub mod desired_state_fixtures {
    use std::sync::Arc;

    use anyhow::{Context, Result};
    use defra_node::Node as EmbeddedNode;
    use gents_store::collection::Collection;
    use gents_store::config_client::ConfigAccess;
    use serde_json::{json, Value};

    use crate::{
        apply_desired_state_plan, config_projection, DesiredStateApplyCounts,
        DesiredStateApplyDocument, DesiredStateApplyPlan,
    };

    pub use crate::desired_state::selection::{apply_selection_changes, Change};
    pub use crate::desired_state::task_selection::apply_task_selections;

    /// A canonical document whose required fields the fixture does not exercise
    /// hold placeholders; `fields` override them.
    pub fn canonical(
        collection: Collection,
        node_did: &str,
        fields: serde_json::Value,
    ) -> serde_json::Value {
        let mut document = match collection {
            Collection::Agent => {
                json!({"creation_nonce":"nonce", "inference_profile_id":"profile"})
            }
            Collection::InferenceProfile => json!({"backend_id":"backend", "model_name":"model"}),
            Collection::Task => json!({"prompt_template":"work"}),
            _ => json!({}),
        };
        document["node_did"] = json!(node_did);
        document[collection.unique_field()] = json!("fixture");
        for (field, value) in fields.as_object().expect("fixture fields") {
            document[field] = value.clone();
        }
        config_projection(collection, Some(&document))
            .expect("canonical fixture")
            .1
            .expect("canonical fixture projection")
    }

    pub fn backend(owner: &str, id: &str) -> Value {
        json!({"node_did":owner,"backend_id":id,"name":"Local","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:8000/v1","auth":{"kind":"unauthenticated"}})
    }

    pub fn document(value: Value) -> DesiredStateApplyDocument {
        DesiredStateApplyDocument {
            collection: Collection::InferenceBackend,
            add: value.clone(),
            update: value,
        }
    }

    pub fn config(collection: Collection, value: Value) -> DesiredStateApplyDocument {
        DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        }
    }

    pub async fn register_config_schemas(node: &EmbeddedNode) -> Result<()> {
        register_config_schemas_dropping_unique_indexes(node, &[]).await
    }

    pub async fn register_config_schemas_dropping_unique_indexes(
        node: &EmbeddedNode,
        duplicate_collections: &[Collection],
    ) -> Result<()> {
        gents_store::test_support::acp_policies::install_identity_acp_policies(node).await?;
        for (name, schema) in gents_protocol::schemas::RUNTIME_COLLECTION_NAMES
            .iter()
            .zip(gents_protocol::schemas::RUNTIME_ALL)
            .chain(
                gents_protocol::schemas::ALL_COLLECTION_NAMES
                    .iter()
                    .zip(gents_protocol::schemas::ALL),
            )
        {
            // Runtime-only schemas (the backend observation has no Collection variant)
            // are installed alongside the configuration collections, as in production.
            if gents_protocol::schemas::RUNTIME_COLLECTION_NAMES.contains(name)
                || Collection::ALL
                    .iter()
                    .any(|collection| collection.graphql_type() == *name)
            {
                let mut schema = (*schema).to_owned();
                for collection in duplicate_collections {
                    if collection.graphql_type() == *name {
                        schema = schema.replace(
                            &format!(
                                "@index(fields: [\"node_did\", \"{}\"], unique: true)",
                                collection.unique_field()
                            ),
                            "",
                        );
                    }
                }
                node.add_collection(
                    &node.node_context(),
                    &gents_protocol::schemas::HomePolicyIds::node_store().render(&schema),
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Configuration writes are signed by the fixture node's own identity. Anonymous
    /// access cannot complete the relationship grants that Node and configuration
    /// publication queue, so it fails with `RelationshipActorRequired`.
    pub fn runtime_access(node: &Arc<EmbeddedNode>) -> Result<ConfigAccess> {
        let runtime = node
            .node_identity_did()
            .context("fixture node has no node identity")?;
        Ok(ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(runtime)?,
        ))
    }

    pub async fn apply(access: &ConfigAccess, docs: Vec<DesiredStateApplyDocument>) -> Result<()> {
        apply_with_counts(access, docs).await.map(|_| ())
    }

    /// Configuration writes require the owner's Node document. Publishes it for
    /// each owner the plan writes to that has none yet.
    pub async fn ensure_owner_nodes(
        access: &ConfigAccess,
        docs: &[DesiredStateApplyDocument],
    ) -> Result<()> {
        let owners: std::collections::BTreeSet<&str> = docs
            .iter()
            .filter(|document| !matches!(document.collection, Collection::Node))
            .filter_map(|document| document.add["node_did"].as_str())
            .collect();
        for owner in owners {
            let response = access
                .execute(&format!(
                    "{{ Node(filter: {{node_did: {{_eq: \"{}\"}}}}) {{ _docID }} }}",
                    gents_store::graphql::escape_graphql_string(owner)
                ))
                .await?;
            if response["data"]["Node"]
                .as_array()
                .is_some_and(Vec::is_empty)
            {
                super::bootstrap_signed_test_node(access, owner).await;
            }
        }
        Ok(())
    }

    pub async fn apply_with_counts(
        access: &ConfigAccess,
        docs: Vec<DesiredStateApplyDocument>,
    ) -> Result<DesiredStateApplyCounts> {
        ensure_owner_nodes(access, &docs).await?;
        let plan = DesiredStateApplyPlan::new(docs)?;
        access
            .transact_write("test.desired.apply", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan, &super::AdmitAllProfiles).await
                })
            })
            .await
    }
}

#[cfg(test)]
mod ensure_test_node_tests {
    use std::sync::Arc;

    use gents_store::node_extension::Extensions;
    use gents_store::test_support::TestStore;
    use tokio::sync::Barrier;

    #[tokio::test]
    async fn concurrent_ensure_test_node_calls_succeed_with_one_node_doc() {
        let store = TestStore::start("ensure-test-node-concurrent", Extensions::none()).await;
        let node_did = store.node_identity.did().to_owned();
        let callers = 8usize;
        let barrier = Arc::new(Barrier::new(callers));
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..callers {
            let node = store.node.clone();
            let node_did = node_did.clone();
            let barrier = barrier.clone();
            tasks.spawn(async move {
                barrier.wait().await;
                super::ensure_test_node(&node, &node_did).await;
                crate::principal::load_node_record(&node, &node_did)
                    .await
                    .unwrap()
                    .expect("Node must exist after ensure_test_node")
                    .0
            });
        }
        let mut observed_doc_ids = Vec::with_capacity(callers);
        while let Some(result) = tasks.join_next().await {
            observed_doc_ids.push(result.expect("ensure_test_node task must not panic"));
        }
        assert_eq!(observed_doc_ids.len(), callers);
        let first = &observed_doc_ids[0];
        assert!(
            observed_doc_ids.iter().all(|doc_id| doc_id == first),
            "all concurrent callers must observe the identical Node doc ID: {observed_doc_ids:?}"
        );

        let access = gents_store::config_client::ConfigAccess::local_as(
            store.node.clone(),
            identity::Did::new(&node_did).unwrap(),
        );
        let rendered = {
            use gents_protocol::document_query::{
                fields::node as fields, Filter, Query, Selection,
            };
            Query::select(Selection::doc_id())
                .filter(Filter::eq(fields::NODE_DID, &node_did))
                .render()
        };
        let response = access
            .transact_readonly("test.ensure_node.count", |txn| {
                let rendered = &rendered;
                Box::pin(async move { txn.run(rendered).await })
            })
            .await
            .unwrap();
        let rows = gents_protocol::graphql::graphql_rows_from_response(&response, "Node");
        assert_eq!(rows.len(), 1, "expected exactly one Node row: {rows:?}");
    }

    #[tokio::test]
    async fn ensure_test_node_tolerates_external_concurrent_node_writer() {
        let store = TestStore::start("ensure-test-node-external-race", Extensions::none()).await;
        let node_did = store.node_identity.did().to_owned();
        let barrier = Arc::new(Barrier::new(2));

        let ensure_task = {
            let node = store.node.clone();
            let node_did = node_did.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                super::ensure_test_node(&node, &node_did).await;
                crate::principal::load_node_record(&node, &node_did)
                    .await
                    .unwrap()
                    .expect("Node must exist after ensure_test_node")
                    .0
            })
        };

        // Direct desired-state write bypassing `ensure_test_node`'s per-node mutex.
        let external_task = {
            let node = store.node.clone();
            let node_did = node_did.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                let access = gents_store::config_client::ConfigAccess::local_as(
                    node.clone(),
                    identity::Did::new(&node_did).unwrap(),
                );
                barrier.wait().await;
                let _ = super::create_test_node_if_absent(&access, &node_did).await;
                crate::principal::load_node_record(&node, &node_did)
                    .await
                    .unwrap()
                    .expect("Node must exist after external write")
                    .0
            })
        };

        let ensure_doc = ensure_task
            .await
            .expect("ensure_test_node must not panic under external writer race");
        let external_doc = external_task.await.expect("external writer task");
        assert_eq!(ensure_doc, external_doc);
    }
}
