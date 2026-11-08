//! Builder-assembled runtimes over a real fixture Agent: the enrolled User
//! configures the Node, its Agent and the Agent's backend; the runtime runs that
//! Agent under its creation binding with fresh backend admission; requests and
//! reads go through the Agent's own signed access.

use gents::{AgentBuilder, Gents, GentsBuilder};

use super::agent_requests::{canonical_now, RequestActors, RequestSeed};
use super::interrupt::{wait_for_runtime_ready, BootedAgent};
use super::TestDb;

/// Enrols the fixture User, its Node and one Agent, then binds that Agent to
/// `backend_id` with `max_concurrent` admission slots as the User.
pub async fn builder_actors(
    db: &TestDb,
    backend_id: &str,
    endpoint: &str,
    model_name: &str,
    max_concurrent: usize,
) -> RequestActors {
    let actors = RequestActors::new(db).await;
    super::fixtures::bind_managed_agent_backend(
        db,
        &actors.fixture,
        backend_id,
        endpoint,
        model_name,
    )
    .await;
    if max_concurrent != 1 {
        set_backend_capacity(db, &actors, backend_id, max_concurrent).await;
    }
    actors
}

async fn set_backend_capacity(
    db: &TestDb,
    actors: &RequestActors,
    backend_id: &str,
    max_concurrent: usize,
) {
    use gents::documents::apply_desired_state_plan;
    use gents::store::config_client::{
        read_desired_state_record_in_txn, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    let owner = db.node_identity.did();
    actors
        .fixture
        .access
        .transact_write("test.builder_runtime.backend_capacity", |txn| {
            Box::pin(async move {
                let (_, mut backend) = read_desired_state_record_in_txn(
                    txn,
                    gents::Collection::InferenceBackend,
                    owner,
                    backend_id,
                )
                .await?
                .expect("bound fixture backend");
                backend["max_concurrent"] = max_concurrent.into();
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: gents::Collection::InferenceBackend,
                    add: backend.clone(),
                    update: backend,
                }])?;
                apply_desired_state_plan(txn, &plan).await.map(|_| ())
            })
        })
        .await
        .expect("configure fixture backend capacity as its User");
}

/// A runtime builder for the test Node whose default is the fixture Agent.
pub fn runtime_builder(db: &TestDb, actors: &RequestActors) -> GentsBuilder {
    Gents::builder()
        .node(db.node.clone())
        .backend_admission(db.home.backend_admission())
        .identity(db.node_identity.clone())
        .default_agent_did(actors.agent_did())
}

/// The fixture Agent, bound to the creation nonce its User published. `name`
/// is the stable label the runtime presents for it.
pub fn fixture_agent(builder: GentsBuilder, actors: &RequestActors, name: &str) -> AgentBuilder {
    builder
        .agent(name)
        .identity_binding(actors.fixture.binding.clone())
}

pub async fn spawn_runtime(db: &TestDb, runtime: Gents) -> BootedAgent {
    let node_did = runtime.node_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(runtime.run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &node_did).await;
    BootedAgent::new(shutdown_tx, handle, node_did)
}

/// Seeds one pending request in an Agent-owned, already titled session, so no
/// title generation reaches the scripted provider.
pub async fn submit_request(
    actors: &RequestActors,
    request_id: &str,
    session_id: &str,
    execution_origin: &str,
    content: &str,
) -> String {
    let created_at = canonical_now();
    if actors.session_doc(session_id).await.is_none() {
        let title = gents_protocol::session::SessionTitle {
            text: "generated-title".into(),
            source: gents_protocol::session::SessionTitleSource::Generated,
        };
        actors
            .create_session_with(serde_json::json!({
                "node_did": actors.node_did(),
                "session_id": session_id,
                "requester_did": actors.agent_did(),
                "agent_did": actors.agent_did(),
                "created_at": created_at,
                "relationships_pending": false,
                "title": title,
            }))
            .await;
    }
    let mut seed = RequestSeed::new(request_id, session_id, &created_at);
    seed.content = Some(content);
    seed.execution_origin = Some(execution_origin);
    actors.create_request(seed).await
}

/// Writes one closed history message into an Agent-owned session as that
/// Agent, related to the session like the runtime's own transcript rows.
pub async fn seed_session_message(
    actors: &RequestActors,
    session_id: &str,
    sequence: u32,
    role: &str,
    content: &str,
    timestamp: &str,
) {
    use gents_protocol::output::rows::{
        output_segment_create_variables, transcript_message_create_variables,
        CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
        OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
        StreamDeclaration, StreamPayload, TranscriptMessage,
    };
    use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

    let role = match role {
        "user" => MessageRole::User,
        "assistant" => MessageRole::Assistant,
        other => panic!("unsupported seeded message role: {other}"),
    };
    let session_doc = actors
        .session_doc(session_id)
        .await
        .expect("seeded history needs its Agent-owned session");
    let node_did = actors.node_did().to_owned();
    let agent_did = actors.agent_did().to_owned();
    let requester_did = Some(agent_did.as_str());
    let request_doc_id = format!("fixture-request:{session_id}:{sequence}");
    let generation = "fixture-generation".to_owned();
    let segment = OutputSegment {
        node_did: node_did.clone(),
        requester_did: requester_did.map(Into::into),
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
            bytes: content.len().try_into().expect("segment length"),
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
        created_at: timestamp.into(),
    };
    actors
        .agent_access
        .transact_write("test.builder_runtime.session_message", |txn| {
            let segment = &segment;
            let session_doc = &session_doc;
            let agent_did = &agent_did;
            let node_did = &node_did;
            let generation = &generation;
            let request_doc_id = &request_doc_id;
            Box::pin(async move {
                let agent: identity::Did = agent_did.parse()?;
                let mut variables = output_segment_create_variables(segment)?;
                variables["input"]["session_doc_id"] = serde_json::json!(session_doc);
                variables["input"]["agent_did"] = serde_json::json!(agent_did);
                variables["input"]["creator_did"] = serde_json::json!(agent_did);
                variables["input"]["parent_linked"] = serde_json::json!(true);
                let created = txn
                    .execute_with_variables(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION, &variables)
                    .await?;
                let close_doc_id =
                    gents::store::graphql::created_doc_id(&created, "AgentOutputSegment")?;
                txn.add_resource_relationship(
                    "AgentOutputSegment",
                    &close_doc_id,
                    "parent",
                    "sessions",
                    session_doc,
                )
                .await?;
                txn.add_actor_relationship(
                    "AgentOutputSegment",
                    &close_doc_id,
                    "working_agent",
                    &agent,
                )
                .await?;
                let message = TranscriptMessage {
                    native_request_id: None,
                    message_key: gents::session::sequence_message_key(
                        node_did,
                        session_id,
                        Some(agent_did),
                        sequence,
                    ),
                    session_id: session_id.into(),
                    node_did: node_did.clone(),
                    requester_did: Some(agent_did.clone()),
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
                let mut variables = transcript_message_create_variables(&message)?;
                variables["input"]["session_doc_id"] = serde_json::json!(session_doc);
                variables["input"]["agent_did"] = serde_json::json!(agent_did);
                variables["input"]["creator_did"] = serde_json::json!(agent_did);
                variables["input"]["parent_linked"] = serde_json::json!(true);
                let created = txn
                    .execute_with_variables(CREATE_AGENT_MESSAGE_MUTATION, &variables)
                    .await?;
                let message_doc_id =
                    gents::store::graphql::created_doc_id(&created, "AgentMessage")?;
                txn.add_resource_relationship(
                    "AgentMessage",
                    &message_doc_id,
                    "parent",
                    "sessions",
                    session_doc,
                )
                .await?;
                txn.add_actor_relationship(
                    "AgentMessage",
                    &message_doc_id,
                    "working_agent",
                    &agent,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .expect("seed session history as its Agent");
}
