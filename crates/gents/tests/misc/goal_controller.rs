use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use gents::goal::{
    claim_continuation, claim_retry_continuation, create_goal_for_session,
    delete_goals_for_session, load_canonical_goal, load_goals_for_session, set_goal,
    update_goal_fields, update_goal_fields_if_status, CreateGoalForSessionError,
    CreateGoalForSessionOutcome, GoalStatus,
};
use gents::{
    ActiveRuntimeSnapshot, ConfigAccess, GoalSource, TriggerSource, UpdateSubscriptionSource,
};
use serde::Deserialize;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::support::mock_subscription::MockUpdateSubscriptionSource;
use crate::support::{
    create_request_for_agent_with_signed_fields, create_response_with_content_and_status,
    set_request_lifecycle_state, test_db, TestDb,
};

const SESSION: &str = "goal-session";
const RESCAN: Duration = Duration::from_millis(20);

fn snapshot(local_did: &str) -> Arc<ActiveRuntimeSnapshot> {
    Arc::new(ActiveRuntimeSnapshot {
        generation: 1,
        principal: None,
        local_did: local_did.to_string(),
        default_behavior_id: crate::support::AGENT_NAME.to_string(),
        behaviors: HashMap::new(),
        tool_surfaces: HashMap::new(),
        backend_admission_configs: HashMap::new(),
        unavailable_behaviors: HashMap::new(),
        active_schedules: HashMap::new(),
        unavailable_schedules: HashSet::new(),
        active_event_triggers: HashMap::new(),
        unavailable_event_triggers: HashSet::new(),
        active_tasks: HashMap::new(),
        dispatchers: HashMap::new(),
        behavior_executor_capacities: HashMap::new(),
        behavior_executor_queue_capacities: HashMap::new(),
    })
}

fn source(db: &TestDb) -> (GoalSource, watch::Sender<Arc<ActiveRuntimeSnapshot>>) {
    let (tx, rx) = watch::channel(snapshot(db.node_identity.did()));
    let subscriptions: Arc<dyn UpdateSubscriptionSource> =
        Arc::new(MockUpdateSubscriptionSource::new());
    (
        GoalSource::with_subscription_source(
            subscriptions,
            rx,
            db.node.clone(),
            CancellationToken::new(),
        )
        .with_rescan_interval(RESCAN),
        tx,
    )
}

async fn seed_completed_request(db: &TestDb, request_id: &str) -> String {
    let doc_id = create_request_for_agent_with_signed_fields(
        db.node.as_ref(),
        db.node_identity.did(),
        request_id,
        SESSION,
        "completed",
        "2026-07-15T00:00:00Z",
        None,
        None,
        None,
        None,
    )
    .await;
    create_response_with_content_and_status(
        db.node.as_ref(),
        &format!("response-{request_id}"),
        request_id,
        SESSION,
        "durable progress",
        "complete",
    )
    .await;
    doc_id
}

async fn seed_failed_request(db: &TestDb, request_id: &str) -> String {
    create_request_for_agent_with_signed_fields(
        db.node.as_ref(),
        db.node_identity.did(),
        request_id,
        SESSION,
        "error",
        "2026-07-15T00:00:00Z",
        None,
        None,
        None,
        None,
    )
    .await
}

#[derive(Debug, Deserialize)]
struct ChildRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    session_id: String,
    behavior_id: Option<String>,
    caused_by_parent_request_id: Option<String>,
    caused_by_trigger_id: Option<String>,
    caused_by_trigger_kind: Option<String>,
    metadata: Option<String>,
    lifecycle_state: Option<String>,
    retry_key: Option<String>,
    subagent_depth: Option<i64>,
    workspace_id: Option<String>,
    workspace_authority: Option<String>,
    workspace_owner_deployment_id: Option<String>,
    workspace_seal_hash: Option<String>,
}

async fn goal_children(db: &TestDb) -> Vec<ChildRow> {
    let response = db
        .node
        .execute(
            r#"{
                AgentRequest(filter: { caused_by_trigger_kind: { _eq: "goal" } }) {
                    _docID request_id session_id behavior_id caused_by_parent_request_id
                    caused_by_trigger_id caused_by_trigger_kind metadata lifecycle_state
                    retry_key subagent_depth workspace_id workspace_authority
                    workspace_owner_deployment_id workspace_seal_hash
                }
            }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "query children: {:?}",
        response.errors
    );
    serde_json::from_value(
        response
            .data
            .and_then(|data| data.get("AgentRequest").cloned())
            .unwrap_or_default(),
    )
    .expect("decode goal children")
}

#[tokio::test]
async fn goal_continuation_preserves_nested_workspace_lineage() {
    let db = test_db("goal-nested-workspace-lineage").await;
    let did = db.node_identity.did();
    let admission = gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(did);
    let mut parent = gents_protocol::request_admission::AgentRequestCreate::base(
        "parent-nested-workspace",
        did,
        did,
        crate::support::AGENT_NAME,
        SESSION,
        "nested durable work",
        "interactive",
        "2026-07-15T00:00:00Z",
        admission,
    );
    parent.subagent_depth = 2;
    parent.workspace_id = Some("workspace-goal".to_string());
    parent.workspace_authority = Some("readOnly".to_string());
    parent.workspace_owner_deployment_id = Some("deployment-owner".to_string());
    parent.workspace_seal_hash = Some("seal-hash".to_string());
    gents::sign_agent_request_create_as_registered_target(&mut parent)
        .await
        .expect("sign nested parent");
    let response = db
        .node
        .execute(&parent.graphql_mutation().expect("parent mutation"))
        .await;
    assert!(
        !response.has_errors(),
        "create nested parent: {:?}",
        response.errors
    );
    let lookup = db
        .node
        .execute(
            r#"{ AgentRequest(filter: { request_id: { _eq: "parent-nested-workspace" } }) { _docID } }"#,
        )
        .await;
    assert!(
        !lookup.has_errors(),
        "lookup nested parent: {:?}",
        lookup.errors
    );
    let parent_doc_id = lookup.data.as_ref().unwrap()["AgentRequest"][0]["_docID"]
        .as_str()
        .expect("parent doc id")
        .to_string();
    set_request_lifecycle_state(db.node.as_ref(), &parent_doc_id, "completed").await;
    create_response_with_content_and_status(
        db.node.as_ref(),
        "response-nested-workspace",
        "parent-nested-workspace",
        SESSION,
        "durable progress",
        "complete",
    )
    .await;
    set_goal(
        db.node.as_ref(),
        did,
        SESSION,
        Some("Preserve nested workspace scope"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");

    let (mut source, _tx) = source(&db);
    tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("lineage continuation timed out")
        .expect("lineage continuation intent");
    let children = goal_children(&db).await;
    assert_eq!(children.len(), 1);
    let child = &children[0];
    assert_eq!(child.subagent_depth, Some(2));
    assert_eq!(child.workspace_id.as_deref(), Some("workspace-goal"));
    assert_eq!(child.workspace_authority.as_deref(), Some("readOnly"));
    assert_eq!(
        child.workspace_owner_deployment_id.as_deref(),
        Some("deployment-owner")
    );
    assert_eq!(child.workspace_seal_hash.as_deref(), Some("seal-hash"));
}

#[tokio::test]
async fn delimiter_ambiguous_goal_parent_pairs_get_distinct_retry_keys() {
    let db = test_db("goal-delimiter-safe-key").await;
    let did = db.node_identity.did();
    for (session, parent) in [("b:c", "d"), ("b", "c:d")] {
        let doc_id = create_request_for_agent_with_signed_fields(
            db.node.as_ref(),
            did,
            parent,
            session,
            "completed",
            "2026-07-15T00:00:00Z",
            None,
            None,
            None,
            None,
        )
        .await;
        create_response_with_content_and_status(
            db.node.as_ref(),
            &format!("response-{session}"),
            parent,
            session,
            "progress",
            "complete",
        )
        .await;
        assert!(!doc_id.is_empty());
        set_goal(
            db.node.as_ref(),
            did,
            session,
            Some(&format!("goal for {session}")),
            Some(GoalStatus::Active),
            None,
        )
        .await
        .expect("set delimiter goal");
    }

    let (mut source, _tx) = source(&db);
    for _ in 0..2 {
        tokio::time::timeout(Duration::from_secs(2), source.next_fire())
            .await
            .expect("delimiter continuation timed out")
            .expect("delimiter continuation intent");
    }
    let children = goal_children(&db).await;
    assert_eq!(children.len(), 2);
    let keys = children
        .iter()
        .map(|child| child.retry_key.as_deref().expect("retry key"))
        .collect::<HashSet<_>>();
    assert_eq!(keys.len(), 2);
}

#[tokio::test]
async fn completed_request_materializes_exactly_one_same_session_goal_child() {
    let db = test_db("goal-exactly-once").await;
    seed_completed_request(&db, "parent-complete").await;
    let goal = set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Finish the durable objective"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");

    let (mut source, _snapshot_tx) = source(&db);
    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("goal source timed out")
        .expect("goal continuation intent");
    assert!(intent.pre_materialized_request_id.is_some());

    let children = goal_children(&db).await;
    assert_eq!(children.len(), 1);
    let child = &children[0];
    assert_eq!(child.session_id, SESSION);
    assert_eq!(
        child.behavior_id.as_deref(),
        Some(crate::support::AGENT_NAME)
    );
    assert_eq!(
        child.caused_by_parent_request_id.as_deref(),
        Some("parent-complete")
    );
    assert_eq!(
        child.caused_by_trigger_id.as_deref(),
        Some(goal.goal_id.as_str())
    );
    assert_eq!(child.caused_by_trigger_kind.as_deref(), Some("goal"));

    assert!(
        tokio::time::timeout(Duration::from_millis(150), source.next_fire())
            .await
            .is_err(),
        "a pending child must suppress duplicate continuation"
    );
    assert_eq!(goal_children(&db).await.len(), 1);
}

#[tokio::test]
async fn foreign_request_id_collision_cannot_preempt_owned_goal_continuation() {
    use sha2::{Digest, Sha256};

    let db = test_db("goal-foreign-request-id-collision").await;
    seed_completed_request(&db, "parent-owned-collision").await;
    let goal = set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Ignore foreign request IDs"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");
    let digest =
        Sha256::digest(format!("{}\0{}", goal.goal_id, "parent-owned-collision").as_bytes());
    let collision_id = format!(
        "goal-cont-{:020}-{}",
        1,
        digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    create_request_for_agent_with_signed_fields(
        db.node.as_ref(),
        "did:key:foreign",
        &collision_id,
        "foreign-session",
        "pending",
        "2026-07-15T00:00:01Z",
        None,
        None,
        None,
        None,
    )
    .await;

    let (mut source, _tx) = source(&db);
    let fired = tokio::time::timeout(Duration::from_secs(2), source.next_fire()).await;
    if !matches!(fired, Ok(Some(_))) {
        let state = db
            .node
            .execute("{ Goal { goal_id status last_continued_from_request_id continuation_sequence } AgentRequest { request_id agent_did session_id retry_key caused_by_trigger_kind } }")
            .await;
        let outcome = match fired {
            Err(_) => "timeout",
            Ok(None) => "source ended",
            Ok(Some(_)) => unreachable!(),
        };
        panic!("owned continuation did not fire ({outcome}); state={state:?}");
    }
    let children = goal_children(&db).await;
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].request_id, collision_id);
    assert_eq!(children[0].session_id, SESSION);
}

#[tokio::test]
async fn foreign_retry_key_collision_is_rejected_instead_of_reused() {
    use sha2::{Digest, Sha256};

    let db = test_db("goal-foreign-retry-key-collision").await;
    seed_completed_request(&db, "parent-owned-retry-collision").await;
    let goal = set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Reject foreign retry keys"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");
    let digest =
        Sha256::digest(format!("{}\0{}", goal.goal_id, "parent-owned-retry-collision").as_bytes());
    let retry_key = format!(
        "goal-continuation:{}",
        digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let mutation = format!(
        r#"mutation {{ create_AgentRequest(input: {{
            request_id: "foreign-retry-collision", agent_did: "did:key:foreign",
            behavior_id: "foreign", session_id: "foreign-session",
            retry_root_request: "foreign-retry-collision",
            retry_key: "{}",
            content: "foreign", status: "completed", lifecycle_state: "completed",
            execution_origin: "interactive", created_at: "2026-07-15T00:00:01Z",
            retry_count: 0, max_retries: 3
        }}) {{ _docID }} }}"#,
        retry_key
    );
    let response = db.node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "seed collision: {:?}",
        response.errors
    );

    let (mut source, _tx) = source(&db);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), source.next_fire())
            .await
            .is_err(),
        "a conflicting retry key must not be reported as an owned continuation"
    );
    assert!(goal_children(&db).await.is_empty());
}

#[tokio::test]
async fn restart_reconcile_does_not_duplicate_a_goal_continuation() {
    let db = test_db("goal-restart-no-duplicate").await;
    seed_completed_request(&db, "parent-restart").await;
    set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Continue exactly once across restart"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");

    let (mut first, _first_tx) = source(&db);
    tokio::time::timeout(Duration::from_secs(2), first.next_fire())
        .await
        .expect("first source timed out")
        .expect("first continuation");
    drop(first);

    let (mut restarted, _restart_tx) = source(&db);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), restarted.next_fire())
            .await
            .is_err(),
        "restart must not emit a second child"
    );
    assert_eq!(goal_children(&db).await.len(), 1);
}

#[tokio::test]
async fn restart_materializes_a_claimed_continuation_after_crash() {
    let db = test_db("goal-restart-claimed").await;
    seed_completed_request(&db, "parent-claimed").await;
    let goal = set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Recover the claimed continuation"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");
    assert!(
        claim_continuation(db.node.as_ref(), &goal, "parent-claimed")
            .await
            .expect("claim continuation")
    );
    assert!(goal_children(&db).await.is_empty());

    let (mut restarted, _restart_tx) = source(&db);
    tokio::time::timeout(Duration::from_secs(2), restarted.next_fire())
        .await
        .expect("recovery source timed out")
        .expect("recovered continuation");
    assert_eq!(goal_children(&db).await.len(), 1);
}

#[tokio::test]
async fn restart_materializes_claimed_retry_without_charging_it_twice() {
    let db = test_db("goal-restart-claimed-retry").await;
    seed_failed_request(&db, "parent-claimed-retry").await;
    let goal = set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Recover one charged retry"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");
    assert!(
        claim_retry_continuation(db.node.as_ref(), &goal, "parent-claimed-retry", 1, "failed",)
            .await
            .expect("atomically charge and claim retry")
    );

    let (mut restarted, _tx) = source(&db);
    tokio::time::timeout(Duration::from_secs(2), restarted.next_fire())
        .await
        .expect("retry recovery timed out")
        .expect("retry recovery intent");
    let recovered = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("load recovered goal")
        .expect("goal exists");
    assert_eq!(recovered.infrastructure_retry_count, Some(1));
    assert_eq!(goal_children(&db).await.len(), 1);
}

#[tokio::test]
async fn restart_materializes_claimed_wrapup_retry_without_charging_it_twice() {
    let db = test_db("goal-restart-claimed-wrapup-retry").await;
    seed_failed_request(&db, "parent-claimed-wrapup-retry").await;
    let goal = set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Recover one charged wrap-up retry"),
        Some(GoalStatus::BudgetLimited),
        None,
    )
    .await
    .expect("set budget-limited goal");
    assert!(claim_retry_continuation(
        db.node.as_ref(),
        &goal,
        "parent-claimed-wrapup-retry",
        1,
        "failed",
    )
    .await
    .expect("atomically charge and claim wrap-up retry"));

    let (mut restarted, _tx) = source(&db);
    tokio::time::timeout(Duration::from_secs(2), restarted.next_fire())
        .await
        .expect("wrap-up retry recovery timed out")
        .expect("wrap-up retry recovery intent");
    let recovered = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("load recovered goal")
        .expect("goal exists");
    assert_eq!(recovered.infrastructure_retry_count, Some(1));
    let children = goal_children(&db).await;
    assert_eq!(children.len(), 1);
    let metadata: serde_json::Value =
        serde_json::from_str(children[0].metadata.as_deref().expect("goal metadata"))
            .expect("decode goal metadata");
    assert_eq!(metadata["goal"]["wrapup"], true);
}

#[tokio::test]
async fn continuation_claim_rejects_a_stale_active_status_snapshot() {
    let db = test_db("goal-claim-status-cas").await;
    let stale = set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Do not race an operator pause"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");
    update_goal_fields(db.node.as_ref(), &stale, r#"status: "paused""#)
        .await
        .expect("pause goal");
    assert!(
        !claim_continuation(db.node.as_ref(), &stale, "parent-stale")
            .await
            .expect("stale claim returns cleanly")
    );
}

#[tokio::test]
async fn controller_status_cas_never_overwrites_a_newer_completion() {
    let db = test_db("goal-controller-status-cas").await;
    let stale = set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Completion wins controller races"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");
    update_goal_fields(db.node.as_ref(), &stale, r#"status: "complete""#)
        .await
        .expect("complete goal");
    assert!(!update_goal_fields_if_status(
        db.node.as_ref(),
        &stale,
        GoalStatus::Active,
        r#"status: "paused", last_failure: "stale controller""#,
    )
    .await
    .expect("conditional controller update"));
    let current = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("load goal")
        .expect("goal exists");
    assert_eq!(current.parsed_status(), Some(GoalStatus::Complete));
}

#[tokio::test]
async fn model_goal_create_is_idempotent_and_never_mutates_a_conflict() {
    let db = test_db("goal-model-create").await;
    let created = create_goal_for_session(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        "Ship the feature",
        Some(1_000),
    )
    .await
    .expect("create model goal");
    assert!(matches!(created, CreateGoalForSessionOutcome::Created(_)));

    let duplicate = create_goal_for_session(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        " Ship the feature ",
        Some(1_000),
    )
    .await
    .expect("idempotent model goal retry");
    assert!(matches!(
        duplicate,
        CreateGoalForSessionOutcome::Idempotent(_)
    ));
    assert_eq!(
        load_goals_for_session(db.node.as_ref(), db.node_identity.did(), SESSION)
            .await
            .expect("load model goals")
            .len(),
        1
    );

    let error = create_goal_for_session(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        "Different objective",
        Some(1_000),
    )
    .await
    .expect_err("conflicting create must fail");
    assert!(matches!(error, CreateGoalForSessionError::Conflict));
    let unchanged = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("load unchanged goal")
        .expect("goal remains");
    assert_eq!(unchanged.objective, "Ship the feature");
    assert_eq!(unchanged.token_budget, Some(1_000));
}

#[tokio::test]
async fn clearing_a_goal_also_clears_its_creation_claim() {
    let db = test_db("goal-model-create-clear").await;
    create_goal_for_session(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        "First objective",
        None,
    )
    .await
    .expect("create first goal");
    assert_eq!(
        delete_goals_for_session(db.node.as_ref(), db.node_identity.did(), SESSION)
            .await
            .expect("clear goal and claim"),
        1
    );
    let replacement = create_goal_for_session(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        "Replacement objective",
        None,
    )
    .await
    .expect("create replacement goal");
    assert!(matches!(
        replacement,
        CreateGoalForSessionOutcome::Created(_)
    ));
}

async fn signed_goal_backed_request(
    db: &TestDb,
    request_id: &str,
) -> gents_protocol::request_admission::AgentRequestCreate {
    let did = db.node_identity.did();
    let admission = gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(did);
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        request_id,
        did,
        did,
        crate::support::AGENT_NAME,
        SESSION,
        "Begin durable work",
        "interactive",
        "2026-07-15T00:00:00Z",
        admission,
    );
    create.retry_key = Some(format!("goal-submit:{SESSION}"));
    gents::sign_agent_request_create_as_registered_target(&mut create)
        .await
        .expect("sign goal-backed request");
    create
}

#[tokio::test]
async fn goal_backed_submission_commits_goal_before_request_visibility_and_retries_idempotently() {
    let db = test_db("goal-backed-submit").await;
    let access = ConfigAccess::Local(db.node.clone());
    let create = signed_goal_backed_request(&db, "goal-backed-first").await;
    let first = gents::goal::submit_goal_backed_request(
        &access,
        db.node_identity.did(),
        SESSION,
        "Ship atomically",
        Some(5_000),
        &create,
    )
    .await
    .expect("atomic goal submission");
    assert_eq!(first, gents::goal::GoalBackedRequestDisposition::Created);

    let response = db
        .node
        .execute(r#"{ Goal { goal_id } AgentRequest(filter: { retry_key: { _eq: "goal-submit:goal-session" } }) { request_id lifecycle_state } }"#)
        .await;
    assert!(
        !response.has_errors(),
        "load atomic pair: {:?}",
        response.errors
    );
    assert_eq!(
        response.data.as_ref().unwrap()["Goal"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let requests = response.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["lifecycle_state"], "pending");

    let retry = gents::goal::submit_goal_backed_request(
        &access,
        db.node_identity.did(),
        SESSION,
        "Ship atomically",
        Some(5_000),
        &create,
    )
    .await
    .expect("idempotent atomic retry");
    assert_eq!(retry, gents::goal::GoalBackedRequestDisposition::Idempotent);

    let committed_goal = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("load committed goal")
        .expect("goal exists");
    update_goal_fields(db.node.as_ref(), &committed_goal, r#"status: "complete""#)
        .await
        .expect("simulate progress after an ambiguous commit acknowledgement");
    let late_retry = gents::goal::submit_goal_backed_request(
        &access,
        db.node_identity.did(),
        SESSION,
        "Ship atomically",
        Some(5_000),
        &create,
    )
    .await
    .expect("exact retry remains idempotent after goal progress");
    assert_eq!(
        late_retry,
        gents::goal::GoalBackedRequestDisposition::Idempotent
    );
}

#[tokio::test]
async fn goal_backed_retry_rejects_changed_logical_request_fields() {
    let db = test_db("goal-backed-submit-fingerprint-conflict").await;
    let access = ConfigAccess::Local(db.node.clone());
    let create = signed_goal_backed_request(&db, "goal-backed-fingerprint").await;
    gents::goal::submit_goal_backed_request(
        &access,
        db.node_identity.did(),
        SESSION,
        "Ship one exact request",
        None,
        &create,
    )
    .await
    .expect("initial goal submission");

    let mut changed = create.clone();
    changed.metadata = Some(r#"{"changed":true}"#.to_string());
    gents::sign_agent_request_create_as_registered_target(&mut changed)
        .await
        .expect("re-sign changed request");
    let error = gents::goal::submit_goal_backed_request(
        &access,
        db.node_identity.did(),
        SESSION,
        "Ship one exact request",
        None,
        &changed,
    )
    .await
    .expect_err("changed retry semantics must conflict");
    assert!(error
        .to_string()
        .contains("different immutable request fields"));
}

#[tokio::test]
async fn goal_backed_submission_rejects_a_matching_terminal_goal_without_publishing() {
    let db = test_db("goal-backed-submit-terminal-goal").await;
    let access = ConfigAccess::Local(db.node.clone());
    set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Already terminal"),
        Some(GoalStatus::Complete),
        None,
    )
    .await
    .expect("set terminal goal");
    let create = signed_goal_backed_request(&db, "goal-backed-terminal").await;
    let error = gents::goal::submit_goal_backed_request(
        &access,
        db.node_identity.did(),
        SESSION,
        "Already terminal",
        None,
        &create,
    )
    .await
    .expect_err("terminal goal must reject a new first request");
    assert!(error.to_string().contains("non-active goal"));
    let response = db
        .node
        .execute("{ AgentRequest { request_id } GoalCreationClaim { creation_key } }")
        .await;
    assert!(
        !response.has_errors(),
        "query terminal rollback: {:?}",
        response.errors
    );
    assert!(response.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(response.data.as_ref().unwrap()["GoalCreationClaim"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn goal_backed_submission_discards_goal_when_request_staging_fails() {
    let db = test_db("goal-backed-submit-rollback").await;
    let access = ConfigAccess::Local(db.node.clone());
    let mut invalid = signed_goal_backed_request(&db, "goal-backed-invalid").await;
    invalid.initial_status = "processing".to_string();
    let error = gents::goal::submit_goal_backed_request(
        &access,
        db.node_identity.did(),
        SESSION,
        "Never publish partially",
        None,
        &invalid,
    )
    .await
    .expect_err("invalid request must abort transaction");
    assert!(error.to_string().contains("pending state"));
    assert!(
        load_goals_for_session(db.node.as_ref(), db.node_identity.did(), SESSION)
            .await
            .expect("load rolled-back goals")
            .is_empty()
    );
    let response = db
        .node
        .execute("{ GoalCreationClaim { creation_key } AgentRequest { request_id } }")
        .await;
    assert!(
        !response.has_errors(),
        "load rollback state: {:?}",
        response.errors
    );
    assert!(response.data.as_ref().unwrap()["GoalCreationClaim"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(response.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn concurrent_model_goal_create_converges_to_one_physical_goal() {
    let db = test_db("goal-model-create-concurrent").await;
    let left_node = db.node.clone();
    let right_node = db.node.clone();
    let did = db.node_identity.did().to_string();
    let left_did = did.clone();
    let left = tokio::spawn(async move {
        create_goal_for_session(
            left_node.as_ref(),
            &left_did,
            SESSION,
            "Concurrent objective",
            None,
        )
        .await
    });
    let right = tokio::spawn(async move {
        create_goal_for_session(
            right_node.as_ref(),
            &did,
            SESSION,
            "Concurrent objective",
            None,
        )
        .await
    });
    left.await.expect("left task").expect("left create");
    right.await.expect("right task").expect("right create");
    assert_eq!(
        load_goals_for_session(db.node.as_ref(), db.node_identity.did(), SESSION)
            .await
            .expect("load converged goals")
            .len(),
        1
    );
}

#[tokio::test]
async fn any_newer_active_request_blocks_goal_continuation_for_the_whole_session() {
    let db = test_db("goal-session-idle").await;
    seed_completed_request(&db, "older-complete").await;
    create_request_for_agent_with_signed_fields(
        db.node.as_ref(),
        db.node_identity.did(),
        "newer-manual",
        SESSION,
        "pending",
        "2026-07-15T00:01:00Z",
        None,
        None,
        None,
        None,
    )
    .await;
    set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Wait for session idleness"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");

    let (mut source, _snapshot_tx) = source(&db);
    assert!(
        tokio::time::timeout(Duration::from_millis(150), source.next_fire())
            .await
            .is_err()
    );
    assert!(goal_children(&db).await.is_empty());
}

#[tokio::test]
async fn interrupted_terminal_pauses_instead_of_self_continuing() {
    let db = test_db("goal-interrupted").await;
    create_request_for_agent_with_signed_fields(
        db.node.as_ref(),
        db.node_identity.did(),
        "parent-interrupted",
        SESSION,
        "interrupted",
        "2026-07-15T00:00:00Z",
        None,
        None,
        None,
        None,
    )
    .await;
    set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Respect human interruption"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");

    let (mut source, _snapshot_tx) = source(&db);
    assert!(
        tokio::time::timeout(Duration::from_millis(150), source.next_fire())
            .await
            .is_err()
    );
    let goal = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("load goal")
        .expect("goal exists");
    assert_eq!(goal.parsed_status(), Some(GoalStatus::Paused));
    assert!(goal_children(&db).await.is_empty());
}

#[tokio::test]
async fn token_budget_materializes_one_wrapup_and_never_repeats_it() {
    let db = test_db("goal-budget-wrapup").await;
    seed_completed_request(&db, "parent-budget").await;
    let usage = r#"mutation {
        add_InferenceCall(input: {
            call_id: "goal-budget-call",
            runtime_instance_id: "goal-test",
            request_id: "parent-budget",
            call_seq: 1,
            backend_id: "backend-test",
            behavior_id: "test",
            agent_did: "did:test:test",
            call_kind: "inference",
            attempt: 1,
            call_state: "completed",
            queued_at: "2026-07-15T00:00:00Z",
            started_at: "2026-07-15T00:00:00Z",
            ended_at: "2026-07-15T00:00:01Z",
            priority: 0,
            queue_depth_at_enqueue: 0,
            controller_generation: 1,
            backend_config_fingerprint: "goal-test",
            prompt_tokens: 100,
            completion_tokens: 5,
            cached_input_tokens: 90
        }) { _docID }
    }"#;
    let response = db.node.execute(usage).await;
    assert!(!response.has_errors(), "seed usage: {:?}", response.errors);
    set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Stop at the durable budget"),
        Some(GoalStatus::Active),
        Some(Some(10)),
    )
    .await
    .expect("set goal");

    let (mut source, _snapshot_tx) = source(&db);
    tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("goal source timed out")
        .expect("wrapup intent");
    let children = goal_children(&db).await;
    assert_eq!(children.len(), 1);
    let metadata: serde_json::Value =
        serde_json::from_str(children[0].metadata.as_deref().expect("goal metadata"))
            .expect("valid goal metadata");
    assert_eq!(
        metadata.pointer("/goal/wrapup"),
        Some(&serde_json::json!(true))
    );

    set_request_lifecycle_state(db.node.as_ref(), &children[0].doc_id, "completed").await;
    create_response_with_content_and_status(
        db.node.as_ref(),
        "budget-wrapup-response",
        &children[0].request_id,
        SESSION,
        "final durable wrapup",
        "complete",
    )
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), source.next_fire())
            .await
            .is_err()
    );
    let goal = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("load goal")
        .expect("goal exists");
    assert_eq!(goal.parsed_status(), Some(GoalStatus::BudgetLimited));
    assert_eq!(goal.tokens_used, Some(15));
    assert_eq!(goal.wrapup_requested, Some(true));
    assert_eq!(goal.wrapup_completed, Some(true));
    assert_eq!(goal_children(&db).await.len(), 1);
}

#[tokio::test]
async fn resume_resets_blocked_audit_identity_and_count() {
    let db = test_db("goal-resume-audit-reset").await;
    let goal = set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Resume with a fresh blocked audit"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");
    update_goal_fields(
        db.node.as_ref(),
        &goal,
        r#"status: "blocked", consecutive_blocked_audits: 3, last_blocked_request_id: "request-3", last_blocked_reason: "needs approval", active_started_at: null"#,
    )
    .await
    .expect("seed blocked audit");

    let resumed = set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        None,
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("resume goal");
    assert_eq!(resumed.parsed_status(), Some(GoalStatus::Active));
    assert_eq!(resumed.consecutive_blocked_audits, Some(0));
    assert_eq!(resumed.last_blocked_request_id, None);
    assert_eq!(resumed.last_blocked_reason, None);
}

#[tokio::test]
async fn provider_usage_limit_moves_active_goal_to_usage_limited() {
    let db = test_db("goal-provider-usage-limit").await;
    create_request_for_agent_with_signed_fields(
        db.node.as_ref(),
        db.node_identity.did(),
        "usage-limited-request",
        SESSION,
        "error",
        "2026-07-15T00:00:00Z",
        None,
        None,
        None,
        None,
    )
    .await;
    let response = db
        .node
        .execute(
            r#"mutation {
                add_InferenceCall(input: {
                    call_id: "usage-limited-call",
                    request_id: "usage-limited-request",
                    call_seq: 1,
                    attempt: 1,
                    call_state: "failed",
                    failure_reason: "provider insufficient_quota: credit balance exhausted"
                }) { _docID }
            }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "seed usage limit: {:?}",
        response.errors
    );
    set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Stop on provider quota exhaustion"),
        Some(GoalStatus::Active),
        None,
    )
    .await
    .expect("set goal");

    let (mut source, _snapshot_tx) = source(&db);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), source.next_fire())
            .await
            .is_err()
    );
    let goal = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("load goal")
        .expect("goal exists");
    assert_eq!(goal.parsed_status(), Some(GoalStatus::UsageLimited));
    assert!(goal
        .last_failure
        .as_deref()
        .is_some_and(|reason| reason.contains("insufficient_quota")));
}

#[tokio::test]
async fn failed_wrapup_retries_twice_then_is_durably_abandoned() {
    let db = test_db("goal-wrapup-retry-bound").await;
    seed_completed_request(&db, "parent-wrapup-retry").await;
    set_goal(
        db.node.as_ref(),
        db.node_identity.did(),
        SESSION,
        Some("Bound failed wrap-up retries"),
        Some(GoalStatus::Active),
        Some(Some(1)),
    )
    .await
    .expect("set goal");
    let response = db
        .node
        .execute(
            r#"mutation {
                add_InferenceCall(input: {
                    call_id: "wrapup-budget-call",
                    request_id: "parent-wrapup-retry",
                    call_seq: 1,
                    attempt: 1,
                    call_state: "completed",
                    prompt_tokens: 2,
                    completion_tokens: 0,
                    cached_input_tokens: 0
                }) { _docID }
            }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "seed wrapup usage: {:?}",
        response.errors
    );

    let (mut source, _snapshot_tx) = source(&db);
    for expected_children in 1..=3 {
        tokio::time::timeout(Duration::from_secs(2), source.next_fire())
            .await
            .unwrap_or_else(|_| panic!("goal source timed out before child {expected_children}"))
            .expect("wrapup or retry intent");
        let children = goal_children(&db).await;
        assert_eq!(children.len(), expected_children);
        let child = children
            .iter()
            .find(|child| child.lifecycle_state.as_deref() == Some("pending"))
            .expect("new pending wrap-up child");
        set_request_lifecycle_state(db.node.as_ref(), &child.doc_id, "failed").await;
    }

    assert!(
        tokio::time::timeout(Duration::from_millis(250), source.next_fire())
            .await
            .is_err(),
        "bounded failed wrap-up must not spawn a fourth child"
    );
    let goal = load_canonical_goal(db.node.as_ref(), db.node_identity.did(), SESSION)
        .await
        .expect("load goal")
        .expect("goal exists");
    assert_eq!(goal.parsed_status(), Some(GoalStatus::BudgetLimited));
    assert_eq!(goal.infrastructure_retry_count, Some(2));
    assert_eq!(goal.wrapup_completed, Some(true));
    assert!(goal
        .last_failure
        .as_deref()
        .is_some_and(|reason| reason.contains("after 2 retries")));
    assert_eq!(goal_children(&db).await.len(), 3);
}
