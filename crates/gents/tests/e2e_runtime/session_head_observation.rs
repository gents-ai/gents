use gents_lean_vocab::tla::replay::{Red, Trace};
use gents_observe::client_projection::query::{load_node_snapshot_on, read_node_snapshot_root};
use gents_protocol::output::TerminalOutput;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_session::request::lifecycle::{
    ClaimOutcome, RequestLifecycle, RequestTerminalOutcome, TerminalizeResult,
};
use gents_session::test_support::{AgentHome, ClaimedRequest, Stage};
use gents_store::test_support::UNREACHED_DEADLINE;

async fn pending(home: &AgentHome, id: &str, second: u64) -> ClaimedRequest {
    ClaimedRequest::client_signed_reserved(home, &home.binding)
        .ids(id, "head-observation")
        .created_at(&format!("2026-09-01T12:00:{second:02}Z"))
        .lease(UNREACHED_DEADLINE)
        .drive(Stage::Admitted)
        .await
}

async fn replay(trace: Trace) {
    let home = AgentHome::start("session-head-trace").await;
    let higher = trace.steps[0].before["aHigher"].as_bool().unwrap();
    let (a_id, b_id) = if higher {
        ("z-request", "a-request")
    } else {
        ("a-request", "z-request")
    };
    let mut a = pending(&home, a_id, 0).await;
    let mut b = None::<ClaimedRequest>;
    let reader = std::cell::OnceCell::new();
    let mut root = None;
    let mut projected = None;
    let mut expected = None;
    let mut a_completed = false;
    let mut boundary_names_b = false;
    for step in &trace.steps {
        match step.action.as_str() {
            "ClaimA" => assert_eq!(
                a.lifecycle.claim_with_identity().await.unwrap(),
                ClaimOutcome::Claimed
            ),
            "AdmitB" => {
                let mut next =
                    pending(&home, b_id, step.after["second"]["B"].as_u64().unwrap()).await;
                if step.after["state"]["B"] == "refused" {
                    next.lifecycle
                        .reject_admission("request refused")
                        .await
                        .unwrap();
                    expected = Some(RequestLifecycleState::Failed);
                    boundary_names_b = a_completed;
                }
                b = Some(next);
            }
            "ClaimB" => {
                let request = b.as_mut().unwrap();
                assert_eq!(
                    request.lifecycle.claim_with_identity().await.unwrap(),
                    ClaimOutcome::Claimed,
                    "{}",
                    trace.config
                );
                boundary_names_b = true;
            }
            "Complete" => {
                let request = match step.actor.as_ref().and_then(serde_json::Value::as_str) {
                    Some("A") => {
                        a_completed = true;
                        &mut a
                    }
                    Some("B") => {
                        expected = Some(RequestLifecycleState::Completed);
                        b.as_mut().unwrap()
                    }
                    actor => panic!("unknown session request {actor:?}"),
                };
                RequestLifecycle::begin_owned_execution_at_with_access(
                    &request.agent_access,
                    &request.doc_id,
                    request.lifecycle.execution_generation().unwrap(),
                    chrono::Utc::now,
                )
                .await
                .unwrap();
                let terminal = request
                    .lifecycle
                    .terminalize_owned(
                        RequestTerminalOutcome::Completed,
                        TerminalOutput::NoMessage,
                        None,
                    )
                    .await
                    .unwrap();
                assert_eq!(terminal, TerminalizeResult::Won, "{}", trace.config);
            }
            "Reopen" => assert!(reader.set(home.client_access(&home.user)).is_ok()),
            "LoadBegin" => {
                assert!(root.is_none());
                root = Some(
                    read_node_snapshot_root(reader.get().unwrap(), home.node_did())
                        .await
                        .unwrap(),
                );
            }
            "LoadEnd" => {
                projected = Some(root.take().unwrap().finish().await.unwrap());
            }
            "Stutter" => assert!(projected.is_some()),
            action => panic!("unmapped session head action {action}"),
        }
    }
    let durable = load_node_snapshot_on(&home.user_access, home.node_did())
        .await
        .unwrap();
    let latest = durable
        .sessions
        .iter()
        .find(|s| s.session_id == "head-observation")
        .and_then(|s| s.observation.as_ref())
        .and_then(|s| s.latest_request.as_ref())
        .unwrap();
    let b = b.unwrap();
    let mut props = vec![(
        "HeadFollowsBoundary",
        !boundary_names_b || latest.request_id == b_id && latest.request_doc_id == b.doc_id,
    )];
    if let Some(state) = expected {
        props.push((
            "ReplyObserved",
            !boundary_names_b
                || projected.as_ref().unwrap().requests.iter().any(|r| {
                    r.request_id == b_id
                        && r.doc_id.as_deref() == Some(&b.doc_id)
                        && r.lifecycle_state == Some(state)
                }),
        ));
    }
    trace.check(Red::Defect, &props);
}

#[tokio::test]
async fn session_head_observation_traces() {
    for json in [
        include_str!("../../proofs/tla/generated/traces/MCSessionHeadObservationNoHeadSeed.json"),
        include_str!(
            "../../proofs/tla/generated/traces/MCSessionHeadObservationNoLivePreference.json"
        ),
        include_str!(
            "../../proofs/tla/generated/traces/MCSessionHeadObservationNoTerminalizationOrder.json"
        ),
        include_str!("../../proofs/tla/generated/traces/MCSessionHeadObservationRequestIdTie.json"),
        include_str!(
            "../../proofs/tla/generated/traces/MCSessionHeadObservationRequestIdTieLiveness.json"
        ),
    ] {
        replay(Trace::load(json, "SessionHeadObservation", &[])).await;
    }
}
