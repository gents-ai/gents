use gents_automation::goal::seed_goal_from_access;
use gents_protocol::goal_status::GoalStatus;
use gents_protocol::request_input::QueueDelivery;
use gents_protocol::row::AgentRequestRow;
use gents_store::graphql::escape_graphql_string;
use gents_store::test_support::rows_when;

use crate::support::agent_requests::submit_user_request;
use crate::support::agent_requests::RequestActors;
use crate::support::builder_runtime::{
    builder_actors, fixture_agent, runtime_builder, spawn_runtime,
};
use crate::support::interrupt::BootedAgent;
use crate::support::live_inference::wait_for_request_terminal;
use crate::support::trial_store;
use gents_inference::test_support::scripted_backend::{
    ScriptedBackend, StreamChunk, StreamPlan, StreamResponse,
};
use gents_store::test_support::TestStore;

const MODEL: &str = "goal-stop-model";
const BACKEND: &str = "goal-stop-backend";

fn blocked_report(marker: &str, id: &str, reason: &str) -> StreamResponse {
    StreamResponse::streams(
        marker,
        vec![StreamChunk::tool_call(
            id,
            "update_goal",
            serde_json::json!({"status": "blocked", "reason": reason}).to_string(),
        )],
    )
}

struct Booted {
    backend: ScriptedBackend,
    actors: RequestActors,
    runtime: BootedAgent,
    session: String,
    _db: TestStore,
}

async fn boot(test: &str, plan: StreamPlan) -> Booted {
    let backend = ScriptedBackend::start_with_plans(MODEL, vec![plan]).unwrap();
    let db = trial_store(test).await;
    let actors = builder_actors(&db, BACKEND, backend.endpoint(), MODEL, 1).await;
    let agent = fixture_agent(runtime_builder(&db, &actors), &actors, actors.agent_did())
        .backend_id(BACKEND)
        .model_name(MODEL)
        .stream_batch_ms(0)
        .deadline_duration_secs(60)
        .done()
        .build()
        .await
        .unwrap();
    let runtime = spawn_runtime(&db, agent).await;
    let session = format!("{test}-session");
    submit_user_request(
        actors.node_did(),
        &actors.fixture,
        &session,
        &format!("{test}-opening"),
        "Hello.",
        QueueDelivery::Queue,
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(&actors.fixture.access, &format!("{test}-opening")).await,
        "completed"
    );
    Booted {
        backend,
        actors,
        runtime,
        session,
        _db: db,
    }
}

/// The Goal row and its continuation requests, once `settled` holds of them.
async fn goal_when<F>(
    actors: &RequestActors,
    session: &str,
    settled: F,
) -> (serde_json::Value, Vec<AgentRequestRow>)
where
    F: Fn(&serde_json::Value, &[AgentRequestRow]) -> bool,
{
    let session = escape_graphql_string(session);
    let query = format!(
        r#"{{
            Goal(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ status blocked_reason wrapup_requested wrapup_completed }}
            AgentRequest(filter: {{ session_id: {{ _eq: "{session}" }}, caused_by_trigger_kind: {{ _eq: "goal" }} }}) {{ {} }}
        }}"#,
        gents::SIGNED_REQUEST_FIELDS,
    );
    let decode = |data: &serde_json::Value| {
        serde_json::from_value::<Vec<AgentRequestRow>>(data["AgentRequest"].clone()).unwrap()
    };
    let data = rows_when(&actors.fixture.access, "Goal", &query, |data| {
        data["Goal"]
            .as_array()
            .and_then(|goals| goals.first())
            .is_some_and(|goal| settled(goal, &decode(data)))
    })
    .await;
    let requests = decode(&data);
    (data["Goal"][0].clone(), requests)
}

fn all_terminal(requests: &[AgentRequestRow]) -> bool {
    requests.iter().all(|request| {
        request
            .lifecycle_state
            .is_some_and(|state| state.is_terminal())
    })
}

/// The failure that motivated one-report blocking: a model that rewords its
/// blocked reason every turn. Its first report pauses the Goal with no further
/// continuation; the user's next message resumes it, and the reworded second
/// report pauses it again at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reworded_blocked_report_pauses_the_goal_and_the_users_message_resumes_it() {
    const MARKER: &str = "reconcile-dock-manifest-goal";
    let plan = StreamPlan::current_authored_user(
        MARKER,
        vec![
            blocked_report(MARKER, "block-1", "I need the dock manifest."),
            StreamResponse::completes(MARKER, ["I need the dock manifest from you."]),
            blocked_report(MARKER, "block-2", "Still waiting on a manifest from you."),
            StreamResponse::completes(MARKER, ["Please send the manifest."]),
        ],
    );
    let Booted {
        backend,
        actors,
        runtime,
        session,
        _db,
    } = boot("goal-reworded-block", plan).await;
    seed_goal_from_access(
        &actors.agent_access,
        actors.node_did(),
        &session,
        Some(&format!("{MARKER}: reconcile the manifest")),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();

    let (goal, continuations) = goal_when(&actors, &session, |goal, requests| {
        goal["status"] == "blocked" && !requests.is_empty() && all_terminal(requests)
    })
    .await;
    assert_eq!(goal["blocked_reason"], "I need the dock manifest.");
    assert_eq!(continuations.len(), 1, "one report stops continuation");
    assert_eq!(backend.observed_requests(MARKER), 2);

    submit_user_request(
        actors.node_did(),
        &actors.fixture,
        &session,
        "goal-reworded-block-answer",
        "The manifest is on its way.",
        QueueDelivery::Queue,
    )
    .await;
    let (goal, continuations) = goal_when(&actors, &session, |goal, requests| {
        goal["status"] == "blocked"
            && goal["blocked_reason"] == "Still waiting on a manifest from you."
            && all_terminal(requests)
    })
    .await;
    assert_eq!(goal["status"], "blocked");
    assert_eq!(
        continuations.len(),
        2,
        "the message resumed the Goal for one more turn"
    );
    assert_eq!(backend.observed_requests(MARKER), 4);
    runtime.shutdown().await;
}

/// A Goal whose budget is reached gets exactly one wrap-up turn and then
/// pauses at its budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_goal_that_reaches_its_budget_gets_one_wrapup_turn_then_pauses() {
    const MARKER: &str = "tally-pallet-goal";
    let plan = StreamPlan::current_authored_user(
        MARKER,
        vec![
            StreamResponse::completes(MARKER, ["Counted the first pallet."]),
            StreamResponse::completes(MARKER, ["Done: one pallet. Next: the rest."]),
        ],
    );
    let Booted {
        backend,
        actors,
        runtime,
        session,
        _db,
    } = boot("goal-budget-wrapup", plan).await;
    seed_goal_from_access(
        &actors.agent_access,
        actors.node_did(),
        &session,
        Some(&format!("{MARKER}: tally every pallet")),
        Some(GoalStatus::Active),
        Some(Some(1)),
        None,
    )
    .await
    .unwrap();

    let (goal, continuations) = goal_when(&actors, &session, |goal, requests| {
        goal["status"] == "budget_limited"
            && goal["wrapup_completed"] == true
            && all_terminal(requests)
    })
    .await;
    assert_eq!(goal["wrapup_requested"], true);
    let wrapups = continuations
        .iter()
        .filter(|request| {
            request
                .input
                .as_ref()
                .and_then(|input| input.goal_continuation.as_ref())
                .is_some_and(|facts| facts.wrapup)
        })
        .count();
    assert_eq!(continuations.len(), 2, "one turn over budget, one wrap-up");
    assert_eq!(wrapups, 1);
    assert_eq!(backend.observed_requests(MARKER), 2);
    runtime.shutdown().await;
}

fn repeated_call(marker: &str, id: &str) -> StreamResponse {
    StreamResponse::streams(
        marker,
        vec![StreamChunk::tool_call(
            id,
            "list_processes",
            "{}".to_owned(),
        )],
    )
}

/// One goal turn that makes the same `list_processes` call, then ends.
fn repeated_turn(marker: &str, turn: usize) -> [StreamResponse; 2] {
    [
        repeated_call(marker, &format!("repeat-{turn}")),
        StreamResponse::completes(marker, ["Checked the processes again."]),
    ]
}

fn last_failure_when_paused(goal: &serde_json::Value) -> bool {
    goal["status"] == "paused"
}

async fn goal_failure(actors: &RequestActors, session: &str) -> serde_json::Value {
    let session = escape_graphql_string(session);
    let data = actors
        .fixture
        .access
        .execute(&format!(
            r#"{{ Goal(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ status last_failure blocked_reason }} }}"#
        ))
        .await
        .unwrap();
    data["data"]["Goal"][0].clone()
}

/// A model that never reports blocked and repeats the same tool call with the
/// same result is paused by the harness after `NO_PROGRESS_LIMIT` turns
/// without progress (Lean `GoalProgress.step`), whatever it says.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_goal_that_repeats_the_same_call_is_paused_for_no_progress() {
    const MARKER: &str = "count-bins-goal";
    let limit = usize::try_from(gents_automation::goal::progress::NO_PROGRESS_LIMIT).unwrap();
    let plan = StreamPlan::current_authored_user(
        MARKER,
        (0..limit + 2)
            .flat_map(|turn| repeated_turn(MARKER, turn))
            .collect(),
    );
    let Booted {
        actors,
        runtime,
        session,
        _db,
        ..
    } = boot("goal-no-progress", plan).await;
    seed_goal_from_access(
        &actors.agent_access,
        actors.node_did(),
        &session,
        Some(&format!("{MARKER}: count every bin")),
        Some(GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();

    let (_, continuations) = goal_when(&actors, &session, |goal, requests| {
        last_failure_when_paused(goal) && all_terminal(requests)
    })
    .await;
    let goal = goal_failure(&actors, &session).await;
    assert_eq!(goal["last_failure"], "no_progress");
    assert!(
        (limit..=limit + 1).contains(&continuations.len()),
        "paused after {} continuations, limit {limit}",
        continuations.len()
    );
    runtime.shutdown().await;
}

/// The user resumes the paused Goal from the session's opening request.
async fn resume(actors: &RequestActors, session: &str) {
    let opening = session.replace("-session", "-opening");
    gents_automation::goal::resume_goal_request(
        &actors.fixture.access,
        &actors.fixture.user,
        actors.node_did(),
        session,
        &opening,
    )
    .await
    .unwrap();
}

async fn set_policy(
    access: &gents_store::config_client::ConfigAccess,
    actors: &RequestActors,
    session: &str,
    on_blocked: gents_protocol::goal::OnBlocked,
) -> Result<
    gents_protocol::goal::GoalStopPolicy,
    gents_protocol::operations::goal::GoalOperationError,
> {
    gents_automation::operations::goal::set_stop_policy(
        access,
        actors.node_did(),
        session,
        &gents_protocol::operations::goal::GoalStopPolicyInput {
            on_blocked: Some(on_blocked),
            ..Default::default()
        },
    )
    .await
}

/// `confirm_once`: the first blocked report gets one confirmation turn and
/// the Goal stays active; the report in that turn pauses it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirm_once_pauses_on_the_second_blocked_report() {
    const MARKER: &str = "label-crates-goal";
    let plan = StreamPlan::current_authored_user(
        MARKER,
        vec![
            blocked_report(MARKER, "block-1", "I need the label printer."),
            StreamResponse::completes(MARKER, ["Checking once more."]),
            blocked_report(MARKER, "block-2", "The printer is still offline."),
            StreamResponse::completes(MARKER, ["I need the printer."]),
        ],
    );
    let Booted {
        actors,
        runtime,
        session,
        _db,
        ..
    } = boot("goal-confirm-once", plan).await;
    seed_goal_from_access(
        &actors.agent_access,
        actors.node_did(),
        &session,
        Some(&format!("{MARKER}: label every crate")),
        Some(GoalStatus::Paused),
        None,
        None,
    )
    .await
    .unwrap();
    set_policy(
        &actors.fixture.access,
        &actors,
        &session,
        gents_protocol::goal::OnBlocked::ConfirmOnce,
    )
    .await
    .unwrap();
    resume(&actors, &session).await;

    let (goal, continuations) = goal_when(&actors, &session, |goal, requests| {
        goal["status"] == "blocked" && all_terminal(requests)
    })
    .await;
    assert_eq!(goal["blocked_reason"], "The printer is still offline.");
    assert_eq!(
        continuations.len(),
        2,
        "one confirmation turn, then the pause"
    );
    runtime.shutdown().await;
}

/// `keep_trying`: blocked reports are kept for the user and the Goal goes
/// on; repeating them makes no progress, so the harness still pauses it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keep_trying_continues_past_blocked_reports_until_no_progress() {
    const MARKER: &str = "sort-mail-goal";
    let limit = usize::try_from(gents_automation::goal::progress::NO_PROGRESS_LIMIT).unwrap();
    let plan = StreamPlan::current_authored_user(
        MARKER,
        (0..limit + 2)
            .flat_map(|turn| {
                [
                    blocked_report(MARKER, &format!("block-{turn}"), "I need the mail key."),
                    StreamResponse::completes(MARKER, ["Trying again."]),
                ]
            })
            .collect(),
    );
    let Booted {
        actors,
        runtime,
        session,
        _db,
        ..
    } = boot("goal-keep-trying", plan).await;
    seed_goal_from_access(
        &actors.agent_access,
        actors.node_did(),
        &session,
        Some(&format!("{MARKER}: sort the mail")),
        Some(GoalStatus::Paused),
        None,
        None,
    )
    .await
    .unwrap();
    set_policy(
        &actors.fixture.access,
        &actors,
        &session,
        gents_protocol::goal::OnBlocked::KeepTrying,
    )
    .await
    .unwrap();
    resume(&actors, &session).await;

    let (_, continuations) = goal_when(&actors, &session, |goal, requests| {
        last_failure_when_paused(goal) && all_terminal(requests)
    })
    .await;
    let goal = goal_failure(&actors, &session).await;
    assert_eq!(goal["last_failure"], "no_progress");
    assert_eq!(goal["blocked_reason"], "I need the mail key.");
    assert!(
        continuations.len() > 1,
        "blocked reports did not stop the goal"
    );
    runtime.shutdown().await;
}

/// Who may create and then change a Goal's stop policy, replayed in the order
/// the Lean access rule (`goal_policies`) decides it: the working Agent can
/// neither create its Goal's policy nor change one its user set.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_working_agent_cannot_loosen_its_goal_policy() {
    const MARKER: &str = "audit-ledger-goal";
    let plan = StreamPlan::current_authored_user(MARKER, vec![]);
    let Booted {
        actors,
        runtime,
        session,
        _db,
        ..
    } = boot("goal-policy-access", plan).await;
    seed_goal_from_access(
        &actors.agent_access,
        actors.node_did(),
        &session,
        Some(&format!("{MARKER}: audit the ledger")),
        Some(GoalStatus::Paused),
        None,
        None,
    )
    .await
    .unwrap();
    for case in gents_lean_vocab::families::contracts_goal_policy_access::cases() {
        let access = match case.input.actor.as_str() {
            "working_agent" => &actors.agent_access,
            "user" => &actors.fixture.access,
            other => panic!("unknown modeled actor {other}"),
        };
        let allowed = set_policy(
            access,
            &actors,
            &session,
            gents_protocol::goal::OnBlocked::ConfirmOnce,
        )
        .await
        .is_ok();
        case.check(allowed);
    }
    runtime.shutdown().await;
}
