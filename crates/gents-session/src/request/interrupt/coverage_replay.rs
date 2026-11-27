use super::*;
use crate::session::message::interrupt_session_with_access;
use gents_lean_vocab::tla::replay::{Red, Trace};
use gents_protocol::session::SessionScope;
use gents_store::test_support::paused_transaction::PausedTransaction;

fn scope(request: &AgentRequest) -> SessionScope {
    SessionScope {
        node_did: request.node_did.clone(),
        session_id: request.session_id.clone(),
        requester_did: request.requester_did.clone(),
    }
}

async fn claimed(trace: &Trace) {
    let mut fixture = unclaimed_fixture("coverage-claim", "100".into(), "900".into()).await;
    let request = fixture.lifecycle.request().clone();
    let mut claim = Some(PausedTransaction::before_commit(
        fixture.lifecycle.claim_with_identity(),
    ));
    let scope = scope(&request);
    let mut interrupt = Some(PausedTransaction::before_commit(
        interrupt_session_with_access(&fixture.home.node, &fixture.agent_access, &scope),
    ));
    for step in &trace.steps {
        match step.action.as_str() {
            "ClaimRead" => assert!(claim.as_mut().unwrap().advance().await),
            "ClaimCommit" => assert_eq!(
                claim.take().unwrap().finish().await.unwrap(),
                crate::request::lifecycle::ClaimOutcome::Claimed
            ),
            "Begin" => assert!(interrupt.as_mut().unwrap().advance().await),
            "Commit" => {
                interrupt.take().unwrap().finish().await.unwrap();
            }
            action => panic!("unmapped claim action {action}"),
        }
    }
    trace.check(
        Red::Defect,
        &[(
            "InterruptedRequestDoesNotRun",
            latch(&fixture.agent_access, &request).await.is_some(),
        )],
    );
}

async fn queued(trace: &Trace) {
    let case: ModeledCase = serde_json::from_value(json!({
        "name":trace.config,"node_id":1,"requester_id":1,"session_id":902,
        "active_request_id":102,"inputs":[]
    }))
    .unwrap();
    let fixture = fixture(&case).await;
    let (node, access, parent) = (&fixture.home.node, &fixture.agent_access, &fixture.parent);
    let scope = scope(parent);
    let mut interrupt =
        PausedTransaction::each_mutation(interrupt_session_with_access(node, access, &scope));
    let mut wakes = RapidHashMap::default();
    let mut docs = Vec::new();
    let mut first = true;
    let mut settled = false;
    let mut rivals = 0;
    for (index, step) in trace.steps.iter().enumerate() {
        match step.action.as_str() {
            "WakeRead" => {
                let actor = step.actor.as_ref().unwrap()["$model"].as_str().unwrap();
                let id: u64 = actor.strip_prefix('w').unwrap().parse().unwrap();
                let case = &case;
                let mut publish = PausedTransaction::before_commit(async move {
                    let entry = serde_json::from_value(json!({
                        "request_id":310+id,"created_at":10,"execution_origin":"scheduled",
                        "source":"background_completion","policy":"coalesce","queue_key":910+id,"queued_after":null
                    })).unwrap();
                    enqueue_generated_wake(access, node, parent, case, &entry).await
                });
                assert!(publish.advance().await);
                assert!(wakes.insert(actor, publish).is_none());
            }
            "WakeCommit" => {
                let actor = step.actor.as_ref().unwrap()["$model"].as_str().unwrap();
                docs.push((wakes.remove(actor).unwrap().finish().await, !settled));
            }
            "Begin" if first => {
                assert!(interrupt.advance().await);
                first = false;
            }
            "Begin" => {}
            "Abort" | "Commit" => settled = !interrupt.advance().await,
            "OtherInterrupt" => {
                interrupt_session_with_access(node, access, &scope)
                    .await
                    .unwrap();
                rivals += 1;
            }
            "OtherWrite" => {
                crate::session::observations::update_session_title_with_source(
                    access,
                    &parent.node_did,
                    parent.requester_did.as_deref(),
                    &parent.session_id,
                    &format!("write {index}"),
                    gents_protocol::session::SessionTitleSource::Generated,
                )
                .await
                .unwrap();
            }
            action => panic!("unmapped interrupt action {action}"),
        }
    }
    assert!(wakes.is_empty());
    let result = interrupt.finish().await;
    let landed = result.is_ok();
    let red = if rivals > 0 {
        assert_eq!(rivals, trace.constants["Attempts"].as_u64().unwrap());
        assert!(
            gents_store::config_client::is_classified_transaction_conflict(&anyhow::Error::new(
                result.unwrap_err()
            ))
        );
        Red::Premise
    } else {
        Red::Defect
    };
    claim_pending(&fixture).await;
    let rows = rows(access, &parent.session_id).await;
    let covered = docs.iter().filter(|(_, covered)| *covered).all(|(doc, _)| {
        rows.iter()
            .any(|r| r["_docID"] == *doc && r["lifecycle_state"] == "interrupted")
    });
    trace.check(
        red,
        &[
            ("InterruptNotPrevented", landed),
            (
                "InterruptedRequestDoesNotRun",
                covered && latch(access, parent).await.is_some(),
            ),
        ],
    );
}

#[tokio::test]
async fn interrupt_coverage_traces() {
    for json in [
        include_str!("../../../../gents/proofs/tla/generated/traces/MCInterruptCoverage.sample-1.json"),
        include_str!("../../../../gents/proofs/tla/generated/traces/MCInterruptCoverage.sample-2.json"),
        include_str!("../../../../gents/proofs/tla/generated/traces/MCInterruptCoverageClaimSkipsMarker.json"),
        include_str!("../../../../gents/proofs/tla/generated/traces/MCInterruptCoverageWakeSkipsMarker.json"),
        include_str!("../../../../gents/proofs/tla/generated/traces/MCInterruptCoverageInterruptReads.json"),
        include_str!("../../../../gents/proofs/tla/generated/traces/MCInterruptCoverageRivalInterrupts.json"),
    ] {
        let trace = Trace::load(json, "InterruptCoverage", &[("Attempts", gents_store::config_client::TRANSACT_CONFLICT_MAX_RETRIES + 1)]);
        if trace.constants["ClaimReadsMarker"] == false { claimed(&trace).await; } else { queued(&trace).await; }
    }
}
