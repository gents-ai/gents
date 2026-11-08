//! Signed request fixtures. The enrolled User configures the Node and its
//! Agent; the Agent authors its own local-self sessions and requests through its
//! managed signer, and every lifecycle write is signed by that Agent.

use std::sync::Arc;

use gents::request::lifecycle::ExecutionOrigin;
use gents::request::watcher::AgentRequest;
use gents::store::graphql::escape_graphql_string;
use gents::{ConfigAccess, NodeIdentity, RequestLifecycle};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;

use super::{ManagedAgentFixture, TestDb};

/// Canonical UTC seconds, the only `created_at` form the signed owners accept.
pub fn canonical_now() -> String {
    canonical_at(chrono::Utc::now())
}

pub fn canonical_at(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// One request row to seed. Unset fields keep the defaults of
/// [`AgentRequestCreate::base`].
#[derive(Clone, Debug, Default)]
pub struct RequestSeed<'a> {
    pub request_id: &'a str,
    pub session_id: &'a str,
    pub lifecycle_state: Option<&'a str>,
    pub created_at: &'a str,
    pub valid_until: Option<&'a str>,
    pub input: Option<&'a str>,
    pub content: Option<&'a str>,
    pub retry_parent_request: Option<&'a str>,
    pub retry_root_request: Option<&'a str>,
    pub execution_origin: Option<&'a str>,
}

impl<'a> RequestSeed<'a> {
    pub fn new(request_id: &'a str, session_id: &'a str, created_at: &'a str) -> Self {
        Self {
            request_id,
            session_id,
            created_at,
            ..Self::default()
        }
    }

    pub fn state(mut self, lifecycle_state: &'a str) -> Self {
        self.lifecycle_state = Some(lifecycle_state);
        self
    }

    pub fn valid_until(mut self, valid_until: Option<&'a str>) -> Self {
        self.valid_until = valid_until;
        self
    }
}

fn parse_state(state: &str) -> RequestLifecycleState {
    RequestLifecycleState::parse(state).expect("lifecycle state")
}

fn is_pre_claim(state: RequestLifecycleState) -> bool {
    matches!(
        state,
        RequestLifecycleState::Pending | RequestLifecycleState::WorkspaceBindingPending
    )
}

pub struct RequestActors {
    pub node: Arc<gents::defra_node::Node>,
    pub fixture: ManagedAgentFixture,
    /// The Agent's own signed access; it owns its sessions and requests.
    pub agent_access: ConfigAccess,
    pub agent_doc: String,
    node_did: String,
}

impl RequestActors {
    pub async fn new(db: &TestDb) -> Self {
        let fixture = super::managed_agent_fixture(db).await;
        Self::from_fixture(db, fixture).await
    }

    pub async fn from_fixture(db: &TestDb, fixture: ManagedAgentFixture) -> Self {
        let node_did = db.node_identity.did().to_owned();
        let runtime = ConfigAccess::local_as(
            db.node.clone(),
            identity::Did::new(&node_did).expect("valid runtime DID"),
        );
        // Production projects the baseline from the live Agent document;
        // fixtures with no projection pass grant the same set as the node admin.
        gents::store::identity::GrantSet::AgentBaseline
            .grant(
                &runtime,
                &identity::Did::new(fixture.agent.did()).expect("valid Agent DID"),
            )
            .await
            .expect("grant fixture Agent baseline");
        let agent_access = ConfigAccess::local_managed_agent(
            db.node.clone(),
            fixture.agent.did().parse().expect("valid Agent DID"),
            fixture.agent.as_ref(),
        )
        .expect("managed fixture Agent access");
        let agent_doc = fixture
            .access
            .transact_readonly("test.agent_requests.agent", |txn| {
                let node_did = &node_did;
                let agent_did = fixture.agent.did();
                Box::pin(async move {
                    Ok::<_, anyhow::Error>(
                        gents::store::config_client::read_desired_state_record_in_txn(
                            txn,
                            gents::Collection::Agent,
                            node_did,
                            agent_did,
                        )
                        .await?
                        .expect("fixture Agent")
                        .0,
                    )
                })
            })
            .await
            .expect("read fixture Agent");
        Self {
            node: db.node.clone(),
            fixture,
            agent_access,
            agent_doc,
            node_did,
        }
    }

    pub fn node_did(&self) -> &str {
        &self.node_did
    }

    pub fn agent_did(&self) -> &str {
        self.fixture.agent.did()
    }

    pub fn agent(&self) -> &Arc<dyn NodeIdentity> {
        &self.fixture.agent
    }

    /// The Agent-owned session for `session_id`, created on first use.
    pub async fn session(&self, session_id: &str, created_at: &str) -> String {
        if let Some(doc) = self.session_doc(session_id).await {
            return doc;
        }
        self.create_session_with(serde_json::json!({
            "node_did": self.node_did,
            "session_id": session_id,
            "requester_did": self.agent_did(),
            "agent_did": self.agent_did(),
            "created_at": created_at,
            "relationships_pending": false,
        }))
        .await
    }

    /// Writes one session row as the Agent and relates it to that Agent.
    pub async fn create_session_with(&self, input: serde_json::Value) -> String {
        let agent_did = self.agent_did().to_owned();
        self.agent_access
            .transact_write("test.agent_requests.session", |txn| {
                let agent_doc = &self.agent_doc;
                let agent_did = &agent_did;
                let input = &input;
                Box::pin(async move {
                    let response = txn
                        .execute_with_variables(
                            "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) {_docID} }",
                            &serde_json::json!({"input": input}),
                        )
                        .await?;
                    let session_doc =
                        gents::store::graphql::created_doc_id(&response, "AgentSession")?;
                    txn.add_resource_relationship(
                        "AgentSession",
                        &session_doc,
                        "parent",
                        "agents",
                        agent_doc,
                    )
                    .await?;
                    txn.add_actor_relationship(
                        "AgentSession",
                        &session_doc,
                        "working_agent",
                        &agent_did.parse()?,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(session_doc)
                })
            })
            .await
            .expect("create Agent-owned session")
    }

    pub async fn session_doc(&self, session_id: &str) -> Option<String> {
        let response = self
            .agent_access
            .execute(&format!(
                r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}, limit: 2) {{ _docID }} }}"#,
                escape_graphql_string(session_id)
            ))
            .await
            .expect("read Agent-owned session");
        let rows = response["data"]["AgentSession"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(rows.len() <= 1, "session id resolves to one session");
        rows.first()
            .and_then(|row| row["_docID"].as_str())
            .map(str::to_owned)
    }

    pub fn request_create(&self, seed: &RequestSeed<'_>) -> AgentRequestCreate {
        let agent = self.agent_did();
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            seed.request_id,
            &self.node_did,
            agent,
            agent,
            seed.session_id,
            seed.content.unwrap_or("hello"),
            seed.execution_origin.unwrap_or("interactive"),
            seed.created_at,
            AgentRequestAdmissionRecord::local_self(agent),
        );
        create.max_retries = gents::request::lifecycle::DEFAULT_REQUEST_MAX_RETRIES.into();
        create.valid_until = seed.valid_until.map(str::to_owned);
        if let Some(input) = seed.input {
            create.input = serde_json::from_str(input).expect("request input JSON");
        }
        if let Some(parent) = seed.retry_parent_request {
            create.retry_parent_request = Some(parent.to_owned());
        }
        if let Some(root) = seed.retry_root_request {
            create.retry_root_request = Some(root.to_owned());
        }
        if let Some(state) = seed.lifecycle_state.map(parse_state) {
            if is_pre_claim(state) {
                create.initial_lifecycle_state = state;
            }
        }
        create
    }

    /// Seeds a signed request in an Agent-owned session and returns its doc ID.
    /// Admission accepts only pre-claim states; a later seeded state is the
    /// working Agent's own update of the admitted row.
    pub async fn create_request(&self, seed: RequestSeed<'_>) -> String {
        let create = self.request_create(&seed);
        let doc_id = self.create_signed(create).await;
        if let Some(state) = seed.lifecycle_state.map(parse_state) {
            if !is_pre_claim(state) {
                self.set_lifecycle_state(&doc_id, state).await;
            }
        }
        doc_id
    }

    pub async fn create_signed(&self, mut create: AgentRequestCreate) -> String {
        let session_doc = self.session(&create.session_id, &create.created_at).await;
        create.agent_doc_id = Some(self.agent_doc.clone());
        create.session_doc_id = Some(session_doc.clone());
        gents::sign_agent_request_create(self.fixture.agent.as_ref(), &mut create)
            .await
            .expect("sign fixture request");
        let agent_did = self.agent_did().to_owned();
        self.agent_access
            .transact_write("test.agent_requests.request", |txn| {
                let create = &create;
                let agent_doc = &self.agent_doc;
                let session_doc = &session_doc;
                let agent_did = &agent_did;
                Box::pin(async move {
                    let response = txn
                        .execute(&create.graphql_mutation().map_err(anyhow::Error::msg)?)
                        .await?;
                    let doc = gents::store::graphql::created_doc_id(&response, "AgentRequest")?;
                    txn.add_resource_relationship(
                        "AgentRequest",
                        &doc,
                        "parent",
                        "agents",
                        agent_doc,
                    )
                    .await?;
                    txn.add_resource_relationship(
                        "AgentRequest",
                        &doc,
                        "session",
                        "sessions",
                        session_doc,
                    )
                    .await?;
                    txn.add_actor_relationship(
                        "AgentRequest",
                        &doc,
                        "working_agent",
                        &agent_did.parse()?,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(doc)
                })
            })
            .await
            .expect("create signed fixture request")
    }

    /// The watcher view of a seeded request, bound to the fixture's actors.
    pub fn build_request(
        &self,
        doc_id: String,
        request_id: String,
        session_id: String,
        created_at: String,
    ) -> AgentRequest {
        let mut request = super::build_request(doc_id, request_id, session_id, created_at);
        request.node_did = self.node_did.clone();
        request.agent_did = self.agent_did().to_owned();
        request.requester_did = Some(self.agent_did().to_owned());
        request.execution_origin = Some("interactive".into());
        request
    }

    /// A lifecycle whose writes are signed by the fixture Agent.
    pub fn lifecycle(
        &self,
        request: AgentRequest,
        deadline_secs: u64,
        origin: ExecutionOrigin,
        backend_id: &str,
    ) -> RequestLifecycle {
        let mut lifecycle = RequestLifecycle::new_with_execution_binding(
            self.node.clone(),
            self.agent_did(),
            &self.node_did,
            request,
            deadline_secs,
            origin,
            backend_id,
        );
        lifecycle
            .bind_managed_agent(self.fixture.agent.as_ref())
            .expect("bind fixture Agent signer");
        lifecycle
    }

    /// Patches request fields as the working Agent, the only actor that
    /// updates a claimed or pending request it owns.
    pub async fn update_request(&self, doc_id: &str, input: serde_json::Value) {
        let input = gents_protocol::graphql::graphql_input_literal(&input).expect("render input");
        self.agent_access
            .write(
                "test.agent_requests.update",
                &format!(
                    r#"mutation {{ update_AgentRequest(docID: "{}", input: {input}) {{ _docID }} }}"#,
                    escape_graphql_string(doc_id)
                ),
            )
            .await
            .expect("update fixture request as working Agent");
    }

    pub async fn set_interrupt_requested_at(&self, doc_id: &str, at: &str) {
        self.update_request(doc_id, serde_json::json!({"interrupt_requested_at": at}))
            .await;
    }

    pub async fn set_lifecycle_state(&self, doc_id: &str, state: RequestLifecycleState) {
        self.update_request(doc_id, serde_json::json!({"lifecycle_state": state}))
            .await;
    }

    /// Replaces the session observation as the Agent that owns the session.
    pub async fn seed_session_observation(
        &self,
        session_id: &str,
        observation: &gents_protocol::session::SessionObservation,
    ) {
        let session_doc = self
            .session_doc(session_id)
            .await
            .expect("seeded session exists");
        let input = gents_protocol::graphql::graphql_input_literal(
            &serde_json::json!({"observation": observation}),
        )
        .expect("render session observation fixture");
        self.agent_access
            .write(
                "test.agent_requests.session_observation",
                &format!(
                    r#"mutation {{ update_AgentSession(docID: "{}", input: {input}) {{ _docID }} }}"#,
                    escape_graphql_string(&session_doc)
                ),
            )
            .await
            .expect("seed session observation as Agent");
    }

    /// Projects `request_id` into its session's observation as the session's Agent.
    pub async fn seed_session_observation_from_request(
        &self,
        session_id: &str,
        request_id: &str,
        preview: &str,
    ) {
        let response = self
            .agent_access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }}, request_id: {{ _eq: "{}" }} }}, limit: 2) {{ _docID lifecycle_state created_at }} }}"#,
                escape_graphql_string(session_id),
                escape_graphql_string(request_id),
            ))
            .await
            .expect("read request for observation");
        let rows = response["data"]["AgentRequest"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            rows.len(),
            1,
            "fixture needs an unambiguous physical request"
        );
        let row = &rows[0];
        let observation = gents_protocol::session::SessionObservation {
            reply_preview: None,
            last_activity_at: row["created_at"]
                .as_str()
                .expect("creation time")
                .to_owned(),
            preview: Some(preview.to_owned()),
            latest_request: Some(gents_protocol::session::SessionRequestObservation {
                request_doc_id: row["_docID"].as_str().expect("physical request").to_owned(),
                request_id: request_id.to_owned(),
                lifecycle_state: serde_json::from_value(row["lifecycle_state"].clone())
                    .expect("canonical lifecycle"),
            }),
        };
        self.seed_session_observation(session_id, &observation)
            .await;
    }

    /// Titles the Agent-owned session, so the runtime schedules no title turn.
    pub async fn set_session_title(
        &self,
        session_id: &str,
        title: &gents_protocol::session::SessionTitle,
    ) {
        let session_doc = self
            .session_doc(session_id)
            .await
            .expect("seeded session exists");
        let input =
            gents_protocol::graphql::graphql_input_literal(&serde_json::json!({"title": title}))
                .expect("render session title fixture");
        self.agent_access
            .write(
                "test.agent_requests.session_title",
                &format!(
                    r#"mutation {{ update_AgentSession(docID: "{}", input: {input}) {{ _docID }} }}"#,
                    escape_graphql_string(&session_doc)
                ),
            )
            .await
            .expect("title session as its Agent");
    }

    /// Publishes one completed transcript message into the Agent-owned
    /// session as that Agent: its text segment, then the message citing it,
    /// each related to the session and its working Agent.
    pub async fn publish_history_message(
        &self,
        session_id: &str,
        sequence: u32,
        role: gents_protocol::output::MessageRole,
        content: &str,
        created_at: &str,
    ) {
        use gents::session::canonical_rows::{
            output_segment_create_variables, transcript_message_create_variables,
            CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        };
        use gents_protocol::output::{
            MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment,
            OutputSource, OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload,
            SegmentRun, SourceClose, StreamDeclaration, StreamPayload, TranscriptMessage,
        };
        use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

        let session_doc = self.session(session_id, created_at).await;
        let agent_did = self.agent_did().to_owned();
        let requester_did = Some(agent_did.clone());
        let generation = "fixture-history".to_owned();
        let request_doc_id = format!("fixture-request:{session_id}:{sequence}");
        let segment = OutputSegment {
            node_did: self.node_did.clone(),
            requester_did: requester_did.clone(),
            session_id: session_id.into(),
            request_doc_id: request_doc_id.clone(),
            source: if role == MessageRole::Assistant {
                OutputSource::ProviderTurn {
                    scope: CaptureScope {
                        kind: CaptureScopeKind::Inference,
                        seq: u64::from(sequence),
                    },
                    turn_index: 0,
                    attempt: 0,
                }
            } else {
                OutputSource::Authored {
                    key: format!("fixture:{sequence}"),
                }
            },
            writer: OutputWriter::RequestExecution {
                execution_generation: generation.clone(),
            },
            ordinal: Some(0),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: content.len().try_into().expect("fixture payload size"),
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::Text,
                }),
            }],
            payload: content.into(),
            close: Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![content.len() as u64],
            }),
            created_at: created_at.into(),
        };
        self.agent_access
            .transact_write("test.agent_requests.history_message", |txn| {
                let segment = &segment;
                let session_doc = &session_doc;
                let agent_did = &agent_did;
                let requester_did = requester_did.as_deref();
                let node_did = &self.node_did;
                let generation = &generation;
                let request_doc_id = &request_doc_id;
                Box::pin(async move {
                    let bind = |mut variables: serde_json::Value| {
                        variables["input"]["session_doc_id"] = session_doc.as_str().into();
                        variables["input"]["agent_did"] = agent_did.as_str().into();
                        variables["input"]["creator_did"] = agent_did.as_str().into();
                        variables["input"]["parent_linked"] = true.into();
                        variables
                    };
                    let relate = |collection: &'static str, doc: String| async move {
                        txn.add_resource_relationship(
                            collection,
                            &doc,
                            "parent",
                            "sessions",
                            session_doc,
                        )
                        .await?;
                        txn.add_actor_relationship(
                            collection,
                            &doc,
                            "working_agent",
                            &agent_did.parse()?,
                        )
                        .await?;
                        Ok::<_, anyhow::Error>(doc)
                    };
                    let created = txn
                        .execute_with_variables(
                            CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                            &bind(output_segment_create_variables(segment)?),
                        )
                        .await?;
                    let close_doc_id = relate(
                        "AgentOutputSegment",
                        gents::store::graphql::created_doc_id(&created, "AgentOutputSegment")?,
                    )
                    .await?;
                    let message = TranscriptMessage {
                        native_request_id: None,
                        message_key: gents::session::sequence_message_key(
                            node_did,
                            session_id,
                            requester_did,
                            sequence,
                        ),
                        session_id: session_id.into(),
                        node_did: node_did.clone(),
                        requester_did: requester_did.map(Into::into),
                        request_doc_id: Some(request_doc_id.clone()),
                        publication: MessagePublication::RequestExecution {
                            execution_generation: generation.clone(),
                        },
                        outcome: OutputOutcome::Complete,
                        sequence,
                        role,
                        native_id: (role == MessageRole::Assistant)
                            .then(|| format!("native-{sequence}")),
                        blocks: vec![MessageBlock::Text {
                            text: PresentedPayload {
                                output: PayloadRef {
                                    close_doc_id,
                                    stream: 0,
                                },
                                presentation: PayloadPresentation::Full,
                            },
                        }],
                        created_at: segment.created_at.clone(),
                    };
                    let created = txn
                        .execute_with_variables(
                            CREATE_AGENT_MESSAGE_MUTATION,
                            &bind(transcript_message_create_variables(&message)?),
                        )
                        .await?;
                    relate(
                        "AgentMessage",
                        gents::store::graphql::created_doc_id(&created, "AgentMessage")?,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .expect("publish history message as the session Agent");
    }

    /// Runs a read as the working Agent, shaped like a native query response.
    pub async fn query(&self, query: &str) -> gents::defra_node::QueryResponse {
        let response = self
            .agent_access
            .execute(query)
            .await
            .expect("query as working Agent");
        serde_json::from_value(response).expect("query response shape")
    }

    /// Reads one request row as the working Agent.
    pub async fn request_row(&self, doc_id: &str, fields: &str) -> serde_json::Value {
        let response = self
            .agent_access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ {fields} }} }}"#,
                escape_graphql_string(doc_id)
            ))
            .await
            .expect("read fixture request as working Agent");
        response["data"]["AgentRequest"][0].clone()
    }
}

/// Submits one User message through the client request owner, as desktop and
/// the CLI do: the fixture User signs it as the requester and the client
/// admission reserves its session for the fixture Agent.
pub async fn submit_user_request(
    node_did: &str,
    fixture: &ManagedAgentFixture,
    session: &str,
    id: &str,
    text: &str,
    delivery: gents_protocol::request_input::QueueDelivery,
) -> String {
    submit_user_request_to(
        &fixture.access,
        &fixture.user,
        node_did,
        &fixture.binding.agent_did,
        session,
        id,
        text,
        delivery,
    )
    .await
}

/// [`submit_user_request`] to any Agent of the User's node.
#[allow(clippy::too_many_arguments)]
pub async fn submit_user_request_to(
    access: &ConfigAccess,
    user: &gents::KeyIdentity,
    node_did: &str,
    agent: &str,
    session: &str,
    id: &str,
    text: &str,
    delivery: gents_protocol::request_input::QueueDelivery,
) -> String {
    let requester = user.did();
    let mut request = AgentRequestCreate::base(
        RequestPurpose::Normal,
        id,
        node_did,
        requester,
        agent,
        session,
        text,
        "interactive",
        canonical_now(),
        AgentRequestAdmissionRecord::client(requester),
    );
    request.input = gents::request::lifecycle::prepare_user_message_input(
        access,
        node_did,
        session,
        request.input,
        delivery,
    )
    .await
    .expect("prepare the User's message input");
    request.session_doc_id = Some(
        gents::request::lifecycle::reserve_goal_session(access, &request)
            .await
            .expect("reserve the User's session"),
    );
    let agent_doc = access
        .transact_readonly("test.user_request.agent", |txn| {
            Box::pin(async move {
                Ok::<_, anyhow::Error>(
                    gents::store::config_client::read_desired_state_record_in_txn(
                        txn,
                        gents::Collection::Agent,
                        node_did,
                        agent,
                    )
                    .await?
                    .expect("selected Agent exists")
                    .0,
                )
            })
        })
        .await
        .expect("read the selected Agent as its User");
    request.agent_doc_id = Some(agent_doc);
    gents::sign_agent_request_create(user, &mut request)
        .await
        .expect("sign the User's request");
    gents::request::lifecycle::submit_client_request(access, &request)
        .await
        .expect("submit the User's request");
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            escape_graphql_string(id)
        ))
        .await
        .expect("read the User's request");
    response["data"]["AgentRequest"][0]["_docID"]
        .as_str()
        .expect("submitted request is visible to its User")
        .to_owned()
}
