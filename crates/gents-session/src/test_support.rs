pub mod lineage_fixtures;

/// An explicitly empty set of lifecycle plugins, for a test below the bundled
/// plugins whose code reaches no lifecycle point. A test that reaches one
/// runs core alone, which no production node does.
pub fn no_lifecycle_plugins() -> gents_store::node_extension::Extensions {
    gents_store::node_extension::Extensions::of(std::sync::Arc::new(
        crate::request::hooks::registry::Registry::default(),
    ))
}

use crate::request::watcher::DefraWatcher;

pub struct ScopedWatcherFixture {
    pub node: Arc<defra_node::Node>,
    pub node_did: String,
    pub agent: Arc<dyn gents_store::identity::NodeIdentity>,
    pub access: gents_store::config_client::ConfigAccess,
    pub client_access: gents_store::config_client::ConfigAccess,
    pub _home: gents_inference::test_support::TestDb,
}

impl ScopedWatcherFixture {
    pub async fn new() -> Self {
        use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
        use gents_store::identity::NodeIdentity;

        let db = gents_inference::test_support::test_db(
            "scoped-watcher",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
        let (home, user, client_access) =
            gents_inference::test_support::signed_test_node_without_agent_from_db(
                db,
                "scoped-watcher-user",
            )
            .await;
        let node = home.node.clone();
        let runtime = home.node_identity.clone();
        let binding = gents_store::identity::provision_agent_identity_authorized(
            &node,
            runtime.as_ref(),
            &identity::Did::new(user.did()).unwrap(),
        )
        .await
        .unwrap();
        let profile_id = format!("{}:inference", binding.agent_did);
        let backend_id = format!("{}:backend", binding.agent_did);
        let plan = DesiredStateApplyPlan::new(
            [
                (
                    gents_store::collection::Collection::InferenceBackend,
                    serde_json::json!({
                        "node_did": runtime.did(),
                        "backend_id": backend_id,
                        "name": "Scoped watcher inference",
                        "provider_kind": "OpenAiCompatible",
                        "endpoint": "http://127.0.0.1:1/v1",
                        "auth": {"kind": "unauthenticated"},
                    }),
                ),
                (
                    gents_store::collection::Collection::InferenceProfile,
                    serde_json::json!({
                        "node_did": runtime.did(),
                        "profile_id": profile_id,
                        "backend_id": backend_id,
                        "model_name": "test-model",
                    }),
                ),
                (
                    gents_store::collection::Collection::Agent,
                    serde_json::json!({
                        "node_did": runtime.did(),
                        "agent_did": binding.agent_did,
                        "creation_nonce": binding.creation_nonce,
                        "display_name": "Scoped watcher agent",
                        "inference_profile_id": profile_id,
                        "enabled": true,
                    }),
                ),
            ]
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
        )
        .unwrap();
        client_access
            .transact_write("test.scoped_watcher.agent", |txn| {
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
        // Production projects the AgentBaseline from each live Agent document
        // (tool permission projection); this fixture has no projection pass, so
        // it grants the same set as the node admin, as the other Agent fixtures do.
        let admin = gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(runtime.did()).unwrap(),
        );
        gents_store::identity::GrantSet::AgentBaseline
            .grant(&admin, &identity::Did::new(&binding.agent_did).unwrap())
            .await
            .unwrap();
        let agent =
            gents_store::identity::resolve_agent_identity(runtime.as_ref(), &binding).unwrap();
        let access = gents_store::config_client::ConfigAccess::local_managed_agent(
            node.clone(),
            agent.did().parse().unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        Self {
            node,
            node_did: runtime.did().to_owned(),
            agent,
            access,
            client_access,
            _home: home,
        }
    }

    pub fn watcher(&self) -> DefraWatcher {
        DefraWatcher::for_agents(self.node.clone(), &self.node_did, vec![self.agent.clone()])
            .expect("selected-agent request watcher")
    }

    /// Creates the request as its requesting User through the signed client
    /// path, then grants the selected Agent `working_agent`. An Agent holds
    /// no create right on `delivered_requests`, so it cannot create its own rows.
    pub async fn create_request(&self, input: serde_json::Value) -> String {
        let agent = identity::Did::new(self.agent.did()).unwrap();
        self.client_access
            .transact_write("test.watcher.request", |txn| {
                let input = &input;
                let agent = &agent;
                Box::pin(async move {
                    let created = txn
                        .execute_client_signed_create(
                            "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                            &serde_json::json!({"input": input}),
                            gents_protocol::schemas::AGENT_REQUEST,
                        )
                        .await?;
                    let doc_id = gents_store::graphql::created_doc_id(&created, "AgentRequest")?;
                    txn.add_actor_relationship("AgentRequest", &doc_id, "working_agent", agent)
                        .await?;
                    Ok::<_, anyhow::Error>(doc_id)
                })
            })
            .await
            .expect("create watcher request for selected agent")
    }

    pub fn request_input(&self, request_id: &str, session_id: &str) -> serde_json::Value {
        serde_json::json!({
            "request_id": request_id,
            "purpose": "normal",
            "node_did": self.node_did,
            "agent_did": self.agent.did(),
            "session_id": session_id,
            "content": "test",
            "lifecycle_state": "pending",
            "execution_origin": "interactive",
            "created_at": "2026-03-12T00:00:01Z",
            "retry_count": 0,
            "max_retries": 0,
        })
    }

    pub async fn insert_request(
        &self,
        request_id: &str,
        session_id: &str,
        lifecycle_state: &str,
        created_at: &str,
    ) -> String {
        let mut input = self.request_input(request_id, session_id);
        input["lifecycle_state"] = serde_json::json!(lifecycle_state);
        input["created_at"] = serde_json::json!(created_at);
        self.create_request(input).await
    }

    pub async fn insert_incoherent_request(&self, request_id: &str, session_id: &str) -> String {
        let mut input = self.request_input(request_id, session_id);
        input["request_hop"] = serde_json::json!(1);
        input["caused_by_parent_request_id"] = serde_json::json!("parent-req-exists");
        self.create_request(input).await
    }

    pub async fn update_request(&self, doc_id: &str, input: serde_json::Value) {
        let doc = gents_store::graphql::escape_graphql_string(doc_id);
        let input = gents_protocol::graphql::graphql_input_literal(&input).unwrap();
        self.access
            .write(
                "test.watcher.update",
                &format!(
                    r#"mutation {{ update_AgentRequest(docID: "{doc}", input: {input}) {{ _docID }} }}"#
                ),
            )
            .await
            .expect("update watcher request as selected agent");
    }

    pub async fn set_completed(&self, doc_id: &str) {
        self.update_request(
            doc_id,
            serde_json::json!({ "lifecycle_state": "completed" }),
        )
        .await;
    }

    pub async fn request_row(&self, doc_id: &str) -> serde_json::Value {
        let doc = gents_store::graphql::escape_graphql_string(doc_id);
        let response = self.access.execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }} }}, limit: 2) {{ lifecycle_state failure_reason claimed_at terminalized_at terminal_redrive_attempts }} }}"#
        )).await.unwrap();
        let rows = response["data"]["AgentRequest"]
            .as_array()
            .unwrap_or_else(|| panic!("selected agent must read request: {response}"));
        let [row] = rows.as_slice() else {
            panic!("selected agent must see one request: {response}");
        };
        row.clone()
    }
}

use crate::session::{output, SequencedMessage};
use gents_inference::llm::message::Message;

use crate::request::lifecycle::{ClaimOutcome, RequestLifecycle};
use crate::request::streaming::DefraStreamWriter;
use defra_node::Node as EmbeddedNode;
use std::{sync::Arc, time::Duration};

pub struct FixtureGuard {
    pub _db: gents_inference::test_support::TestDb,
    pub owner: gents_store::config_client::ConfigAccess,
}

pub fn fixture_access(
    node: &Arc<EmbeddedNode>,
    lifecycle: &RequestLifecycle,
) -> gents_store::config_client::ConfigAccess {
    gents_store::config_client::ConfigAccess::local_with_agent_signer(
        node.clone(),
        lifecycle.actor_did().unwrap(),
        lifecycle.managed_agent_signer().unwrap(),
    )
    .unwrap()
}

pub async fn fixture_with_ids(
    name: &str,
    request_id: &str,
    session_id: &str,
) -> (
    Arc<EmbeddedNode>,
    FixtureGuard,
    RequestLifecycle,
    DefraStreamWriter,
) {
    use gents_store::identity::NodeIdentity;
    let (db, user, owner, binding) = gents_inference::test_support::signed_test_node(
        name,
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = db.node.clone();
    let agent =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding).unwrap();
    let create = crate::request::lifecycle::build_signed_client_request(
        &owner,
        crate::request::lifecycle::RequestSpec::new(
            gents_protocol::request_admission::RequestPurpose::Normal,
            crate::request::lifecycle::RequestIdentity {
                request_id: request_id.into(),
                node_did: db.node_identity.did().into(),
                requester_did: Some(user.did().into()),
                agent_did: agent.did().into(),
                session_id: session_id.into(),
                content: "hello".into(),
                execution_origin: crate::request::lifecycle::ExecutionOrigin::Interactive,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
            gents_protocol::request_admission::AgentRequestAdmissionRecord::client(user.did()),
        ),
        crate::request::lifecycle::RequestSigner::Identity(&user),
    )
    .await
    .unwrap();
    crate::request::lifecycle::submit_client_request(&owner, &create)
        .await
        .unwrap();
    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        agent.did().parse().unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let response = access
        .execute(&format!(
            "{{ AgentRequest {{ {} }} }}",
            gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
        ))
        .await
        .unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    let request = row.try_into().unwrap();
    let (_authority_owner, authority) = gents_p2p::p2p_reconcile::enrollment_authority_channel();
    let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
        node.clone(),
        db.node_identity.clone(),
        authority,
    );
    let (request, admission) = verifier
        .verify_fresh_for_claim(&request, agent.did())
        .await
        .unwrap();
    let mut lifecycle = RequestLifecycle::new_with_node_did(node.clone(), request, 60);
    lifecycle.bind_managed_agent(agent.as_ref()).unwrap();
    lifecycle.set_client_claim_admission(admission);
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    let writer =
        DefraStreamWriter::new_managed(node.clone(), agent.as_ref(), Duration::from_millis(1))
            .unwrap();
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    (node, FixtureGuard { _db: db, owner }, lifecycle, writer)
}

pub async fn fixture_query(
    access: &gents_store::config_client::ConfigAccess,
    document: &str,
) -> defra_node::QueryResponse {
    access
        .transact_readonly("test.canonical_stream.read", |txn| {
            Box::pin(async move { txn.execute_local_response(document).await })
        })
        .await
        .unwrap()
}

pub async fn fixture(
    name: &str,
) -> (
    Arc<EmbeddedNode>,
    FixtureGuard,
    RequestLifecycle,
    DefraStreamWriter,
) {
    fixture_with_ids(name, &format!("request-{name}"), &format!("session-{name}")).await
}

use gents_store::config_client::ConfigAccess;
use gents_store::identity::{KeyIdentity, NodeIdentity};

pub async fn publish_provider_message(
    writer: &crate::request::streaming::DefraStreamWriter,
    request: &crate::request::lifecycle::RequestLifecycle,
    turn: usize,
    message: &gents_protocol::message::Message,
) -> anyhow::Result<String> {
    writer
        .start_provider_attempt(
            &request.request().doc_id,
            turn,
            0,
            format!("inference.{}", turn + 1).parse()?,
        )
        .await;
    Ok(writer
        .publish_native_turn(request, turn, 0, message)
        .await?
        .message_doc_id)
}

pub async fn admitted_client_lifecycle(
    node: std::sync::Arc<defra_node::Node>,
    runtime: std::sync::Arc<dyn gents_store::identity::NodeIdentity>,
    request: &gents_protocol::row::AgentRequest,
    agent: &dyn gents_store::identity::NodeIdentity,
) -> anyhow::Result<crate::request::lifecycle::RequestLifecycle> {
    let (_owner, authority) = gents_p2p::p2p_reconcile::enrollment_authority_channel();
    let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
        node.clone(),
        runtime,
        authority,
    );
    let (request, admission) = verifier
        .verify_fresh_for_claim_with_identity(request, agent)
        .await?;
    let mut lifecycle =
        crate::request::lifecycle::RequestLifecycle::new_with_node_did(node, request, 60);
    lifecycle.bind_managed_agent(agent)?;
    lifecycle.set_client_claim_admission(admission);
    Ok(lifecycle)
}

/// Create an Agent-owned session as that Agent and publish its Agent
/// relationships in the same transaction, so requests and Goals may bind it.
pub async fn create_agent_owned_session(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> String {
    create_agent_owned_session_at(
        access,
        node_did,
        session_id,
        requester_did,
        "2020-01-01T00:00:00Z",
    )
    .await
}

/// `created_at` is immutable on AgentSession, so a fixture that needs a
/// particular creation time supplies it here rather than updating the row.
pub async fn create_agent_owned_session_at(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    created_at: &str,
) -> String {
    let agent_did = access
        .actor_did()
        .expect("an Agent-owned session needs its Agent actor");
    access
        .transact_write("test.agent_owned_session", |txn| {
            let agent_did = &agent_did;
            Box::pin(async move {
                crate::session::ensure_session_in_txn(
                    txn,
                    session_id,
                    node_did,
                    agent_did,
                    requester_did,
                    None,
                    None,
                    created_at,
                )
                .await?;
                let session =
                    gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                        txn, node_did, session_id,
                    )
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("created session is not visible"))?;
                let (agent_doc, _) = gents_documents::read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Agent,
                    node_did,
                    agent_did,
                )
                .await?
                .ok_or_else(|| anyhow::anyhow!("session Agent is not visible"))?;
                crate::session::stage_created_session_relationships_in_txn(
                    txn,
                    &session.doc_id,
                    &agent_doc,
                    &identity::Did::new(agent_did)?,
                )
                .await?;
                Ok::<_, anyhow::Error>(session.doc_id)
            })
        })
        .await
        .unwrap()
}

/// Makes the session owners' warnings visible to a scoped subscriber: tracing
/// caches callsite interest globally, so a scoped subscriber sees an event only
/// when a global default has enabled its target first.
pub fn enable_scoped_event_capture() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        tracing::subscriber::set_global_default(CapturedTargetSubscriber)
            .expect("no other global tracing default in this test binary");
    });
}

struct CapturedTargetSubscriber;

impl tracing::Subscriber for CapturedTargetSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target().starts_with("gents_session::")
            || metadata.target() == crate::session::REFUSAL_PROJECTION_EVENT_TARGET
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::Id {
        tracing::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::Id, _follows: &tracing::Id) {}

    fn event(&self, _event: &tracing::Event<'_>) {}

    fn enter(&self, _span: &tracing::Id) {}

    fn exit(&self, _span: &tracing::Id) {}
}

pub async fn seed_owned_agent(
    access: &ConfigAccess,
    runtime_did: &str,
    agent: &gents_protocol::agent_identity::AgentIdentityBinding,
) {
    access.transact_write("test.client.owned_agent", |txn| Box::pin(async move {
        let node = txn.execute_client_signed_create(
            "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
            &serde_json::json!({"input":{"node_did":runtime_did}}),
            gents_protocol::schemas::NODE,
        ).await?;
        let node = gents_store::graphql::created_doc_id(&node, "Node")?;
        let created = txn.execute_client_signed_create(
            "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
            &serde_json::json!({"input":{"node_did":runtime_did,"agent_did":agent.agent_did,"creation_nonce":agent.creation_nonce,"inference_profile_id":"fixture","enabled":true}}),
            gents_protocol::schemas::AGENT,
        ).await?;
        let agent_doc = gents_store::graphql::created_doc_id(&created, "Agent")?;
        txn.add_actor_relationship("Node", &node, "runtime_reader", &identity::Did::new(runtime_did)?).await?;
        txn.add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node).await?;
        txn.add_actor_relationship("Agent", &agent_doc, "executor", &identity::Did::new(&agent.agent_did)?).await?;
        Ok::<_, anyhow::Error>(())
    })).await.unwrap();
}

/// A node served over HTTP through the protected start, with the runtime
/// identity it runs as.
pub async fn serve_client_node(
    name: &str,
    composition: gents_store::node_extension::Extensions,
) -> (gents_inference::test_support::TestDb, KeyIdentity) {
    let db = gents_inference::test_support::test_db(name, composition).await;
    let runtime = gents_store::test_support::held_runtime_identity(db.node_identity.did());
    (db, runtime)
}

/// Grants the user the `OperatorUser` set as the runtime admin, as
/// `signed_test_node` does. Without it, seeding the user's client-signed
/// Node and Agent ends in an incomplete signing conversation.
pub async fn grant_operator_user(
    node: &Arc<defra_node::Node>,
    runtime: &dyn NodeIdentity,
    user: &KeyIdentity,
) {
    let admin = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did()).unwrap());
    gents_store::identity::GrantSet::OperatorUser
        .grant(&admin, &identity::Did::new(user.did()).unwrap())
        .await
        .unwrap();
}

pub async fn sequenced_history_on(
    access: &gents_store::config_client::ConfigAccess,
    session_id: &str,
    node_did: &str,
    requester: Option<&str>,
    after: Option<u32>,
    excluded_request: Option<&str>,
) -> anyhow::Result<Vec<SequencedMessage>> {
    output::sequenced::load_sequenced_messages_using(
        output::access::ReadAccess::Config(access),
        session_id,
        node_did,
        requester,
        None,
        after,
        excluded_request,
        None,
    )
    .await
}

pub async fn history_on(
    access: &gents_store::config_client::ConfigAccess,
    session_id: &str,
    node_did: &str,
    requester: Option<&str>,
    excluded_request: Option<&str>,
) -> anyhow::Result<Vec<Message>> {
    Ok(sequenced_history_on(
        access,
        session_id,
        node_did,
        requester,
        None,
        excluded_request,
    )
    .await?
    .into_iter()
    .map(|row| row.message)
    .collect())
}

pub async fn session_rewrite_rows(
    access: &gents_store::config_client::ConfigAccess,
) -> Vec<Vec<String>> {
    let response = access
        .execute(
            "{ AgentSession { _docID session_id agent_did } \
           AgentMessage { _docID session_id sequence message_key } \
           CompactionEntry { _docID session_id sequence compaction_key summary } }",
        )
        .await
        .unwrap();
    let data = &response["data"];
    ["AgentSession", "AgentMessage", "CompactionEntry"]
        .into_iter()
        .map(|collection| {
            let mut rows = data[collection]
                .as_array()
                .expect("collection rows")
                .iter()
                .map(|row| row.to_string())
                .collect::<Vec<_>>();
            rows.sort();
            rows
        })
        .collect()
}

/// The shared fixture installs its Agent as the non-admin user, which the
/// native source projection refuses; this fixture installs it as the node.
pub async fn fork_reader_node(
    name: &str,
) -> (
    gents_inference::test_support::TestDb,
    gents_store::identity::KeyIdentity,
    gents_store::config_client::ConfigAccess,
    gents_protocol::agent_identity::AgentIdentityBinding,
) {
    use gents_store::config_client::NodePermission;
    use gents_store::identity::NodeIdentity;
    let (db, user, client) = gents_inference::test_support::signed_test_node_without_agent(
        name,
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let runtime = db.node_identity.as_ref();
    let admin = gents_store::config_client::ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(runtime.did()).unwrap(),
    );
    let user_did = identity::Did::new(user.did()).unwrap();
    admin
        .grant_node_permission(&user_did, NodePermission::DacRelationDelete)
        .await
        .unwrap();
    let binding =
        gents_store::identity::provision_agent_identity_authorized(&db.node, runtime, &user_did)
            .await
            .unwrap();
    gents_inference::test_support::install_test_agent_binding_with_access(
        runtime.did(),
        "Default fixture agent",
        &binding,
        &admin,
    )
    .await;
    gents_documents::test_support::set_test_default_agent_as(
        &client,
        runtime.did(),
        &binding.agent_did,
    )
    .await;
    (db, user, client, binding)
}

pub async fn assert_block_signer(node: &defra_node::Node, cid: &str, signer: &str) {
    let (block, signature) = node
        .authorized_signed_block_bytes(cid, Some(signer))
        .await
        .unwrap();
    let block = defra_core::block::Block::from_dag_cbor(&block).unwrap();
    let signature = defra_core::block::Signature::from_dag_cbor(&signature).unwrap();
    assert_eq!(
        db::block::verify::verified_signature_signer_did(&block, &signature).unwrap(),
        signer
    );
}

pub async fn test_p2p_db(name: &str) -> gents_inference::test_support::TestDb {
    let db = gents_inference::test_support::embedded::test_db_with_p2p(
        name,
        crate::test_support::no_lifecycle_plugins(),
        |path| gents_p2p::test_support::transport::test_p2p_config(&Default::default(), path),
    )
    .await;
    let (db, _user, _access) =
        gents_inference::test_support::signed_test_node_without_agent_from_db(db, name).await;
    db
}
