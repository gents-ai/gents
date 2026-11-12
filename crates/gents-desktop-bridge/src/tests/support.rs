use std::sync::Arc;

use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use gents_protocol::job::JobProgress;
use gents_store::graphql::escape_graphql_string;
use tempfile::TempDir;

/// A test's `#[tokio::test]` runtime is its entry point; code under test takes
/// that runtime's handle from here.
#[allow(
    clippy::disallowed_methods,
    reason = "a test's #[tokio::test] runtime is its entry-point runtime"
)]
#[cfg(feature = "tauri-host")]
pub fn runtime_handle() -> tokio::runtime::Handle {
    tokio::runtime::Handle::current()
}

pub fn node_builder() -> gents::defra_node::NodeBuilder {
    use gents_store::identity::NodeIdentity as _;

    let directory = tempfile::tempdir().unwrap();
    let identity =
        gents_store::identity::KeyIdentity::load_or_create(directory.path().join("node.key"), None)
            .unwrap();
    gents::defra_node::Node::builder().with_node_identity_did(identity.did())
}

pub async fn boot_core() -> (Arc<ClientCore>, TempDir) {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let paths = DesktopPaths::from_root(tempdir.path());
    let core = ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only())
        .await
        .expect("core starts");
    (Arc::new(core), tempdir)
}

/// A desktop client whose own key is the User of a served runtime it routes
/// to, with a published chat Agent: the shape a desktop reaches its runtime in.
pub struct ServedDesktop {
    pub core: Arc<ClientCore>,
    pub runtime: gents_desktop_core::test_fixtures::ServedRuntimeHome,
    pub agent: gents_protocol::agent_identity::AgentIdentityBinding,
    _directory: TempDir,
}

pub async fn served_desktop() -> ServedDesktop {
    let directory = tempfile::tempdir().unwrap();
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(directory.path()),
            ClientCoreOptions::local_simulated_route(),
        )
        .await
        .unwrap(),
    );
    let (runtime, agent) = served_runtime_for(&core, ServedRoute::CoHosted).await;
    ServedDesktop {
        core,
        runtime,
        agent,
        _directory: directory,
    }
}

/// How a served runtime is routed from the desktop.
#[derive(Clone, Copy)]
pub enum ServedRoute {
    /// The co-hosted standard peer route `ServedRuntimeHome::connect` adds.
    CoHosted,
    /// A managed enrollment route.
    Enrollment,
    /// A further standard peer listening on this port.
    Peer(u16),
}

/// A served runtime whose User is `core`'s key, with a published chat Agent,
/// routed from `core` by `route`.
pub async fn served_runtime_for(
    core: &Arc<ClientCore>,
    route: ServedRoute,
) -> (
    gents_desktop_core::test_fixtures::ServedRuntimeHome,
    gents_protocol::agent_identity::AgentIdentityBinding,
) {
    let runtime = gents_desktop_core::test_fixtures::ServedRuntimeHome::start_unpublished_for(
        Arc::new(core.node_identity().clone()),
    )
    .await;
    runtime.publish_node(serde_json::json!({})).await;
    let agent = runtime.publish_chat_agent().await;
    let home = runtime.home_path().display().to_string();
    match route {
        ServedRoute::CoHosted => runtime.connect(core).await,
        ServedRoute::Enrollment => core
            .add_managed_enrollment_peer_for_test(runtime.node_did(), runtime.graphql(), &home, 1)
            .await
            .unwrap(),
        ServedRoute::Peer(port) => core
            .add_local_standard_peer_route_for_test(
                "Served peer",
                &format!(
                    "127.0.0.1:{port}/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb"
                ),
                runtime.node_did(),
                runtime.graphql(),
                &home,
            )
            .await
            .unwrap(),
    }
    (runtime, agent)
}

/// A desktop client whose key is the User of two served runtimes: the first
/// through a managed enrollment route, the second as a standard peer.
pub async fn served_desktop_pair() -> (
    ServedDesktop,
    gents_desktop_core::test_fixtures::ServedRuntimeHome,
    gents_protocol::agent_identity::AgentIdentityBinding,
) {
    let directory = tempfile::tempdir().unwrap();
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(directory.path()),
            ClientCoreOptions::local_simulated_route(),
        )
        .await
        .unwrap(),
    );
    let (runtime, agent) = served_runtime_for(&core, ServedRoute::Enrollment).await;
    let (second, second_agent) = served_runtime_for(&core, ServedRoute::Peer(56002)).await;
    (
        ServedDesktop {
            core,
            runtime,
            agent,
            _directory: directory,
        },
        second,
        second_agent,
    )
}

impl ServedDesktop {
    pub fn node_did(&self) -> &str {
        self.runtime.node_did()
    }

    pub async fn shutdown(self) {
        self.core.shutdown().await.unwrap();
        self.runtime.shutdown().await;
    }
}

/// The model a [`RunningDesktop`]'s scripted provider serves.
pub const SCRIPTED_MODEL: &str = "scripted-model";

/// A desktop routed to a served runtime that is running its owned loop on a
/// scripted provider, with a lead Agent that may start and message a worker
/// Agent. Requests, sessions and their tool calls are the runtime's own.
pub struct RunningDesktop {
    pub core: Arc<ClientCore>,
    pub runtime: gents_desktop_core::test_fixtures::ServedRuntimeHome,
    pub lead: gents_protocol::agent_identity::AgentIdentityBinding,
    pub provider: super::streaming_backend::MockStreamingBackend,
    running: Option<gents::runtime::embedded::RunningRuntime>,
    _directory: TempDir,
}

impl RunningDesktop {
    pub async fn start(plans: Vec<super::streaming_backend::StreamPlan>) -> Self {
        let provider =
            super::streaming_backend::MockStreamingBackend::start_with_plans(SCRIPTED_MODEL, plans)
                .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let core = Arc::new(
            ClientCore::start_with_paths_and_options(
                DesktopPaths::from_root(directory.path()),
                ClientCoreOptions::local_simulated_route(),
            )
            .await
            .unwrap(),
        );
        let runtime = gents_desktop_core::test_fixtures::ServedRuntimeHome::start_unpublished_for(
            Arc::new(core.node_identity().clone()),
        )
        .await;
        runtime.publish_node(serde_json::json!({})).await;
        let (lead, _worker) = runtime
            .publish_delegating_agents(provider.endpoint(), SCRIPTED_MODEL)
            .await;
        runtime
            .apply_as_user(
                "test.served_runtime.default_agent",
                vec![(
                    gents_store::collection::Collection::DefaultAgentSelection,
                    serde_json::json!({
                        "node_did": runtime.node_did(),
                        "default_agent_did": lead.agent_did,
                    }),
                )],
            )
            .await;
        runtime.connect(&core).await;
        let running = runtime.boot().await;
        Self {
            core,
            runtime,
            lead,
            provider,
            running: Some(running),
            _directory: directory,
        }
    }

    pub fn node_did(&self) -> &str {
        self.runtime.node_did()
    }

    /// The User's DID, the requester of the sessions the desktop starts.
    pub fn user_did(&self) -> String {
        self.core.node_identity().did().to_owned()
    }

    /// Submits `content` as the User's `request_id` in `session_id` for the
    /// lead Agent. Returns the request's document.
    pub async fn send(&self, session_id: &str, request_id: &str, content: &str) -> String {
        self.send_shaped(session_id, request_id, |create| {
            create.content = content.to_owned()
        })
        .await
    }

    /// [`Self::send`] with `shape` setting the content and input.
    pub async fn send_shaped(
        &self,
        session_id: &str,
        request_id: &str,
        shape: impl FnOnce(&mut gents_protocol::request_admission::AgentRequestCreate),
    ) -> String {
        submit_signed(
            &self.runtime.user_access(),
            self.core.node_identity(),
            &self.runtime,
            &self.lead,
            session_id,
            request_id,
            shape,
        )
        .await
        .unwrap()
    }

    /// Reads `query` as the User until `ready` accepts its data.
    pub async fn wait_for(
        &self,
        what: &str,
        query: &str,
        ready: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let access = self.runtime.user_access();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let data = access.execute(query).await.unwrap()["data"].clone();
            if ready(&data) {
                return data;
            }
            if tokio::time::Instant::now() >= deadline {
                let state = access
                    .execute("{ AgentRequest { request_id session_id requester_did lifecycle_state failure_reason caused_by_parent_tool_call_id } AgentToolCall { tool_call_id tool_name lifecycle_state status } AgentOutputSegment { payload } }")
                    .await;
                panic!("timed out waiting for {what}: {data}; runtime state: {state:?}");
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// Waits until the request `request_doc` is in `state`; returns its row.
    pub async fn wait_for_request_state(
        &self,
        request_doc: &str,
        state: &str,
    ) -> serde_json::Value {
        self.wait_for(
            &format!("request {request_doc} {state}"),
            &format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID request_id session_id requester_did lifecycle_state }} }}"#,
                escape_graphql_string(request_doc)
            ),
            |data| data["AgentRequest"][0]["lifecycle_state"] == state,
        )
        .await["AgentRequest"][0]
            .clone()
    }

    /// Waits for the request the tool call `tool_call_id` caused to reach
    /// `state`; returns its row.
    pub async fn wait_for_caused(&self, tool_call_id: &str, state: &str) -> serde_json::Value {
        self.wait_for(
            &format!("request caused by {tool_call_id} {state}"),
            &format!(
                r#"{{ AgentRequest(filter: {{ caused_by_parent_tool_call_id: {{ _eq: "{}" }} }}) {{ _docID request_id session_id requester_did lifecycle_state }} }}"#,
                escape_graphql_string(tool_call_id)
            ),
            |data| {
                data["AgentRequest"]
                    .as_array()
                    .is_some_and(|rows| rows.len() == 1 && rows[0]["lifecycle_state"] == state)
            },
        )
        .await["AgentRequest"][0]
            .clone()
    }

    pub async fn shutdown(mut self) {
        drop(self.provider);
        if let Some(running) = self.running.take() {
            running.shutdown().await.unwrap();
        }
        self.core.shutdown().await.unwrap();
        self.runtime.shutdown().await;
    }
}

/// The session lineage a running runtime produces from a User's request:
/// in `sess_parent` the lead Agent starts a worker session with `agent_new`
/// (`tc_start`), then messages it (`tc_message`) and a session it opened as a
/// client (`sess_existing`, `tc_existing`). `sess_unrelated` is a User
/// session nothing started or messaged. Ids are the runtime's own.
pub struct ProvenanceFixture {
    pub desktop: RunningDesktop,
    pub parent_doc: String,
    pub child_session: String,
    pub child_opening: String,
    pub child_message: String,
    pub existing_message: String,
}

impl ProvenanceFixture {
    pub async fn shutdown(self) {
        self.desktop.shutdown().await;
    }
}

pub async fn provenance_fixture() -> ProvenanceFixture {
    use super::streaming_backend::{StreamChunk, StreamPlan, StreamResponse, StreamScript};

    let held = |marker: &'static str| {
        StreamPlan::current_authored_user(
            marker,
            vec![StreamResponse::Stream(StreamScript::paused(
                marker,
                ["working"],
            ))],
        )
    };
    let completes = |marker: &'static str| {
        StreamPlan::current_authored_user(marker, vec![StreamResponse::completes(marker, ["done"])])
    };
    let desktop = RunningDesktop::start(vec![
        StreamPlan::current_authored_user(
            "PARENT-OPENS-WORK",
            vec![StreamResponse::streams(
                "PARENT-OPENS-WORK",
                vec![StreamChunk::tool_call(
                    "tc_start",
                    "agent_new",
                    serde_json::json!({"agent": "worker", "prompt": "CHILD-OPENING-WORK"})
                        .to_string(),
                )],
            )],
        ),
        completes("CHILD-OPENING-WORK"),
        held("CHILD-FOLLOW-UP"),
        completes("EXISTING-OPENING-WORK"),
        held("EXISTING-FOLLOW-UP"),
        held("UNRELATED-WORK"),
    ])
    .await;
    desktop
        .provider
        .enable_dynamic_followups("PARENT-OPENS-WORK");

    // The lead Agent opens `sess_existing` with itself as a client.
    let lead = gents_store::identity::resolve_agent_identity(
        desktop.runtime.runtime().as_ref(),
        &desktop.lead,
    )
    .unwrap();
    let existing = submit_signed(
        &desktop.runtime.agent_access(&desktop.lead).await,
        lead.as_ref(),
        &desktop.runtime,
        &desktop.lead,
        "sess_existing",
        "req_existing_1",
        |create| create.content = "EXISTING-OPENING-WORK".into(),
    )
    .await
    .unwrap();
    desktop.wait_for_request_state(&existing, "completed").await;
    let unrelated = desktop
        .send("sess_unrelated", "req_unrelated", "UNRELATED-WORK")
        .await;
    desktop
        .wait_for_request_state(&unrelated, "processing")
        .await;

    let parent_doc = desktop
        .send("sess_parent", "req_parent", "PARENT-OPENS-WORK")
        .await;
    let opening = desktop.wait_for_caused("tc_start", "completed").await;
    let child_session = opening["session_id"].as_str().unwrap().to_owned();
    desktop.provider.enqueue_response(
        "PARENT-OPENS-WORK",
        StreamResponse::streams(
            "PARENT-OPENS-WORK",
            vec![
                StreamChunk::tool_call(
                    "tc_message",
                    "agent_message",
                    serde_json::json!({"session_id": child_session, "message": "CHILD-FOLLOW-UP"})
                        .to_string(),
                ),
                StreamChunk::tool_call(
                    "tc_existing",
                    "agent_message",
                    serde_json::json!({"session_id": "sess_existing", "message": "EXISTING-FOLLOW-UP"})
                        .to_string(),
                ),
            ],
        ),
    );
    desktop.provider.enqueue_response(
        "PARENT-OPENS-WORK",
        StreamResponse::completes("PARENT-OPENS-WORK", ["parent done"]),
    );
    let message = desktop.wait_for_caused("tc_message", "processing").await;
    let existing_message = desktop.wait_for_caused("tc_existing", "processing").await;
    desktop
        .wait_for_request_state(&parent_doc, "completed")
        .await;
    ProvenanceFixture {
        parent_doc,
        child_session,
        child_opening: opening["request_id"].as_str().unwrap().to_owned(),
        child_message: message["request_id"].as_str().unwrap().to_owned(),
        existing_message: existing_message["request_id"].as_str().unwrap().to_owned(),
        desktop,
    }
}

/// Submits `request_id` in `session_id` as `core`'s User through the client
/// admission owner, reserving the session first. Returns its document.
pub async fn submit_request(
    core: &ClientCore,
    runtime: &gents_desktop_core::test_fixtures::ServedRuntimeHome,
    agent: &gents_protocol::agent_identity::AgentIdentityBinding,
    session_id: &str,
    request_id: &str,
) -> String {
    try_submit_request(core, runtime, agent, session_id, request_id)
        .await
        .unwrap()
}

/// [`submit_request`], returning the owner's refusal.
pub async fn try_submit_request(
    core: &ClientCore,
    runtime: &gents_desktop_core::test_fixtures::ServedRuntimeHome,
    agent: &gents_protocol::agent_identity::AgentIdentityBinding,
    session_id: &str,
    request_id: &str,
) -> anyhow::Result<String> {
    submit_signed(
        &runtime.user_access(),
        core.node_identity(),
        runtime,
        agent,
        session_id,
        request_id,
        |create| create.content = format!("{request_id} text"),
    )
    .await
}

/// Submits `request_id` in `session_id` for `agent`, signed by `signer` as
/// its requester, through the client admission owner on `access` (the
/// signer's own access), reserving the session first. `shape` sets the
/// content and input before signing. Returns the request's document.
pub async fn submit_signed(
    access: &gents_store::config_client::ConfigAccess,
    signer: &dyn gents_store::identity::NodeIdentity,
    runtime: &gents_desktop_core::test_fixtures::ServedRuntimeHome,
    agent: &gents_protocol::agent_identity::AgentIdentityBinding,
    session_id: &str,
    request_id: &str,
    shape: impl FnOnce(&mut gents_protocol::request_admission::AgentRequestCreate),
) -> anyhow::Result<String> {
    use gents_protocol::request_admission::{
        AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
    };

    let requester = signer.did().to_owned();
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        request_id,
        runtime.node_did(),
        &requester,
        &agent.agent_did,
        session_id,
        String::new(),
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        AgentRequestAdmissionRecord::client(&requester),
    );
    shape(&mut create);
    create.session_doc_id =
        Some(gents_session::request::lifecycle::reserve_goal_session(access, &create).await?);
    create.agent_doc_id = Some(runtime.agent_doc_id(agent).await);
    gents_session::request::admission::sign_agent_request_create(signer, &mut create).await?;
    gents_session::request::lifecycle::submit_client_request(access, &create).await?;
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            escape_graphql_string(request_id),
            escape_graphql_string(session_id),
        ))
        .await?;
    Ok(response["data"]["AgentRequest"][0]["_docID"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("submitted request is not readable"))?
        .to_owned())
}

/// Ends the pending request `doc_id` failed with `reason`, as its working
/// Agent rejects its admission through the request lifecycle owner.
pub async fn reject_admission(
    runtime: &gents_desktop_core::test_fixtures::ServedRuntimeHome,
    agent: &gents_protocol::agent_identity::AgentIdentityBinding,
    doc_id: &str,
    reason: &str,
) {
    let access = runtime.agent_access(agent).await;
    let response = access
        .execute(&format!(
            "{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{}\" }} }}) {{ {} }} }}",
            escape_graphql_string(doc_id),
            gents_protocol::row::AGENT_REQUEST_FIELDS,
        ))
        .await
        .unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    let mut lifecycle = gents_session::request::lifecycle::RequestLifecycle::new_with_node_did(
        runtime.node().clone(),
        row.try_into().unwrap(),
        300,
    );
    let identity =
        gents_store::identity::resolve_agent_identity(runtime.runtime().as_ref(), agent).unwrap();
    lifecycle.bind_managed_agent(identity.as_ref()).unwrap();
    lifecycle.reject_admission(reason).await.unwrap();
}

/// The DID of [`fixture_signer`]`(7)`; the sessions it starts carry
/// receipts that key signed.
pub const OPERATOR: &str = "did:key:z6MkvDqGT54cXesYGvABpF1UapVNwjCqRcafi4Px6Thv5T3Z";

/// A fixed test key, so fixture rows can name its DID as a constant.
pub fn fixture_signer(seed: u8) -> gents_store::identity::KeyIdentity {
    use std::io::Write as _;
    // A named temporary file is created private to its owner, as a key must be.
    let mut key = tempfile::NamedTempFile::new().unwrap();
    key.write_all(&crypto::ed25519_key_from_seed(&[seed; 32]).unwrap())
        .unwrap();
    gents_store::identity::KeyIdentity::load_existing(key.path(), None).unwrap()
}

/// Write `session` as creation writes a started session: the immutable
/// starter index at create, then the receipt `signer` signs for the created
/// physical document, binding `source`, `parent` and `opening`.
pub async fn create_started_session(
    core: &Arc<ClientCore>,
    signer: &gents_store::identity::KeyIdentity,
    mut session: gents_protocol::session::AgentSession,
    source: &gents_protocol::session::SessionReference,
    parent: &str,
    opening: &str,
) {
    use gents_protocol::session::{
        SessionCreationCause, SessionCreationReceipt, SessionCreationStarter,
    };
    use gents_store::identity::NodeIdentity as _;
    assert_eq!(
        session.requester_did.as_deref(),
        Some(signer.did()),
        "the creating Agent signs its receipt"
    );
    let mut provenance = session.provenance.take().unwrap_or_default();
    provenance.parent_request_doc_id = Some(parent.into());
    provenance.opening_request_doc_id = Some(opening.into());
    provenance.started_by = Some(source.clone());
    let mut input = serde_json::to_value(&gents_protocol::session::AgentSession {
        provenance: Some(provenance.clone()),
        ..session.clone()
    })
    .unwrap();
    input["started_by_node_did"] = source.node_did.clone().into();
    input["started_by_session_id"] = source.session_id.clone().into();
    let input = gents_protocol::graphql::graphql_input_literal(&input).unwrap();
    let doc_id = created_doc_id(
        core,
        &format!("mutation {{ create_AgentSession(input: {input}) {{ _docID }} }}"),
        "AgentSession",
    )
    .await;
    let mut receipt = SessionCreationReceipt {
        node_did: session.node_did.clone(),
        session_id: session.session_id.clone(),
        session_doc_id: doc_id,
        requester_did: session.requester_did.clone(),
        agent_did: session.agent_did.clone(),
        opening_request_doc_id: opening.into(),
        cause: SessionCreationCause::AgentNew,
        cause_binding: "fixture-call".into(),
        starter: Some(SessionCreationStarter {
            source: source.clone(),
            parent_request_doc_id: parent.into(),
        }),
        signature: Vec::new(),
    };
    receipt.signature = signer.sign(&receipt.signing_payload()).await.unwrap();
    provenance.creation_receipt = Some(receipt);
    write_provenance(core, &session.node_did, &session.session_id, &provenance).await;
}

pub async fn write_provenance(
    core: &Arc<ClientCore>,
    node_did: &str,
    session_id: &str,
    provenance: &gents_protocol::session::SessionProvenance,
) {
    let input = gents_protocol::graphql::graphql_input_literal(
        &serde_json::json!({ "provenance": provenance }),
    )
    .unwrap();
    let response = core
        .node()
        .execute(&format!(
            r#"mutation {{ update_AgentSession(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }}, input: {input}) {{ _docID }} }}"#,
            escape_graphql_string(node_did),
            escape_graphql_string(session_id),
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
}

/// A fixture session on `node_did` with no provenance.
pub fn fixture_session(node_did: &str, session_id: &str) -> gents_protocol::session::AgentSession {
    gents_protocol::session::AgentSession {
        session_id: session_id.into(),
        node_did: node_did.into(),
        requester_did: Some(OPERATOR.into()),
        agent_did: "worker".into(),
        created_at: "2026-05-20T00:00:00Z".into(),
        closed_at: None,
        archived_at: None,
        archive_revision: 0,
        stopped_at: None,
        title: None,
        tags: Vec::new(),
        provenance: None,
        observation: None,
    }
}

pub struct CountingRead {
    inner: gents_store::config_client::ConfigAccess,
    queries: std::sync::atomic::AtomicUsize,
}

impl CountingRead {
    pub fn new(inner: gents_store::config_client::ConfigAccess) -> Self {
        Self {
            inner,
            queries: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn queries(&self) -> usize {
        self.queries.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl gents_store::config_client::ConfigRead for CountingRead {
    async fn execute_read(
        &self,
        document: &str,
    ) -> Result<serde_json::Value, gents_store::config_client::ConfigClientError> {
        self.queries
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.execute_read(document).await
    }
}

pub async fn refresh_request_and_assert_fresh_projection(
    core: &ClientCore,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    request_id: &str,
) {
    let projection = core
        .refresh_local_request(node_did, session_id, Some(request_id), None, &[])
        .await
        .expect("refresh request");
    let projection = projection.unwrap_or_else(|| {
        panic!("request {request_id} has no fresh projection for node {node_did}")
    });
    assert_eq!(projection.requester_did.as_deref(), requester_did);
    assert!(
        projection.requests.iter().any(|row| {
            row.node_did.as_deref() == Some(node_did)
                && row.session_id.as_deref() == Some(session_id)
                && row.requester_did.as_deref() == requester_did
                && row.request_id == request_id
        }),
        "fresh request projection omitted {request_id} for node {node_did}"
    );
}

async fn created_doc_id(core: &Arc<ClientCore>, mutation: &str, collection: &str) -> String {
    let response = core.node().execute(mutation).await;
    assert!(
        !response.has_errors(),
        "seed {collection}: {:?}",
        response.errors
    );
    gents_store::graphql::single_mutation_document(&response, &format!("create_{collection}"))
        .expect("created document")
        .and_then(|document| document["_docID"].as_str())
        .expect("created document id")
        .to_owned()
}

#[allow(clippy::too_many_arguments)]
pub async fn create_request(
    core: &Arc<ClientCore>,
    request_id: &str,
    node_did: &str,
    session_id: &str,
    lifecycle_state: &str,
    created_at: &str,
    cause: Option<(&str, &str, &str)>,
) -> String {
    let caused = cause
        .map(|(doc, call, call_doc)| {
            format!(
                r#"request_hop: 1, caused_by_parent_request_id: "req_parent", caused_by_parent_request_doc_id: "{doc}", caused_by_parent_tool_call_id: "{call}", caused_by_parent_tool_call_doc_id: "{call_doc}","#
            )
        })
        .unwrap_or_default();
    created_doc_id(
        core,
        &format!(
            r#"mutation {{ create_AgentRequest(input: {{ purpose: "normal", request_id: "{request_id}", node_did: "{node_did}", requester_did: "{OPERATOR}", agent_did: "worker", session_id: "{session_id}", {caused} content: "work", lifecycle_state: "{lifecycle_state}", backend_id: "", created_at: "{created_at}", retry_count: 0 }}) {{ _docID }} }}"#
        ),
        "AgentRequest",
    )
    .await
}

pub async fn create_session(
    core: &Arc<ClientCore>,
    node_did: &str,
    session_id: &str,
    started_by: Option<&str>,
) {
    let session = gents_protocol::session::AgentSession {
        session_id: session_id.into(),
        node_did: node_did.into(),
        requester_did: Some(OPERATOR.into()),
        agent_did: "worker".into(),
        created_at: "2026-05-20T00:00:00Z".into(),
        closed_at: None,
        archived_at: None,
        archive_revision: 0,
        stopped_at: None,
        title: None,
        tags: Vec::new(),
        provenance: started_by.map(|doc_id| gents_protocol::session::SessionProvenance {
            parent_request_doc_id: Some(doc_id.into()),
            ..Default::default()
        }),
        observation: None,
    };
    let input = gents_protocol::graphql::graphql_input_literal(
        &serde_json::to_value(&session).expect("session json"),
    )
    .expect("session input");
    created_doc_id(
        core,
        &format!("mutation {{ create_AgentSession(input: {input}) {{ _docID }} }}"),
        "AgentSession",
    )
    .await;
}

pub async fn seed_job_row_for_node(
    core: &Arc<ClientCore>,
    node_did: &str,
    session_id: &str,
    tool_call_id: &str,
    lifecycle_state: &str,
    progress: &JobProgress,
    coded_output: Option<(&str, &str)>,
) -> String {
    use gents_protocol::job::{JobCeiling, JobOrigin, JobPin};
    use gents_protocol::output::rows::{
        output_segment_create_variables, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{
        OutputOutcome, OutputSegment, OutputSource, OutputWriter, SegmentRun, SourceClose,
        StreamDeclaration, StreamPayload,
    };
    use gents_store::config_client::ConfigAccess;

    let access = ConfigAccess::local(core.node_arc());
    let request_doc_id = create_request(
        core,
        &format!("req-{tool_call_id}"),
        node_did,
        session_id,
        "completed",
        "2026-05-20T00:00:00Z",
        None,
    )
    .await;
    let mut pin = JobPin {
        script_id: "fixture-script".into(),
        source_digest: "sha256:source".into(),
        origin: JobOrigin::Installed,
        engine_digest: "sha256:engine".into(),
        plugin_ref: "fixture/workflow".into(),
        agent_budget: 4,
        handle: tool_call_id.into(),
        cwd: "/fixture/cwd".into(),
        adopted_at: 1_780_000_000_000,
        args_digest: "sha256:args".into(),
        chain_root_doc_id: String::new(),
        resume_of: None,
        replayed_from: None,
        ceiling_at_pin: JobCeiling {
            targets: vec!["general".into()],
            max_live: 2,
            max_spawns: 8,
            max_messages_per_slot: 4,
            result_bytes: 65_536,
            max_wall_secs: 600,
        },
    };
    let completed_at = if lifecycle_state == "running" {
        String::new()
    } else {
        r#"completed_at: "2026-05-20T00:05:00Z","#.to_owned()
    };
    let mutation = format!(
        r#"mutation($job_pin: JSON, $job_progress: JSON) {{ create_AgentToolCall(input: {{
            tool_call_key: "{key}", request_id: "req-{call}", request_doc_id: "{request_doc_id}",
            session_id: "{session}", node_did: "{node}", requester_did: "{OPERATOR}",
            tool_name: "workflow", tool_call_id: "{call}", message_sequence: 1,
            await_mode: "background", status: "{lifecycle}", lifecycle_state: "{lifecycle}",
            started_at: "2026-05-20T00:00:00Z", {completed_at}
            job_pin: $job_pin, job_progress: $job_progress
        }}) {{ _docID }} }}"#,
        node = escape_graphql_string(node_did),
        key = escape_graphql_string(&format!("{session_id}:{tool_call_id}")),
        request_doc_id = escape_graphql_string(&request_doc_id),
        session = escape_graphql_string(session_id),
        call = escape_graphql_string(tool_call_id),
        lifecycle = escape_graphql_string(lifecycle_state),
    );
    let variables = serde_json::json!({ "job_pin": pin, "job_progress": progress });
    let response = access
        .transact_write("test.fixture.job_row", |txn| {
            let (mutation, variables) = (mutation.clone(), variables.clone());
            Box::pin(async move { txn.execute_with_variables(&mutation, &variables).await })
        })
        .await
        .expect("seed job row");
    let row_doc_id = gents_protocol::graphql::extract_mutation_doc_id(&response, "AgentToolCall")
        .expect("job row doc id");
    pin.chain_root_doc_id = row_doc_id.clone();
    let update = format!(
        r#"mutation($job_pin: JSON) {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ job_pin: $job_pin }}) {{ _docID }} }}"#,
        escape_graphql_string(&row_doc_id)
    );
    let variables = serde_json::json!({ "job_pin": pin });
    access
        .transact_write("test.fixture.job_pin", |txn| {
            let (update, variables) = (update.clone(), variables.clone());
            Box::pin(async move { txn.execute_with_variables(&update, &variables).await })
        })
        .await
        .expect("pin the job chain root");

    if let Some((code, message)) = coded_output {
        let payload = serde_json::json!({ "code": code, "message": message }).to_string();
        let segment = OutputSegment {
            node_did: node_did.into(),
            requester_did: Some(OPERATOR.into()),
            session_id: session_id.into(),
            request_doc_id,
            source: OutputSource::ToolCall {
                tool_call_doc_id: row_doc_id.clone(),
            },
            writer: OutputWriter::ToolExecution {
                tool_call_doc_id: row_doc_id.clone(),
            },
            ordinal: Some(0),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: payload.len() as u32,
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::ToolOutput,
                }),
            }],
            payload: payload.clone(),
            close: Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![payload.len() as u64],
            }),
            created_at: "2026-05-20T00:05:00Z".into(),
        };
        let variables = output_segment_create_variables(&segment).expect("segment variables");
        access
            .transact_write("test.fixture.job_output", |txn| {
                let variables = variables.clone();
                Box::pin(async move {
                    txn.execute_with_variables(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION, &variables)
                        .await
                })
            })
            .await
            .expect("seed job output");
    }
    row_doc_id
}

/// A front for a served runtime that answers `GET /status` as a ready
/// `gents serve` daemon of `node_did` does and forwards every other request
/// to the runtime's authenticated DefraDB HTTP: an embedded test runtime has
/// no daemon status route.
pub struct ServingStatusProxy {
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl ServingStatusProxy {
    pub async fn start(upstream_graphql: &str, node_did: &str) -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let upstream = reqwest::Url::parse(upstream_graphql)
            .unwrap()
            .socket_addrs(|| None)
            .unwrap()[0];
        let status = serde_json::json!({
            "node_did": node_did,
            gents_protocol::serve_lifecycle::STATUS_LIFECYCLE_FIELD: "ready",
            "request_input_contract_version":
                gents_protocol::request_input::REQUEST_INPUT_CONTRACT_VERSION,
            gents_protocol::goal_control::STATUS_GOAL_STOP_CONTRACT_FIELD:
                gents_protocol::goal_control::GOAL_STOP_CONTRACT_VERSION,
        })
        .to_string();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                let status = status.clone();
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut byte = [0_u8; 1];
                    while !head.ends_with(b"\r\n") {
                        match client.read(&mut byte).await {
                            Ok(1) => head.push(byte[0]),
                            _ => return,
                        }
                    }
                    if head.starts_with(b"GET /status ") {
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{status}",
                            status.len()
                        );
                        let _ = client.write_all(response.as_bytes()).await;
                        return;
                    }
                    let Ok(mut server) = tokio::net::TcpStream::connect(upstream).await else {
                        return;
                    };
                    if server.write_all(&head).await.is_err() {
                        return;
                    }
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
                });
            }
        });
        Self { url, task }
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

impl Drop for ServingStatusProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Seeds one plugin job row in `session_id` of a served desktop: the User's
/// request comes through client admission, and the working Agent records the
/// job's tool call, its pin and any closed terminal output as session
/// children, as the tool lifecycle owner persists them. Returns the row's
/// `_docID`.
pub async fn seed_served_job_row(
    served: &ServedDesktop,
    session_id: &str,
    tool_call_id: &str,
    lifecycle_state: &str,
    progress: &JobProgress,
    coded_output: Option<(&str, &str)>,
) -> String {
    use gents_protocol::job::{JobCeiling, JobOrigin, JobPin};
    use gents_protocol::output::rows::output_segment_create_variables;
    use gents_protocol::output::{
        OutputOutcome, OutputSegment, OutputSource, OutputWriter, SegmentRun, SourceClose,
        StreamDeclaration, StreamPayload,
    };

    let request_id = format!("req-{tool_call_id}");
    let request_doc = submit_request(
        &served.core,
        &served.runtime,
        &served.agent,
        session_id,
        &request_id,
    )
    .await;
    let session_doc = served
        .runtime
        .user_access()
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ session_doc_id }} }}"#,
            escape_graphql_string(&request_doc)
        ))
        .await
        .unwrap()["data"]["AgentRequest"][0]["session_doc_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let user = served.core.node_identity().did().to_owned();
    let pin = JobPin {
        script_id: "fixture-script".into(),
        source_digest: "sha256:source".into(),
        origin: JobOrigin::Installed,
        engine_digest: "sha256:engine".into(),
        plugin_ref: "fixture/workflow".into(),
        agent_budget: 4,
        handle: tool_call_id.into(),
        cwd: "/fixture/cwd".into(),
        adopted_at: 1_780_000_000_000,
        args_digest: "sha256:args".into(),
        chain_root_doc_id: String::new(),
        resume_of: None,
        replayed_from: None,
        ceiling_at_pin: JobCeiling {
            targets: vec!["general".into()],
            max_live: 2,
            max_spawns: 8,
            max_messages_per_slot: 4,
            result_bytes: 65_536,
            max_wall_secs: 600,
        },
    };
    let mut call = serde_json::json!({
        "tool_call_key": format!("{session_id}:{tool_call_id}"),
        "request_id": request_id, "request_doc_id": request_doc,
        "session_id": session_id, "session_doc_id": session_doc,
        "node_did": served.node_did(), "requester_did": user,
        "agent_did": served.agent.agent_did,
        "tool_name": "workflow", "tool_call_id": tool_call_id, "message_sequence": 1,
        "await_mode": "background", "status": lifecycle_state,
        "lifecycle_state": lifecycle_state, "started_at": "2026-05-20T00:00:00Z",
        "job_progress": progress,
    });
    if lifecycle_state != "running" {
        call["completed_at"] = "2026-05-20T00:05:00Z".into();
    }
    let segment = coded_output.map(|(code, message)| {
        let payload = serde_json::json!({ "code": code, "message": message }).to_string();
        (payload, request_doc.clone())
    });
    let node_did = served.node_did().to_owned();
    served
        .runtime
        .agent_access(&served.agent)
        .await
        .transact_write("test.served_job_row", |txn| {
            let (call, session_doc, segment, user, node_did) =
                (&call, &session_doc, &segment, &user, &node_did);
            let pin = &pin;
            Box::pin(async move {
                let mut pin = pin.clone();
                let response = txn
                    .execute_with_variables(
                        "mutation($input: AgentToolCallMutationInputArg!) { create_AgentToolCall(input: $input) {_docID} }",
                        &serde_json::json!({"input": call}),
                    )
                    .await?;
                let row = gents_store::graphql::created_doc_id(&response, "AgentToolCall")?;
                txn.add_resource_relationship("AgentToolCall", &row, "parent", "sessions", session_doc)
                    .await?;
                pin.chain_root_doc_id = row.clone();
                txn.execute_with_variables(
                    &format!(
                        r#"mutation($job_pin: JSON) {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ job_pin: $job_pin }}) {{ _docID }} }}"#,
                        escape_graphql_string(&row)
                    ),
                    &serde_json::json!({"job_pin": pin}),
                )
                .await?;
                if let Some((payload, request_doc_id)) = segment {
                    let segment = OutputSegment {
                        node_did: node_did.clone(),
                        requester_did: Some(user.clone()),
                        session_id: call["session_id"].as_str().unwrap().into(),
                        request_doc_id: request_doc_id.clone(),
                        source: OutputSource::ToolCall {
                            tool_call_doc_id: row.clone(),
                        },
                        writer: OutputWriter::ToolExecution {
                            tool_call_doc_id: row.clone(),
                        },
                        ordinal: Some(0),
                        runs: vec![SegmentRun {
                            stream: 0,
                            bytes: payload.len() as u32,
                            declaration: Some(StreamDeclaration {
                                block_index: 0,
                                part_index: 0,
                                payload: StreamPayload::ToolOutput,
                            }),
                        }],
                        payload: payload.clone(),
                        close: Some(SourceClose::Closed {
                            outcome: OutputOutcome::Complete,
                            segments: 1,
                            stream_bytes: vec![payload.len() as u64],
                        }),
                        created_at: "2026-05-20T00:05:00Z".into(),
                    };
                    let response = txn
                        .execute_with_variables(
                            gents_protocol::output::rows::CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                            &output_segment_create_variables(&segment)?,
                        )
                        .await?;
                    let doc =
                        gents_store::graphql::created_doc_id(&response, "AgentOutputSegment")?;
                    txn.add_resource_relationship(
                        "AgentOutputSegment",
                        &doc,
                        "parent",
                        "sessions",
                        session_doc,
                    )
                    .await?;
                }
                Ok::<_, anyhow::Error>(row)
            })
        })
        .await
        .unwrap()
}

/// Records a plain completed `tool_name` call `tool_call_id` in the served
/// request `request_id`, as the working Agent persists it.
pub async fn seed_served_tool_call(
    served: &ServedDesktop,
    session_id: &str,
    request_id: &str,
    tool_call_id: &str,
    tool_name: &str,
) -> String {
    let request = served
        .runtime
        .user_access()
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ _docID session_doc_id requester_did }} }}"#,
            escape_graphql_string(request_id)
        ))
        .await
        .unwrap()["data"]["AgentRequest"][0]
        .clone();
    let session_doc = request["session_doc_id"].as_str().unwrap().to_owned();
    let call = serde_json::json!({
        "tool_call_key": format!("{session_id}:{tool_call_id}"),
        "request_id": request_id, "request_doc_id": request["_docID"],
        "session_id": session_id, "session_doc_id": session_doc,
        "node_did": served.node_did(), "requester_did": request["requester_did"],
        "agent_did": served.agent.agent_did,
        "tool_name": tool_name, "tool_call_id": tool_call_id, "message_sequence": 1,
        "await_mode": "foreground", "status": "completed", "lifecycle_state": "completed",
        "started_at": "2026-05-20T00:00:00Z", "completed_at": "2026-05-20T00:00:01Z",
    });
    served
        .runtime
        .agent_access(&served.agent)
        .await
        .transact_write("test.served_tool_call", |txn| {
            let (call, session_doc) = (&call, &session_doc);
            Box::pin(async move {
                let response = txn
                    .execute_with_variables(
                        "mutation($input: AgentToolCallMutationInputArg!) { create_AgentToolCall(input: $input) {_docID} }",
                        &serde_json::json!({"input": call}),
                    )
                    .await?;
                let row = gents_store::graphql::created_doc_id(&response, "AgentToolCall")?;
                txn.add_resource_relationship("AgentToolCall", &row, "parent", "sessions", session_doc)
                    .await?;
                Ok::<_, anyhow::Error>(row)
            })
        })
        .await
        .unwrap()
}
