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
