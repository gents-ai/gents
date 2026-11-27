use super::*;

use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_session::request::lifecycle::queue::QueueSource;
use gents_session::{request::watcher::DefraWatcher, request::watcher::Watcher};
use gents_store::identity::NodeIdentity;

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
    gents_store::config_client::ConfigAccess::local_as(node.clone(), root)
        .transact_write("test.convergence.request", |txn| {
            let (input, working_agent) = (&input, &working_agent);
            Box::pin(async move {
                let response = txn
                    .execute_with_variables(
                        "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                        &json!({"input": input}),
                    )
                    .await?;
                let doc = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
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
        AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose, RequestScope,
    };
    AgentRequestCreate::base(
        RequestPurpose::Normal,
        request_id,
        RequestScope::new(node_did, agent_did, agent_did, session_id),
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
    signer: &dyn gents_store::identity::NodeIdentity,
    working_agent: Option<&str>,
) -> String {
    gents_session::request::admission::sign_agent_request_create(signer, &mut create)
        .await
        .expect("sign replicated request");
    let mutation = create
        .graphql_mutation()
        .expect("render replicated request");
    let root = ::identity::Did::new(node.node_identity_did().expect("runtime DID")).unwrap();
    let working_agent = working_agent.map(|did| ::identity::Did::new(did).expect("Agent DID"));
    gents_store::config_client::ConfigAccess::local_as(node.clone(), root)
        .transact_write("test.convergence.signed_request", |txn| {
            let (mutation, working_agent) = (&mutation, &working_agent);
            Box::pin(async move {
                let response = txn.execute(mutation).await?;
                let doc = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
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
    let mut response = gents_store::config_client::ConfigAccess::local_as(node.clone(), root)
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
fn foreign_did(db: &gents_store::test_support::TestStore) -> String {
    gents_store::identity::NodeIdentity::did(
        &gents_store::identity::KeyIdentity::load_or_create(
            db.path().join("foreign-owner.key"),
            None,
        )
        .unwrap(),
    )
    .to_owned()
}

async fn create_owned_pending_request(
    node: &std::sync::Arc<EmbeddedNode>,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    node_did: &str,
) -> String {
    let input = json!({
        "request_id": request_id,
        "purpose": "normal",
        "node_did": node_did,
        "requester_did": null,
        "agent_did": agent_did,
        "session_id": session_id,
        "retry_root_request": request_id,
        "content": "hello",
        "lifecycle_state": "pending",
        "execution_origin": "interactive",
        "created_at": CONVERGENCE_CREATED_AT,
        "retry_count": 0,
        "request_hop": 0,
    });
    root_write_request(node, input, Some(agent_did)).await
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

pub(super) async fn single_claimer_watcher_never_claims_foreign_replica() {
    let db = trial_store("convergence-single-claimer").await;
    let agent = support::managed_agent_fixture(&db).await.agent;
    let owner_did = db.node_identity.did();
    let foreign = foreign_did(&db);

    let foreign_doc = create_owned_pending_request(
        &db.node,
        agent.did(),
        "convergence-foreign-req",
        "convergence-foreign-session",
        &foreign,
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

    let own_doc = create_owned_pending_request(
        &db.node,
        agent.did(),
        "convergence-own-req",
        "convergence-own-session",
        owner_did,
    )
    .await;
    let claimed = watcher.try_fetch_request(&own_doc).await.unwrap();
    let claimed = claimed.expect("own pending request must be claimable (guards a vacuous filter)");
    assert_eq!(
        claimed.node_did, owner_did,
        "the claimable request must be the owner's own"
    );
}

pub(super) async fn reconcile_coalesce_never_supersedes_foreign_replica() {
    let db = trial_store("convergence-coalesce-foreign").await;
    let agent = support::managed_agent_fixture(&db).await.agent;
    let agent_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        db.node.clone(),
        ::identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let owner_did = db.node_identity.did();
    let foreign = foreign_did(&db);
    let session_id = "convergence-coalesce-foreign-session";
    let key = format!("background_completion:{session_id}");
    let foreign_identity = gents_store::identity::KeyIdentity::load_or_create(
        db.path().join("foreign-owner.key"),
        None,
    )
    .unwrap();
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
            &foreign_identity as &dyn gents_store::identity::NodeIdentity,
            foreign_parent_id,
            foreign_parent_doc.as_str(),
            "convergence-coalesce-foreign",
            "2026-03-23T00:00:02Z",
        ),
    ] {
        use gents_protocol::request_admission::{
            AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose, RequestScope,
        };
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            request_id,
            RequestScope::new(node_did, identity.did(), identity.did(), session_id),
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

    let survivor = gents_session::request::lifecycle::queue::reconcile_coalesced_pending_request_for_with_access(
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
