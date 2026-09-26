use gents::{AgentIdentity, DocumentRuntimeOptions, Gents};

use super::fixtures::bind_behavior_backend;
use super::interrupt::{wait_for_runtime_ready, BootedAgent};
use super::streaming_backend::{MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse};

pub struct AcceptedTurnSpec<'a> {
    pub backend_id: &'a str,
    pub model: &'a str,
    pub parent_behavior_id: &'a str,
    pub configured_behavior_ids: &'a [&'a str],
    pub request_id: &'a str,
    pub session_id: &'a str,
    pub prompt: &'a str,
    /// One accepted provider turn. Multiple calls in this vector are published
    /// atomically in their provider order, never as separate fixture turns.
    pub accepted_chunks: Vec<StreamChunk>,
    /// Additional exact backend plans, normally paused or completing children.
    pub child_plans: Vec<StreamPlan>,
    pub valid_until: Option<&'a str>,
    pub subagent_depth: Option<u32>,
    /// Optional caller-owned admission fields (for example a workspace seal).
    /// This runs before the canonical request is signed.
    pub request_setup:
        Option<Box<dyn FnOnce(&mut gents_protocol::request_admission::AgentRequestCreate) + 'a>>,
}

pub struct PreparedAcceptedTurn {
    pub backend: MockStreamingBackend,
}

pub struct AcceptedTurnRuntime {
    pub runtime: BootedAgent,
    pub backend: MockStreamingBackend,
}

/// Enqueue another canonically signed local-self request for an existing
/// session while its public runtime remains active. This creates only the
/// request admission document; provider acceptance remains runtime-owned.
pub async fn enqueue_local_accepted_request(
    db: &super::TestDb,
    behavior_id: &str,
    request_id: &str,
    session_id: &str,
    prompt: &str,
) {
    enqueue_local_accepted_request_until(db, behavior_id, request_id, session_id, prompt, None)
        .await;
}

/// Enqueue a canonically signed local-self request with an optional admission
/// deadline. The runtime remains the sole owner of accepting and executing it.
pub async fn enqueue_local_accepted_request_until(
    db: &super::TestDb,
    behavior_id: &str,
    request_id: &str,
    session_id: &str,
    prompt: &str,
    valid_until: Option<&str>,
) {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut request = gents_protocol::request_admission::AgentRequestCreate::base(
        request_id,
        db.node_identity.did(),
        db.node_identity.did(),
        behavior_id,
        session_id,
        prompt,
        "interactive",
        now,
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
            db.node_identity.did(),
        ),
    );
    request.valid_until = valid_until.map(str::to_owned);
    gents::sign_agent_request_create(db.node_identity.as_ref(), &mut request)
        .await
        .expect("sign follow-up accepted-turn request");
    let response = db.node.execute(&request.graphql_mutation().unwrap()).await;
    assert!(
        !response.has_errors(),
        "create follow-up accepted-turn request failed: {:?}",
        response.errors
    );
}

impl AcceptedTurnRuntime {
    pub async fn shutdown(self) {
        self.runtime.shutdown().await;
    }

    pub async fn crash(self) {
        self.runtime.crash().await;
    }
}

/// Drive a real provider acceptance through the public runtime. This helper
/// owns no transition policy: callers supply provider chunks/plans and observe
/// durable rows through the production owners.
pub async fn prepare_accepted_turn(
    db: &super::TestDb,
    spec: AcceptedTurnSpec<'_>,
) -> PreparedAcceptedTurn {
    prepare_accepted_turn_as(db, db.node_identity.as_ref(), spec).await
}

/// Prepare an accepted turn for a separately authenticated principal sharing
/// the fixture node. All durable scope and signatures derive from `identity`.
pub async fn prepare_accepted_turn_as(
    db: &super::TestDb,
    identity: &dyn AgentIdentity,
    mut spec: AcceptedTurnSpec<'_>,
) -> PreparedAcceptedTurn {
    let mut plans = vec![StreamPlan::current_authored_user(
        spec.prompt,
        vec![
            StreamResponse::streams(spec.prompt, spec.accepted_chunks),
            StreamResponse::completes(spec.prompt, ["parent complete"]),
        ],
    )];
    plans.extend(
        spec.child_plans
            .into_iter()
            .map(StreamPlan::with_current_authored_user),
    );
    let backend = MockStreamingBackend::start_with_plans(spec.model, plans)
        .expect("start accepted-turn backend");
    for behavior_id in spec.configured_behavior_ids {
        bind_behavior_backend(
            db.node.as_ref(),
            identity.did(),
            behavior_id,
            spec.backend_id,
            backend.endpoint(),
            spec.model,
        )
        .await;
    }

    let existing_session =
        super::snapshots::fetch_session_snapshot(db.node.as_ref(), spec.session_id).await;
    if existing_session.is_none() {
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut session = super::session_document(spec.session_id, spec.parent_behavior_id, &now);
        session.agent_did = identity.did().to_string();
        session.requester_did = Some(identity.did().to_string());
        session.title = Some(gents_protocol::session::SessionTitle {
            text: "generated-title".into(),
            source: gents_protocol::session::SessionTitleSource::Generated,
        });
        super::create_session_document(db.node.as_ref(), &session).await;
    } else if existing_session.is_some_and(|session| session.title.is_none()) {
        let session_id = gents::graphql::escape_graphql_string(spec.session_id);
        let title = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
            "text": "generated-title",
            "source": "generated"
        }))
        .expect("render accepted-turn fixture title");
        let response = db
            .node
            .execute(&format!(
                r#"mutation {{ update_AgentSession(filter: {{ session_id: {{ _eq: "{session_id}" }} }}, input: {{ title: {title} }}) {{ _docID }} }}"#
            ))
            .await;
        assert!(
            !response.has_errors(),
            "prepopulate accepted-turn session title failed: {:?}",
            response.errors
        );
    }

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut request = gents_protocol::request_admission::AgentRequestCreate::base(
        spec.request_id,
        identity.did(),
        identity.did(),
        spec.parent_behavior_id,
        spec.session_id,
        spec.prompt,
        "interactive",
        now,
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    request.valid_until = spec.valid_until.map(str::to_owned);
    request.subagent_depth = spec.subagent_depth.unwrap_or(0);
    if let Some(setup) = spec.request_setup.take() {
        setup(&mut request);
    }
    gents::sign_agent_request_create(identity, &mut request)
        .await
        .expect("sign accepted-turn request");
    let response = db.node.execute(&request.graphql_mutation().unwrap()).await;
    assert!(
        !response.has_errors(),
        "create accepted-turn request failed: {:?}",
        response.errors
    );

    PreparedAcceptedTurn { backend }
}

/// Start a caller-composed public runtime after [`prepare_accepted_turn`] has
/// persisted and signed the request. This is the seam for custom tool factories
/// and gates configured through `Gents::builder()`.
pub async fn boot_prepared_accepted_turn(
    db: &super::TestDb,
    prepared: PreparedAcceptedTurn,
    agent: Gents,
) -> AcceptedTurnRuntime {
    let agent_did = agent.agent_did().to_string();
    let progress = agent.shutdown_progress();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &agent_did).await;
    AcceptedTurnRuntime {
        runtime: BootedAgent::new(shutdown_tx, handle, agent_did)
            .with_shutdown_evidence("accepted-turn agent", progress),
        backend: prepared.backend,
    }
}

/// Convenience path for the document-configured production runtime.
pub async fn boot_accepted_turn(
    db: &super::TestDb,
    spec: AcceptedTurnSpec<'_>,
    runtime_options: DocumentRuntimeOptions,
) -> AcceptedTurnRuntime {
    let prepared = prepare_accepted_turn(db, spec).await;
    let identity: std::sync::Arc<dyn AgentIdentity> = db.node_identity.clone();
    let agent = Gents::from_default_behavior_documents(db.node.clone(), identity, runtime_options)
        .await
        .expect("build accepted-turn runtime");
    boot_prepared_accepted_turn(db, prepared, agent).await
}

/// Boot a real accepted turn with an explicitly configured backend worker
/// capacity. Multi-child fixtures use this only when their premise requires
/// simultaneous child execution before the parent releases its worker slot.
pub async fn boot_accepted_turn_with_backend_capacity(
    db: &super::TestDb,
    spec: AcceptedTurnSpec<'_>,
    runtime_options: DocumentRuntimeOptions,
    max_concurrent: usize,
) -> AcceptedTurnRuntime {
    boot_accepted_turn_with_backend_capacity_inner(db, spec, runtime_options, max_concurrent, None)
        .await
}

/// Keep later provider turns caller-controlled while a previously accepted
/// background tool is already executing. The dynamic marker is armed before
/// runtime boot, so the parent cannot consume its static completion response.
pub async fn boot_accepted_turn_with_backend_capacity_and_dynamic_followups(
    db: &super::TestDb,
    spec: AcceptedTurnSpec<'_>,
    runtime_options: DocumentRuntimeOptions,
    max_concurrent: usize,
    marker: &str,
) -> AcceptedTurnRuntime {
    boot_accepted_turn_with_backend_capacity_inner(
        db,
        spec,
        runtime_options,
        max_concurrent,
        Some(marker),
    )
    .await
}

async fn boot_accepted_turn_with_backend_capacity_inner(
    db: &super::TestDb,
    spec: AcceptedTurnSpec<'_>,
    runtime_options: DocumentRuntimeOptions,
    max_concurrent: usize,
    dynamic_marker: Option<&str>,
) -> AcceptedTurnRuntime {
    assert!(max_concurrent > 0);
    let backend_id = spec.backend_id.to_owned();
    let prepared = prepare_accepted_turn(db, spec).await;
    if let Some(marker) = dynamic_marker {
        prepared.backend.enable_dynamic_followups(marker);
    }
    use gents::config_client::{
        apply_desired_state_plan, read_desired_state_record_in_txn, DesiredStateApplyDocument,
        DesiredStateApplyPlan,
    };
    let did = db.node_identity.did();
    gents::ConfigAccess::transact_local(
        db.node.as_ref(),
        None,
        "test.accepted_turn_backend_capacity",
        |txn| {
            let backend_id = backend_id.clone();
            Box::pin(async move {
                let (_, mut backend) = read_desired_state_record_in_txn(
                    txn,
                    gents::Collection::InferenceBackend,
                    did,
                    &backend_id,
                )
                .await?
                .ok_or_else(|| anyhow::anyhow!("accepted-turn backend {backend_id} missing"))?;
                backend["max_concurrent"] = max_concurrent.into();
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: gents::Collection::InferenceBackend,
                    add: backend.clone(),
                    update: backend,
                }])?;
                apply_desired_state_plan(txn, &plan).await.map(|_| ())
            })
        },
    )
    .await
    .expect("configure accepted-turn backend worker capacity");
    let identity: std::sync::Arc<dyn AgentIdentity> = db.node_identity.clone();
    let agent = Gents::from_default_behavior_documents(db.node.clone(), identity, runtime_options)
        .await
        .expect("build accepted-turn runtime");
    boot_prepared_accepted_turn(db, prepared, agent).await
}
