use std::sync::Arc;

use gents::defra_node::{Node as EmbeddedNode, QueryResponse};
use gents_protocol::row::AgentRequest;
use gents_store::identity::NodeIdentity;
use gents_store::test_support::TestStore;
use serde::Deserialize;

pub mod accepted_turn;
pub mod agent_requests;
pub mod agent_transcript;
pub mod builder_runtime;
pub mod commit_race_proxy;

pub mod fixtures;
pub mod interrupt;
pub mod live_inference;
pub mod ports;

pub mod signed_mailbox;
pub mod snapshots;

/// A test's `#[tokio::test]` runtime is its entry point; constructors take
/// that runtime's handle from here.
#[allow(
    clippy::disallowed_methods,
    reason = "a test's #[tokio::test] runtime is its entry-point runtime"
)]
pub fn runtime_handle() -> tokio::runtime::Handle {
    tokio::runtime::Handle::current()
}

pub const NODE_DID: &str = "did:test:test";
pub const AGENT_NAME: &str = "test";
pub const BACKEND_ID: &str = "backend-test";
pub const DEADLINE_SECS: u64 = 300;

/// Installs a test-writer subscriber when `RUST_LOG` names a filter, so owner
/// diagnostics are visible for a failing test; absent the variable it is inert.
pub fn init_test_tracing() {
    if let Ok(filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_test_writer()
            .try_init();
    }
}

pub struct ManagedAgentFixture {
    pub user: gents_store::identity::KeyIdentity,
    pub access: gents_store::config_client::ConfigAccess,
    pub agent: Arc<dyn NodeIdentity>,
    pub binding: gents_protocol::agent_identity::AgentIdentityBinding,
}

pub async fn managed_fixture_user(db: &TestStore) -> gents_store::identity::KeyIdentity {
    let path = db.path().join("fixture-user.key");
    tokio::task::spawn_blocking(move || {
        gents_store::identity::KeyIdentity::load_or_create(path, None)
    })
    .await
    .expect("join held test user setup")
    .expect("create held test user")
}

pub async fn load_managed_fixture_user(db: &TestStore) -> gents_store::identity::KeyIdentity {
    let path = db.path().join("fixture-user.key");
    tokio::task::spawn_blocking(move || {
        gents_store::identity::KeyIdentity::load_existing(path, None)
    })
    .await
    .expect("join retained test user load")
    .expect("load retained test user")
}

pub async fn managed_agent_fixture(db: &TestStore) -> ManagedAgentFixture {
    let user = managed_fixture_user(db).await;
    let runtime = db.node_identity.as_ref();
    assert_ne!(user.did(), runtime.did());
    let access = db.client_access(&user);
    // An enrolled User's grants bound what its Agent creation may project.
    gents_documents::test_support::enroll_test_user(&db.node, runtime, &user, &access).await;
    let (agent, binding) = install_managed_agent(db, &access, &user, "Test agent").await;
    ManagedAgentFixture {
        user,
        access,
        agent,
        binding,
    }
}

/// Makes the fixture User the household owner `gents init` creates: the native
/// admin that installs packs and their collection schemas.
pub async fn grant_household_owner(db: &TestStore, user: &gents_store::identity::KeyIdentity) {
    let runtime = db.node_identity.as_ref();
    let admin = gents_store::config_client::ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(runtime.did()).expect("valid runtime DID"),
    );
    gents_store::identity::GrantSet::HouseholdOwner
        .grant(
            &admin,
            &identity::Did::new(user.did()).expect("valid user DID"),
        )
        .await
        .expect("grant fixture User household ownership");
}

/// The configuring User lets `caller` open sessions on `target`'s Agent (the
/// `session_writer` relation behind `invoke`), as delegation to another Agent needs.
pub async fn grant_agent_session_writer(
    db: &TestStore,
    access: &gents_store::config_client::ConfigAccess,
    target_agent_did: &str,
    caller_agent_did: &str,
) {
    let node_did = db.node_identity.did();
    let target_doc = access
        .transact_readonly("test.agent_session_writer.read", |txn| {
            Box::pin(async move {
                Ok::<_, anyhow::Error>(
                    gents_documents::read_desired_state_record_in_txn(
                        txn,
                        gents_store::collection::Collection::Agent,
                        node_did,
                        target_agent_did,
                    )
                    .await?
                    .expect("target Agent")
                    .0,
                )
            })
        })
        .await
        .expect("read target Agent");
    access
        .add_actor_relationship(
            "Agent",
            &target_doc,
            "session_writer",
            &identity::Did::new(caller_agent_did).expect("valid caller DID"),
        )
        .await
        .expect("grant caller session_writer on target Agent");
}

pub async fn install_managed_agent(
    db: &TestStore,
    access: &gents_store::config_client::ConfigAccess,
    user: &gents_store::identity::KeyIdentity,
    display_name: &str,
) -> (
    Arc<dyn NodeIdentity>,
    gents_protocol::agent_identity::AgentIdentityBinding,
) {
    gents_documents::test_support::create_agent_on(
        db,
        user,
        access,
        display_name,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
}

/// A [`TestStore`](gents_store::test_support::TestStore) projecting what a
/// trial home's node projects: the meta-only tool ceiling and the lifecycle
/// hook composition.
pub async fn trial_store(name: &str) -> TestStore {
    init_test_tracing();
    TestStore::start_with_tool_policy(
        name,
        gents_automation::lifecycle_hooks::composition(),
        Arc::new(gents_tools::surface::ToolCeiling::meta_only()),
    )
    .await
}

/// A [`trial_store`] over the home at `path`, retained after the node stops,
/// so the trial's own home owner can reopen it.
pub async fn trial_store_at(path: &std::path::Path) -> TestStore {
    init_test_tracing();
    TestStore::open_at_with_tool_policy(
        path,
        gents_automation::lifecycle_hooks::composition(),
        Arc::new(gents_tools::surface::ToolCeiling::meta_only()),
    )
    .await
    .expect("trial store")
}

pub async fn test_p2p_db(name: &str) -> TestStore {
    test_p2p_db_with_admission(name, TestP2pAdmission::default()).await
}

pub async fn test_p2p_db_with_admission(name: &str, admission: TestP2pAdmission) -> TestStore {
    let db = test_p2p_db_without_node(name, admission).await;
    install_runtime_node(&db).await;
    db
}

/// A P2P-enabled store whose Node document its enrolling User installs, as
/// `managed_agent_fixture` does.
pub async fn test_p2p_db_without_node(name: &str, admission: TestP2pAdmission) -> TestStore {
    init_test_tracing();
    TestStore::start_configured_with_tool_policy(
        name,
        gents_automation::lifecycle_hooks::composition(),
        Arc::new(move |path, builder| builder.with_p2p(test_p2p_config(&admission, path))),
        Arc::new(gents_tools::surface::ToolCeiling::meta_only()),
    )
    .await
}

/// Enrols the fixture User and writes the runtime's Node document as that
/// User, as `gents init` does: the Node is User-configured, never authored by the
/// runtime it describes. Publication and enrollment owners require exactly one
/// visible local Node before they publish, and later fixture configuration by
/// the same User updates this document instead of racing a second owner.
async fn install_runtime_node(db: &TestStore) {
    let user = managed_fixture_user(db).await;
    gents_documents::test_support::enroll_test_user(
        &db.node,
        db.node_identity.as_ref(),
        &user,
        &db.client_access(&user),
    )
    .await;
}

pub fn build_request(
    doc_id: String,
    request_id: String,
    session_id: String,
    created_at: String,
) -> AgentRequest {
    AgentRequest {
        retry_parent_request_doc_id: None,
        purpose: gents_protocol::request_admission::RequestPurpose::Normal,
        doc_id,
        request_id,
        node_did: NODE_DID.into(),
        requester_did: None,
        agent_did: AGENT_NAME.into(),
        session_id,
        content: "hello".into(),
        max_total_tokens: None,
        input: Default::default(),
        execution_origin: None,
        created_at,
        deadline: None,
        execution_generation: None,
        execution_lease_secs: None,
        execution_lease_expires_at: None,
        request_hop: 0,
        interrupt_generation: None,
        caused_by_parent_request_id: None,
        caused_by_parent_request_doc_id: None,
        caused_by_parent_tool_call_id: None,
        caused_by_parent_tool_call_doc_id: None,
        caused_by_trigger_id: None,
        caused_by_trigger_kind: None,
        caused_by_source_doc_id: None,
        caused_by_correlation: None,
        caused_by_trigger_context: None,
        workspace_id: None,
        workspace_authority: None,
        workspace_owner_node_did: None,
        workspace_seal_hash: None,
        executed_model_name: None,
        executed_reasoning_effort: None,
        executed_service_tier: None,
        executed_inference_profile_id: None,
        executed_runtime_generation: None,
    }
}

pub async fn create_session_document(
    node: &EmbeddedNode,
    session: &gents_protocol::session::AgentSession,
) {
    let input = gents_protocol::graphql::graphql_input_literal(
        &serde_json::to_value(session).expect("serialize canonical AgentSession"),
    )
    .expect("render session fixture");
    let response = node
        .execute(&format!(
            "mutation {{ create_AgentSession(input: {input}) {{ _docID }} }}"
        ))
        .await;
    assert!(
        !response.has_errors(),
        "create canonical session: {:?}",
        response.errors
    );
}

pub fn session_document(
    session_id: &str,
    agent_did: &str,
    created_at: &str,
) -> gents_protocol::session::AgentSession {
    gents_protocol::session::AgentSession {
        session_id: session_id.into(),
        node_did: NODE_DID.into(),
        requester_did: None,
        agent_did: agent_did.into(),
        created_at: created_at.into(),
        closed_at: None,
        archived_at: None,
        archive_revision: 0,
        stopped_at: None,
        title: None,
        tags: vec![],
        provenance: None,
        observation: None,
    }
}

pub fn first_row<T>(resp: &QueryResponse, key: &str) -> T
where
    T: for<'de> Deserialize<'de>,
{
    assert!(!resp.has_errors(), "query failed: {:?}", resp.errors);
    let value = resp
        .data
        .as_ref()
        .and_then(|data| data.get(key))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .unwrap_or_else(|| panic!("missing row for {key}"));
    serde_json::from_value(value).unwrap_or_else(|err| panic!("decode {key} failed: {err}"))
}

pub fn first_optional_row<T>(resp: &QueryResponse, key: &str) -> Option<T>
where
    T: for<'de> Deserialize<'de>,
{
    assert!(!resp.has_errors(), "query failed: {:?}", resp.errors);
    resp.data
        .as_ref()
        .and_then(|data| data.get(key))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .map(|value| {
            serde_json::from_value(value).unwrap_or_else(|err| panic!("decode {key} failed: {err}"))
        })
}

pub async fn begin_owned_execution(
    lifecycle: &mut gents_session::request::lifecycle::RequestLifecycle,
    node: &std::sync::Arc<gents::defra_node::Node>,
) -> anyhow::Result<String> {
    let writer = gents_session::request::streaming::DefraStreamWriter::new(
        node.clone(),
        &lifecycle.request().node_did,
        std::time::Duration::ZERO,
    );
    lifecycle.begin_owned_execution(&writer).await?;
    Ok(lifecycle.request().doc_id.clone())
}

use gents_p2p::test_support::transport::{test_p2p_config, TestP2pAdmission};
