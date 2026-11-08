use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gents::documents::{Agent, InferenceProfile, Node};
use gents::store::config_client::DesiredStateApplyPlan;
use gents::store::graphql::escape_graphql_string;
use gents::{
    default_inference_profile_id_for_agent, Collection, DocumentRuntimeOptions, Gents,
    NodeIdentity, ToolCeiling,
};
use gents_protocol::output::reconstruction::{reconstruct_message, ObservedSegment};
use gents_protocol::output::{OutputSegment, TranscriptMessage};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::Deserialize;

use crate::support::agent_requests::RequestActors;
use crate::support::interrupt::BootedAgent;
use crate::support::snapshots::SnapshotReader;
use crate::support::TestDb;

#[path = "inference_target.rs"]
pub mod inference_target;
pub use inference_target::InferenceTarget;

pub fn live_target() -> InferenceTarget {
    InferenceTarget::selected().unwrap_or_else(|error| panic!("{error:#}"))
}

/// Binds the live target as the node's client key. The fixture User is the
/// User `gents init` creates: its key is the node's native admin
/// (`GrantSet::HouseholdOwner`), and it configures the Node, the managed Agent
/// and the Agent's inference documents. Requests and reads go through the
/// returned actors.
pub async fn bind_target(db: &TestDb, target: &InferenceTarget) -> RequestActors {
    let fixture = crate::support::managed_agent_fixture(db).await;
    crate::support::grant_household_owner(db, &fixture.user).await;
    let node_did = db.node_identity.did().to_owned();
    let agent_did = fixture.binding.agent_did.clone();
    let profile_id = default_inference_profile_id_for_agent(&agent_did);
    let backend = target.backend(&node_did);
    let profile = InferenceProfile {
        profile_id: profile_id.clone(),
        ..target.profile(&node_did)
    };
    let sampling = target.sampling(&node_did);
    fixture
        .access
        .transact_write("test.bind_live_backend", |txn| {
            use gents::documents::apply_desired_state_plan;
            use gents::store::config_client::{
                read_desired_state_record_in_txn as read, DesiredStateApplyDocument,
            };
            let node_did = &node_did;
            let agent_did = &agent_did;
            let profile_id = &profile_id;
            let backend = &backend;
            let profile = &profile;
            let sampling = &sampling;
            Box::pin(async move {
                let (_, node) = read(txn, Collection::Node, node_did, node_did)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("the fixture User's Node is missing"))?;
                let mut node: Node = serde_json::from_value(node)?;
                node.default_agent_did = Some(agent_did.clone());
                let (_, agent) = read(txn, Collection::Agent, node_did, agent_did)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("the fixture Agent is missing"))?;
                let mut agent: Agent = serde_json::from_value(agent)?;
                agent.display_name = Some("Live default agent".to_string());
                agent.inference_profile_id = profile_id.clone();
                let plan = DesiredStateApplyPlan::new(
                    [
                        (Collection::Node, serde_json::to_value(node)?),
                        (Collection::InferenceBackend, serde_json::to_value(backend)?),
                        (Collection::InferenceProfile, serde_json::to_value(profile)?),
                    ]
                    .into_iter()
                    .chain(
                        sampling
                            .as_ref()
                            .map(serde_json::to_value)
                            .transpose()?
                            .map(|sampling| (Collection::InferenceSampling, sampling)),
                    )
                    .chain([(Collection::Agent, serde_json::to_value(agent)?)])
                    .map(|(collection, value)| DesiredStateApplyDocument {
                        collection,
                        add: value.clone(),
                        update: value,
                    })
                    .collect(),
                )?;
                apply_desired_state_plan(txn, &plan).await.map(|_| ())
            })
        })
        .await
        .expect("bind the live target as the node's client key");
    RequestActors::from_fixture(db, fixture).await
}

pub async fn boot_live_agent(db: &TestDb, identity: Arc<dyn NodeIdentity>) -> Result<BootedAgent> {
    boot_live_agent_with_ceiling(db, identity, ToolCeiling::meta_only()).await
}

pub async fn boot_live_agent_with_ceiling(
    db: &TestDb,
    identity: Arc<dyn NodeIdentity>,
    tool_ceiling: ToolCeiling,
) -> Result<BootedAgent> {
    Ok(boot_live_agent_with_options(
        db,
        identity,
        DocumentRuntimeOptions {
            tool_ceiling,
            ..Default::default()
        },
    )
    .await?
    .0)
}

pub async fn boot_live_agent_with_options(
    db: &TestDb,
    identity: Arc<dyn NodeIdentity>,
    options: DocumentRuntimeOptions,
) -> Result<(BootedAgent, Gents)> {
    let (running, agent) =
        gents::eval::runner::embedded::boot_runtime(&db.home, identity, options).await?;
    Ok((
        BootedAgent::new(running.shutdown, running.handle, running.node_did),
        agent,
    ))
}

fn is_terminal(state: &str) -> bool {
    RequestLifecycleState::is_terminal_str(Some(state))
}

fn optional_row<T>(data: &serde_json::Value, key: &str) -> Option<T>
where
    T: serde::de::DeserializeOwned,
{
    data[key]
        .as_array()
        .and_then(|rows| rows.first())
        .map(|row| serde_json::from_value(row.clone()).expect("decode observed row"))
}

async fn fetch_request_lifecycle<R>(reader: &R, request_id: &str) -> Option<String>
where
    R: SnapshotReader + ?Sized,
{
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 2) {{
                lifecycle_state
            }}
        }}"#
    );
    #[derive(Deserialize)]
    struct Row {
        lifecycle_state: Option<String>,
    }
    let data = reader.read_data(&query).await;
    let rows = data["AgentRequest"].as_array().cloned().unwrap_or_default();
    assert!(
        rows.len() <= 1,
        "live test request label is ambiguous: {request_id}"
    );
    optional_row::<Row>(&data, "AgentRequest").and_then(|r| r.lifecycle_state)
}

/// Waits for `request_id` to terminalize, observed as `reader`: ACP shows a
/// request only to its related actors, so the reader is one of them or the
/// node's native admin.
pub async fn wait_for_request_terminal<R>(reader: &R, request_id: &str, timeout: Duration) -> String
where
    R: SnapshotReader + ?Sized,
{
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last = String::from("<none>");
    loop {
        if let Some(state) = fetch_request_lifecycle(reader, request_id).await {
            last = state.clone();
            if is_terminal(&state) {
                return state;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            let escaped = escape_graphql_string(request_id);
            let evidence = reader
                .read_data(&format!(
                    r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 2) {{
                    _docID lifecycle_state execution_generation execution_lease_expires_at
                    terminal_output failure_reason
                }}
                InferenceCall(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 10) {{
                    call_id call_state failure_reason started_at ended_at
                }}
                AgentToolCall(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 10) {{
                    _docID tool_name lifecycle_state started_at completed_at
                }}
            }}"#
                ))
                .await;
            panic!("timed out waiting for request {request_id} to terminalize; last={last}; evidence={evidence}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

pub async fn terminal_assistant_answer<R>(reader: &R, request_id: &str) -> String
where
    R: SnapshotReader + ?Sized,
{
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 2) {{
                _docID node_did requester_did session_id terminal_output
            }}
        }}"#
    );
    #[derive(Deserialize)]
    struct RequestRow {
        #[serde(rename = "_docID")]
        doc_id: String,
        node_did: String,
        requester_did: Option<String>,
        session_id: Option<String>,
        terminal_output: Option<gents_protocol::output::TerminalOutput>,
    }
    let data = reader.read_data(&query).await;
    let row = optional_row::<RequestRow>(&data, "AgentRequest");
    let Some(row) = row else { return String::new() };
    let session_id = match row.session_id {
        Some(s) if !s.is_empty() => s,
        _ => return String::new(),
    };
    let escaped_session = escape_graphql_string(&session_id);
    let requester_filter = row
        .requester_did
        .as_deref()
        .map(|did| {
            format!(
                r#", requester_did: {{ _eq: "{}" }}"#,
                escape_graphql_string(did)
            )
        })
        .unwrap_or_else(|| ", requester_did: { _eq: null }".to_owned());
    let Some(gents_protocol::output::TerminalOutput::Message { message_doc_id }) =
        row.terminal_output
    else {
        return String::new();
    };
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ _docID: {{ _eq: "{}" }}, request_doc_id: {{ _eq: "{}" }}, session_id: {{ _eq: "{escaped_session}" }}, node_did: {{ _eq: "{}" }}, role: {{ _eq: "assistant" }}{requester_filter} }}, limit: 2
            ) {{ message_key session_id node_did requester_did request_doc_id publication outcome sequence role native_id blocks created_at }}
            AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }}, node_did: {{ _eq: "{}" }}{requester_filter} }}) {{ _docID node_did requester_did session_id request_doc_id source ordinal writer runs payload close created_at }}
        }}"#,
        escape_graphql_string(&message_doc_id),
        escape_graphql_string(&row.doc_id),
        escape_graphql_string(&row.node_did),
        escape_graphql_string(&row.doc_id),
        escape_graphql_string(&row.node_did)
    );
    let data = reader.read_data(&query).await;
    let headers = data["AgentMessage"]
        .as_array()
        .expect("canonical live answer query omitted headers");
    assert_eq!(
        headers.len(),
        1,
        "terminal output did not select one physical assistant header"
    );
    let header = serde_json::from_value::<TranscriptMessage>(headers[0].clone())
        .expect("decode selected canonical terminal header");
    let observed = data["AgentOutputSegment"]
        .as_array()
        .expect("canonical live answer query omitted segments")
        .iter()
        .map(|value| {
            let doc_id = value["_docID"]
                .as_str()
                .expect("canonical segment omitted physical ID")
                .to_owned();
            let mut segment = value.clone();
            segment
                .as_object_mut()
                .expect("canonical segment row object")
                .remove("_docID");
            (
                doc_id,
                serde_json::from_value::<OutputSegment>(segment)
                    .expect("decode canonical output segment"),
            )
        })
        .collect::<Vec<_>>();
    let observations = observed
        .iter()
        .map(|(doc_id, segment)| ObservedSegment { doc_id, segment })
        .collect::<Vec<_>>();
    let message = reconstruct_message(&observations, &[], &[], &header)
        .expect("reconstruct selected canonical terminal message");
    gents_protocol::transcript::present_message(&message).body_markdown
}

pub async fn wait_for_assistant_answer<R>(reader: &R, request_id: &str, timeout: Duration) -> String
where
    R: SnapshotReader + ?Sized,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let answer = terminal_assistant_answer(reader, request_id).await;
        if !answer.trim().is_empty() {
            return answer;
        }
        if tokio::time::Instant::now() >= deadline {
            return answer;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// A live node's User: the client key `gents init` creates, which is the
/// node's native admin (`GrantSet::HouseholdOwner`). It configures the node's
/// Agents and inference documents, submits the User's requests and observes
/// the node, whose ACP-governed rows its admin standing reads.
pub struct LiveUser {
    pub db_node: Arc<gents::defra_node::Node>,
    pub runtime: Arc<dyn NodeIdentity>,
    pub user: gents::KeyIdentity,
    pub access: gents::ConfigAccess,
    pub node_did: String,
}

/// One Agent a [`LiveUser`] configures on its node.
pub struct LiveAgentSpec<'a> {
    pub display_name: &'a str,
    pub inference_profile_id: &'a str,
    pub system_prompt: &'a str,
    pub description: Option<&'a str>,
    pub default_for_node: bool,
}

impl LiveUser {
    /// Makes the node's fixture User its household owner, then writes the
    /// Node document and the target's backend as that User.
    pub async fn install(db: &TestDb, target: &InferenceTarget) -> Self {
        let user = crate::support::managed_fixture_user(db).await;
        crate::support::grant_household_owner(db, &user).await;
        let access = db.client_access(&user);
        let node_did = db.node_identity.did().to_owned();
        let live = Self {
            db_node: db.node.clone(),
            runtime: db.node_identity.clone(),
            user,
            access,
            node_did,
        };
        live.apply(
            "test.live_user.node",
            vec![
                (
                    Collection::Node,
                    serde_json::json!({ "node_did": live.node_did }),
                ),
                (
                    Collection::InferenceBackend,
                    serde_json::to_value(target.backend(&live.node_did))
                        .expect("serialize live backend"),
                ),
            ],
        )
        .await;
        live
    }

    pub fn user_did(&self) -> &str {
        self.user.did()
    }

    /// Applies desired-state documents as the User.
    pub async fn apply(
        &self,
        operation: &'static str,
        documents: Vec<(Collection, serde_json::Value)>,
    ) {
        use gents::documents::apply_desired_state_plan;
        use gents::store::config_client::DesiredStateApplyDocument;
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
        .expect("build live fixture plan");
        self.access
            .transact_write(operation, |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
            })
            .await
            .unwrap_or_else(|error| panic!("{operation} as the node's User: {error:#}"));
    }

    /// Provisions one managed Agent under the User and configures its
    /// high-effort live profile, sampling and context. Returns its DID.
    pub async fn agent(&self, target: &InferenceTarget, spec: LiveAgentSpec<'_>) -> String {
        use gents::documents::AgentContext;
        let user_did = ::identity::Did::new(self.user.did()).expect("valid User DID");
        let binding = gents::store::identity::provision_agent_identity_authorized(
            &self.db_node,
            self.runtime.as_ref(),
            &user_did,
        )
        .await
        .expect("provision a live Agent under its User");
        let context_id = format!("{}:context", binding.agent_did);
        let sampling_id = format!("{}:live-sampling", binding.agent_did);
        let profile = InferenceProfile {
            profile_id: spec.inference_profile_id.to_string(),
            sampling_id: Some(sampling_id.clone()),
            reasoning_effort: Some(gents::ReasoningEffort::High),
            ..target.profile(&self.node_did)
        };
        let context = AgentContext {
            context_id: context_id.clone(),
            node_did: self.node_did.clone(),
            display_name: None,
            description: None,
            system_prompt: Some(spec.system_prompt.to_string()),
            tools_id: None,
            compaction_id: None,
            skill_ids: Vec::new(),
            tags: Vec::new(),
        };
        let agent = Agent {
            agent_did: binding.agent_did.clone(),
            creation_nonce: binding.creation_nonce,
            node_did: self.node_did.clone(),
            display_name: Some(spec.display_name.to_string()),
            description: spec.description.map(ToOwned::to_owned),
            context_id: Some(context_id),
            inference_profile_id: spec.inference_profile_id.to_string(),
            enabled: true,
            tags: Vec::new(),
            created_at: Some(crate::support::agent_requests::canonical_now()),
        };
        let mut documents = vec![
            (
                Collection::InferenceSampling,
                serde_json::to_value(target.live_sampling(&self.node_did, &sampling_id))
                    .expect("serialize live sampling"),
            ),
            (
                Collection::InferenceProfile,
                serde_json::to_value(profile).expect("serialize live profile"),
            ),
            (
                Collection::AgentContext,
                serde_json::to_value(context).expect("serialize live context"),
            ),
            (
                Collection::Agent,
                serde_json::to_value(agent).expect("serialize live Agent"),
            ),
        ];
        if spec.default_for_node {
            documents.push((
                Collection::Node,
                serde_json::json!({
                    "node_did": self.node_did,
                    "default_agent_did": binding.agent_did,
                }),
            ));
        }
        self.apply("test.live_user.agent", documents).await;
        // Production projects the baseline from the live Agent document;
        // fixtures with no projection pass grant the same set as the node admin.
        gents::store::identity::GrantSet::AgentBaseline
            .grant(
                &gents::ConfigAccess::local_as(
                    self.db_node.clone(),
                    ::identity::Did::new(&self.node_did).expect("valid runtime DID"),
                ),
                &::identity::Did::new(&binding.agent_did).expect("valid Agent DID"),
            )
            .await
            .expect("grant the live Agent its baseline");
        binding.agent_did
    }

    /// Submits the User's message to `agent_did` through the client request
    /// owner and returns the request's physical ID.
    pub async fn request(
        &self,
        agent_did: &str,
        request_id: &str,
        session_id: &str,
        content: &str,
    ) -> String {
        crate::support::agent_requests::submit_user_request_to(
            &self.access,
            &self.user,
            &self.node_did,
            agent_did,
            session_id,
            request_id,
            content,
            gents_protocol::request_input::QueueDelivery::Queue,
        )
        .await
    }

    /// The Node document as the User reads it.
    pub async fn node_config(&self) -> Node {
        let node_did = self.node_did.clone();
        self.access
            .transact_readonly("test.live_user.read_node", |txn| {
                let node_did = &node_did;
                Box::pin(async move {
                    let (_, node) = gents::store::config_client::read_desired_state_record_in_txn(
                        txn,
                        Collection::Node,
                        node_did,
                        node_did,
                    )
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("the live Node is missing"))?;
                    Ok::<_, anyhow::Error>(serde_json::from_value::<Node>(node)?)
                })
            })
            .await
            .expect("read the live Node as its User")
    }

    /// Reserves the User's session on `agent_did` before its first message,
    /// as a client does to attach a Goal to a new session.
    pub async fn reserve_session(&self, agent_did: &str, session_id: &str) -> String {
        use gents_protocol::request_admission::{
            AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
        };
        let request = AgentRequestCreate::base(
            RequestPurpose::Normal,
            &format!("{session_id}:reservation"),
            &self.node_did,
            self.user.did(),
            agent_did,
            session_id,
            "",
            "interactive",
            crate::support::agent_requests::canonical_now(),
            AgentRequestAdmissionRecord::client(self.user.did()),
        );
        gents::request::lifecycle::reserve_goal_session(&self.access, &request)
            .await
            .expect("reserve the User's session")
    }

    /// Reads as the User, shaped like a native response for observation
    /// helpers that inspect `data` and `errors`.
    pub async fn execute(&self, query: &str) -> gents::defra_node::QueryResponse {
        let value = match self.access.execute(query).await {
            Ok(value) => value,
            Err(error) => serde_json::json!({
                "data": null,
                "errors": [{ "message": format!("{error:#}") }],
            }),
        };
        serde_json::from_value(value).expect("decode the User's GraphQL response")
    }
}

impl SnapshotReader for LiveUser {
    async fn read_data(&self, query: &str) -> serde_json::Value {
        self.access.read_data(query).await
    }
}
