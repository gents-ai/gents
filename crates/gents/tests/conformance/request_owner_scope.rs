use super::*;

use gents::__test_internals::QueueSource;
use gents::{DefraWatcher, NodeIdentity, Watcher};
use gents_protocol::request_lifecycle::RequestLifecycleState;

const CONVERGENCE_CREATED_AT: &str = "2026-03-23T00:00:00Z";

/// A replicated row as the runtime root persists it: the root authors the row
/// with any node scope and relates the working Agent, which then executes it.
async fn root_write_request(
    node: &std::sync::Arc<EmbeddedNode>,
    input: Value,
    working_agent: Option<&str>,
) -> String {
    let root = ::identity::Did::new(node.node_identity_did().expect("runtime DID")).unwrap();
    let working_agent = working_agent.map(|did| ::identity::Did::new(did).expect("Agent DID"));
    gents::ConfigAccess::local_as(node.clone(), root)
        .transact_write("test.convergence.request", |txn| {
            let (input, working_agent) = (&input, &working_agent);
            Box::pin(async move {
                let response = txn
                    .execute_with_variables(
                        "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                        &json!({"input": input}),
                    )
                    .await?;
                let doc = gents::store::graphql::created_doc_id(&response, "AgentRequest")?;
                if let Some(agent) = working_agent {
                    txn.add_actor_relationship("AgentRequest", &doc, "working_agent", agent)
                        .await?;
                }
                Ok::<_, anyhow::Error>(doc)
            })
        })
        .await
        .expect("root writes replicated request")
}

fn local_self_request(
    request_id: &str,
    node_did: &str,
    session_id: &str,
    agent_did: &str,
) -> gents_protocol::request_admission::AgentRequestCreate {
    use gents_protocol::request_admission::{
        AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
    };
    AgentRequestCreate::base(
        RequestPurpose::Normal,
        request_id,
        node_did,
        agent_did,
        agent_did,
        session_id,
        "work",
        "interactive",
        CONVERGENCE_CREATED_AT,
        AgentRequestAdmissionRecord::local_self(agent_did),
    )
}

/// Persists a signed request create as the runtime root would a replicated row.
async fn root_write_signed(
    node: &std::sync::Arc<EmbeddedNode>,
    mut create: gents_protocol::request_admission::AgentRequestCreate,
    signer: &dyn gents::NodeIdentity,
    working_agent: Option<&str>,
) -> String {
    gents::sign_agent_request_create(signer, &mut create)
        .await
        .expect("sign replicated request");
    let mutation = create
        .graphql_mutation()
        .expect("render replicated request");
    let root = ::identity::Did::new(node.node_identity_did().expect("runtime DID")).unwrap();
    let working_agent = working_agent.map(|did| ::identity::Did::new(did).expect("Agent DID"));
    gents::ConfigAccess::local_as(node.clone(), root)
        .transact_write("test.convergence.signed_request", |txn| {
            let (mutation, working_agent) = (&mutation, &working_agent);
            Box::pin(async move {
                let response = txn.execute(mutation).await?;
                let doc = gents::store::graphql::created_doc_id(&response, "AgentRequest")?;
                if let Some(agent) = working_agent {
                    txn.add_actor_relationship("AgentRequest", &doc, "working_agent", agent)
                        .await?;
                }
                Ok::<_, anyhow::Error>(doc)
            })
        })
        .await
        .expect("root writes signed replicated request")
}

/// The runtime root authored the fixture rows and reads them as their owner.
async fn root_read(node: &std::sync::Arc<EmbeddedNode>, query: &str) -> Value {
    let root = ::identity::Did::new(node.node_identity_did().expect("runtime DID")).unwrap();
    let mut response = gents::ConfigAccess::local_as(node.clone(), root)
        .execute(query)
        .await
        .expect("root reads fixture rows");
    response["data"].take()
}

fn first_data_row<T>(data: &Value, key: &str) -> T
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(data[key][0].clone())
        .unwrap_or_else(|error| panic!("decode {key}: {error}; data={data}"))
}

/// A syntactically valid DID for a node this process does not run.
fn foreign_did(db: &support::TestDb) -> String {
    gents::NodeIdentity::did(
        &gents::KeyIdentity::load_or_create(db.data_path().join("foreign-owner.key"), None)
            .unwrap(),
    )
    .to_owned()
}

#[derive(Debug, Deserialize)]
struct ConvergenceRow {
    lifecycle_state: RequestLifecycleState,
    failure_reason: Option<String>,
    terminalized_at: Option<String>,
    terminal_redrive_attempts: Option<i64>,
}

async fn create_owned_request(
    node: &std::sync::Arc<EmbeddedNode>,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    node_did: &str,
    lifecycle_state: &str,
) -> String {
    create_owned_request_with_times(
        node,
        agent_did,
        request_id,
        session_id,
        node_did,
        lifecycle_state,
        CONVERGENCE_CREATED_AT,
        CONVERGENCE_CREATED_AT,
    )
    .await
}

async fn seed_owned_request_projection(
    node: &std::sync::Arc<EmbeddedNode>,
    session_id: &str,
    request_id: &str,
    node_did: &str,
    agent_did: &str,
) {
    let rows = root_read(
        node,
        &format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 2) {{ _docID lifecycle_state created_at }} }}"#,
            escape_graphql_string(request_id)
        ),
    )
    .await;
    let row = &rows["AgentRequest"][0];
    let mut session =
        support::session_document_in_scope(node_did, session_id, agent_did, CONVERGENCE_CREATED_AT);
    session.observation = Some(gents_protocol::session::SessionObservation {
        reply_preview: None,
        last_activity_at: row["created_at"]
            .as_str()
            .expect("creation time")
            .to_owned(),
        preview: Some("hello".into()),
        latest_request: Some(gents_protocol::session::SessionRequestObservation {
            request_doc_id: row["_docID"].as_str().expect("physical request").to_owned(),
            request_id: request_id.to_owned(),
            lifecycle_state: serde_json::from_value(row["lifecycle_state"].clone())
                .expect("canonical lifecycle"),
        }),
    });
    let root = ::identity::Did::new(node.node_identity_did().expect("runtime DID")).unwrap();
    let agent = ::identity::Did::new(agent_did).expect("Agent DID");
    let session = serde_json::to_value(&session).expect("serialize session");
    gents::ConfigAccess::local_as(node.clone(), root)
        .transact_write("test.convergence.session", |txn| {
            let (session, agent) = (&session, &agent);
            Box::pin(async move {
                let response = txn
                    .execute_with_variables(
                        "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }",
                        &json!({"input": session}),
                    )
                    .await?;
                let doc = gents::store::graphql::created_doc_id(&response, "AgentSession")?;
                txn.add_actor_relationship("AgentSession", &doc, "working_agent", agent)
                    .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .expect("root writes request-owned session projection");
}

#[allow(clippy::too_many_arguments)]
async fn create_owned_request_with_times(
    node: &std::sync::Arc<EmbeddedNode>,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    node_did: &str,
    lifecycle_state: &str,
    created_at: &str,
    terminalized_at: &str,
) -> String {
    create_owned_request_with_times_and_requester(
        node,
        agent_did,
        request_id,
        session_id,
        node_did,
        lifecycle_state,
        created_at,
        terminalized_at,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn create_owned_request_with_times_and_requester(
    node: &std::sync::Arc<EmbeddedNode>,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    node_did: &str,
    lifecycle_state: &str,
    created_at: &str,
    terminalized_at: &str,
    requester_did: Option<&str>,
) -> String {
    create_recovery_request(
        node,
        request_id,
        session_id,
        node_did,
        agent_did,
        lifecycle_state,
        created_at,
        terminalized_at,
        requester_did,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn create_recovery_request(
    node: &std::sync::Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    node_did: &str,
    agent_did: &str,
    lifecycle_state: &str,
    created_at: &str,
    terminalized_at: &str,
    requester_did: Option<&str>,
) -> String {
    let mut input = json!({
        "request_id": request_id,
        "purpose": "normal",
        "node_did": node_did,
        "requester_did": requester_did,
        "agent_did": agent_did,
        "session_id": session_id,
        "retry_root_request": request_id,
        "content": "hello",
        "lifecycle_state": lifecycle_state,
        "execution_origin": "interactive",
        "created_at": created_at,
        "retry_count": 0,
        "max_retries": gents::request::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
        "request_hop": 0,
    });
    if RequestLifecycleState::is_terminal_str(Some(lifecycle_state)) {
        input["terminalized_at"] = json!(terminalized_at);
        input["terminal_redrive_attempts"] = json!(0);
    }
    if matches!(lifecycle_state, "claimed" | "processing") {
        input["execution_generation"] = json!(uuid::Uuid::new_v4().to_string());
        input["execution_lease_expires_at"] = json!(CONVERGENCE_CREATED_AT);
    }
    root_write_request(node, input, Some(agent_did)).await
}

async fn create_recovery_request_basic(
    node: &std::sync::Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    node_did: &str,
    agent_did: &str,
    state: &str,
) -> String {
    create_recovery_request(
        node,
        request_id,
        session_id,
        node_did,
        agent_did,
        state,
        CONVERGENCE_CREATED_AT,
        CONVERGENCE_CREATED_AT,
        None,
    )
    .await
}

#[derive(Debug, Deserialize)]
struct QueueConvergenceRow {
    lifecycle_state: RequestLifecycleState,
    node_did: String,
    superseded_by_request: Option<String>,
}

fn coalesce_wakeup_input(session_id: &str) -> String {
    format!(
        r#"{{"queue":{{"source":"background_completion","policy":"coalesce","key":"background_completion:{session_id}","queued_after_request_id":null}}}}"#
    )
}

#[allow(clippy::too_many_arguments)]
async fn create_queue_request(
    node: &std::sync::Arc<EmbeddedNode>,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    node_did: &str,
    execution_origin: &str,
    input: &str,
    created_at: &str,
) -> String {
    let queue_input = serde_json::from_str::<Value>(input).expect("request input JSON");
    root_write_request(
        node,
        json!({
            "request_id": request_id,
            "purpose": "normal",
            "node_did": node_did,
            "agent_did": agent_did,
            "session_id": session_id,
            "retry_root_request": request_id,
            "content": "wake up",
            "input": queue_input,
            "lifecycle_state": "pending",
            "execution_origin": execution_origin,
            "created_at": created_at,
            "retry_count": 0,
            "max_retries": gents::request::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
            "request_hop": 0,
        }),
        Some(agent_did),
    )
    .await
}

async fn fetch_queue_convergence_row(
    node: &std::sync::Arc<EmbeddedNode>,
    request_id: &str,
) -> QueueConvergenceRow {
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}, limit: 1) {{
                lifecycle_state
                node_did
                superseded_by_request
            }}
        }}"#
    );
    first_data_row(&root_read(node, &query).await, "AgentRequest")
}

async fn fetch_convergence_row(
    node: &std::sync::Arc<EmbeddedNode>,
    request_id: &str,
) -> ConvergenceRow {
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}, limit: 1) {{
                lifecycle_state
                failure_reason
                terminalized_at
                terminal_redrive_attempts
            }}
        }}"#
    );
    first_data_row(&root_read(node, &query).await, "AgentRequest")
}

async fn composite_commit_count(node: &std::sync::Arc<EmbeddedNode>, doc_id: &str) -> usize {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"query {{
            _commits(
                docID: ["{escaped_doc_id}"],
                filter: {{ fieldName: {{ _eq: "_C" }} }}
            ) {{ cid }}
        }}"#
    );
    root_read(node, &query)
        .await
        .get("_commits")
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len)
}

pub(super) async fn single_claimer_watcher_never_claims_foreign_replica() {
    let db = test_db("convergence-single-claimer").await;
    let agent = support::managed_agent(&db).await;
    let owner_did = db.node_identity.did();
    let foreign = foreign_did(&db);

    let foreign_doc = create_owned_request(
        &db.node,
        agent.did(),
        "convergence-foreign-req",
        "convergence-foreign-session",
        &foreign,
        "pending",
    )
    .await;

    let mut watcher = DefraWatcher::for_agents(db.node.clone(), owner_did, vec![agent.clone()])
        .expect("request watcher");

    assert!(
        watcher
            .try_fetch_request(&foreign_doc)
            .await
            .unwrap()
            .is_none(),
        "foreign replica must not be claimable via try_fetch_request"
    );

    let scanned = tokio::time::timeout(Duration::from_millis(750), watcher.next_request()).await;
    assert!(
        scanned.is_err(),
        "watcher scan (next_request) must never yield a foreign replica, got {scanned:?}"
    );

    let own_doc = create_owned_request(
        &db.node,
        agent.did(),
        "convergence-own-req",
        "convergence-own-session",
        owner_did,
        "pending",
    )
    .await;
    let claimed = watcher.try_fetch_request(&own_doc).await.unwrap();
    let claimed = claimed.expect("own pending request must be claimable (guards a vacuous filter)");
    assert_eq!(
        claimed.node_did, owner_did,
        "the claimable request must be the owner's own"
    );
}

pub(super) async fn canonical_recovery_repairs_expired_request_after_terminal_write_gap() {
    let db = test_db("convergence-terminal-repair").await;
    let agent = support::managed_agent(&db).await;
    let owner_did = db.node_identity.did();
    let request_id = "convergence-terminal-repair-request";
    let session_id = "convergence-terminal-repair-session";
    let request_doc_id = create_recovery_request_basic(
        &db.node,
        request_id,
        session_id,
        owner_did,
        agent.did(),
        "processing",
    )
    .await;
    seed_owned_request_projection(&db.node, session_id, request_id, owner_did, agent.did()).await;
    let request_commits_before_repair = composite_commit_count(&db.node, &request_doc_id).await;
    let repaired = RequestLifecycle::repair_terminal_requests_with_identities(
        &db.node,
        owner_did,
        std::slice::from_ref(&agent),
    )
    .await
    .unwrap();
    assert_eq!(repaired.repaired, 1);
    assert_eq!(repaired.failed, 0);
    assert_eq!(
        composite_commit_count(&db.node, &request_doc_id).await,
        request_commits_before_repair + 1,
        "one logical terminal repair must add one composite request commit"
    );
    let row = fetch_convergence_row(&db.node, request_id).await;
    assert_eq!(row.lifecycle_state, RequestLifecycleState::Failed);
    assert_eq!(
        row.failure_reason.as_deref(),
        Some("execution lease expired")
    );
    assert!(row.terminalized_at.is_some());
    assert_eq!(row.terminal_redrive_attempts, Some(0));

    let duplicate = RequestLifecycle::repair_terminal_requests_with_identities(
        &db.node,
        owner_did,
        std::slice::from_ref(&agent),
    )
    .await
    .unwrap();
    assert_eq!(duplicate.repaired, 0, "duplicate observation is idempotent");

    let provider_request_id = "convergence-no-interrupt-latch";
    let provider_session_id = "convergence-no-interrupt-latch-session";
    create_recovery_request_basic(
        &db.node,
        provider_request_id,
        provider_session_id,
        owner_did,
        agent.did(),
        "processing",
    )
    .await;
    seed_owned_request_projection(
        &db.node,
        provider_session_id,
        provider_request_id,
        owner_did,
        agent.did(),
    )
    .await;
    let provider_repair = RequestLifecycle::repair_terminal_requests_with_identities(
        &db.node,
        owner_did,
        std::slice::from_ref(&agent),
    )
    .await
    .unwrap();
    assert_eq!(provider_repair.repaired, 1);
    let provider_row = fetch_convergence_row(&db.node, provider_request_id).await;
    assert_eq!(provider_row.lifecycle_state, RequestLifecycleState::Failed);
    assert_eq!(
        provider_row.failure_reason.as_deref(),
        Some("execution lease expired"),
        "without the request-owned interrupt latch, recovery remains Failed"
    );

    let runtime_interrupt_request_id = "convergence-runtime-interrupt-stamp";
    let runtime_interrupt_session_id = "convergence-runtime-interrupt-session";
    let runtime_interrupt_doc_id = create_recovery_request_basic(
        &db.node,
        runtime_interrupt_request_id,
        runtime_interrupt_session_id,
        owner_did,
        agent.did(),
        "processing",
    )
    .await;
    seed_owned_request_projection(
        &db.node,
        runtime_interrupt_session_id,
        runtime_interrupt_request_id,
        owner_did,
        agent.did(),
    )
    .await;
    let runtime_interrupt_doc_id = escape_graphql_string(&runtime_interrupt_doc_id);
    gents::ConfigAccess::local_managed_agent(
        db.node.clone(),
        ::identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap()
    .write(
        "test.convergence.interrupt_latch",
        &format!(
            r#"mutation {{
                update_AgentRequest(
                    docID: "{runtime_interrupt_doc_id}",
                    input: {{ interrupt_requested_at: "2026-07-10T00:00:00Z" }}
                ) {{ _docID }}
            }}"#
        ),
    )
    .await
    .expect("seed request-owned interrupt latch as working Agent");

    let runtime_interrupt_repair = RequestLifecycle::repair_terminal_requests_with_identities(
        &db.node,
        owner_did,
        std::slice::from_ref(&agent),
    )
    .await
    .unwrap();
    assert_eq!(runtime_interrupt_repair.repaired, 1);
    let runtime_interrupt_row = fetch_convergence_row(&db.node, runtime_interrupt_request_id).await;
    assert_eq!(
        runtime_interrupt_row.lifecycle_state,
        RequestLifecycleState::Interrupted
    );
    assert_eq!(
        runtime_interrupt_row.failure_reason.as_deref(),
        Some("execution lease expired"),
        "the durable interrupt latch selects Interrupted lifecycle, while the canonical recovery diagnostic records the expired ownership boundary that triggered repair"
    );
}

pub(super) async fn recover_stuck_requests_recovers_claimed_lifecycle_state() {
    let db = test_db("convergence-recover-claimed").await;
    let agent = support::managed_agent(&db).await;
    let owner_did = db.node_identity.did();

    create_recovery_request_basic(
        &db.node,
        "convergence-stuck-claimed",
        "convergence-stuck-claimed-session",
        owner_did,
        agent.did(),
        "claimed",
    )
    .await;
    seed_owned_request_projection(
        &db.node,
        "convergence-stuck-claimed-session",
        "convergence-stuck-claimed",
        owner_did,
        agent.did(),
    )
    .await;
    let report = RequestLifecycle::recover_all_with_identities(
        &db.node,
        owner_did,
        std::slice::from_ref(&agent),
    )
    .await
    .unwrap();
    assert_eq!(
        report.requests_recovered, 1,
        "a claimed own-request must be recovered (Lean requestRecoveryStale = claimed ∨ processing)"
    );

    let row = fetch_convergence_row(&db.node, "convergence-stuck-claimed").await;
    assert!(
        matches!(
            row.lifecycle_state,
            RequestLifecycleState::Failed | RequestLifecycleState::Completed
        ),
        "recovered request must be terminal, got {}",
        row.lifecycle_state
    );
}

pub(super) async fn reconcile_coalesce_never_supersedes_foreign_replica() {
    let db = test_db("convergence-coalesce-foreign").await;
    let agent = support::managed_agent(&db).await;
    let agent_access = gents::ConfigAccess::local_managed_agent(
        db.node.clone(),
        ::identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let owner_did = db.node_identity.did();
    let foreign = foreign_did(&db);
    let session_id = "convergence-coalesce-foreign-session";
    let key = format!("background_completion:{session_id}");
    let foreign_identity =
        gents::KeyIdentity::load_or_create(db.data_path().join("foreign-owner.key"), None).unwrap();
    let owner_parent_id = "convergence-coalesce-owner-parent";
    let owner_parent_doc = root_write_signed(
        &db.node,
        local_self_request(owner_parent_id, owner_did, session_id, agent.did()),
        agent.as_ref(),
        Some(agent.did()),
    )
    .await;
    let foreign_parent_id = "convergence-coalesce-foreign-parent";
    let foreign_parent_doc = root_write_signed(
        &db.node,
        local_self_request(
            foreign_parent_id,
            &foreign,
            session_id,
            foreign_identity.did(),
        ),
        &foreign_identity,
        None,
    )
    .await;

    for (node_did, identity, parent_id, parent_doc, request_id, created_at) in [
        (
            owner_did,
            agent.as_ref(),
            owner_parent_id,
            owner_parent_doc.as_str(),
            "convergence-coalesce-owner-survivor",
            "2026-03-23T00:00:00Z",
        ),
        (
            owner_did,
            agent.as_ref(),
            owner_parent_id,
            owner_parent_doc.as_str(),
            "convergence-coalesce-owner-duplicate",
            "2026-03-23T00:00:01Z",
        ),
        (
            foreign.as_str(),
            &foreign_identity as &dyn gents::NodeIdentity,
            foreign_parent_id,
            foreign_parent_doc.as_str(),
            "convergence-coalesce-foreign",
            "2026-03-23T00:00:02Z",
        ),
    ] {
        use gents_protocol::request_admission::{
            AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
        };
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            request_id,
            node_did,
            identity.did(),
            identity.did(),
            session_id,
            "wake up",
            "scheduled",
            created_at,
            AgentRequestAdmissionRecord::runtime_local_control(identity.did(), parent_id),
        );
        create.input = serde_json::from_str(&coalesce_wakeup_input(session_id)).unwrap();
        create.input.queue.as_mut().unwrap().queued_after_request_id = Some(parent_id.into());
        create.caused_by_parent_request_id = Some(parent_id.into());
        create.caused_by_parent_request_doc_id = Some(parent_doc.into());
        root_write_signed(
            &db.node,
            create,
            identity,
            (node_did == owner_did).then_some(agent.did()),
        )
        .await;
    }

    let survivor = gents::__test_internals::reconcile_coalesced_pending_request_for_with_access(
        (&db.node, &agent_access),
        agent.did(),
        session_id,
        owner_did,
        QueueSource::BackgroundCompletion,
        &key,
        None,
    )
    .await
    .unwrap()
    .expect("owner survivor");
    assert_eq!(
        survivor.request_id, "convergence-coalesce-owner-survivor",
        "the earliest owner row is the coalesce survivor"
    );

    let owner_survivor =
        fetch_queue_convergence_row(&db.node, "convergence-coalesce-owner-survivor").await;
    assert_eq!(
        owner_survivor.lifecycle_state,
        RequestLifecycleState::Pending
    );

    let owner_duplicate =
        fetch_queue_convergence_row(&db.node, "convergence-coalesce-owner-duplicate").await;
    assert_eq!(
        owner_duplicate.lifecycle_state,
        RequestLifecycleState::Superseded
    );
    assert_eq!(
        owner_duplicate.superseded_by_request.as_deref(),
        Some("convergence-coalesce-owner-survivor"),
    );

    let foreign_row = fetch_queue_convergence_row(&db.node, "convergence-coalesce-foreign").await;
    assert_eq!(
        foreign_row.node_did, foreign,
        "foreign replica ownership unchanged"
    );
    assert_eq!(
        foreign_row.lifecycle_state,
        RequestLifecycleState::Pending,
        "foreign replica must not be superseded by the owner's coalesce reconcile"
    );
    assert_eq!(
        foreign_row.superseded_by_request.as_deref().unwrap_or(""),
        "",
        "foreign replica must carry no supersede pointer"
    );
}

pub(super) async fn drain_wakeups_never_interrupts_foreign_replica() {
    let db = test_db("convergence-drain-foreign").await;
    let agent = support::managed_agent(&db).await;
    let owner_did = db.node_identity.did();
    let foreign = foreign_did(&db);
    let session_id = "convergence-drain-foreign-session";
    let input = coalesce_wakeup_input(session_id);
    let target_doc_id = create_owned_request(
        &db.node,
        agent.did(),
        "convergence-drain-active",
        session_id,
        owner_did,
        "processing",
    )
    .await;

    create_queue_request(
        &db.node,
        agent.did(),
        "convergence-drain-owner",
        session_id,
        owner_did,
        "scheduled",
        &input,
        "2026-03-23T00:00:00Z",
    )
    .await;
    create_queue_request(
        &db.node,
        agent.did(),
        "convergence-drain-foreign",
        session_id,
        &foreign,
        "scheduled",
        &input,
        "2026-03-23T00:00:01Z",
    )
    .await;

    let signer = agent.managed_agent_signer().expect("managed Agent signer");
    gents::interrupt_request_by_doc_id(&db.node, Some(&signer), &target_doc_id, owner_did, None)
        .await
        .unwrap();
    let latched = root_read(
        &db.node,
        &format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ interrupt_requested_at }} }}"#,
            escape_graphql_string(&target_doc_id)
        ),
    )
    .await;
    assert!(
        latched["AgentRequest"][0]["interrupt_requested_at"].is_string(),
        "the exact owned request must be latched"
    );

    let owner = fetch_queue_convergence_row(&db.node, "convergence-drain-owner").await;
    assert_eq!(owner.lifecycle_state, RequestLifecycleState::Interrupted);

    let foreign_row = fetch_queue_convergence_row(&db.node, "convergence-drain-foreign").await;
    assert_eq!(foreign_row.node_did, foreign);
    assert_eq!(
        foreign_row.lifecycle_state,
        RequestLifecycleState::Pending,
        "foreign replica must not be interrupted by the owner's wake-up drain"
    );
}
