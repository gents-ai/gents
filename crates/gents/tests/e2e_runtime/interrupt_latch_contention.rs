use std::sync::Arc;

use gents::operations::request::interrupt;
use gents::test_support::tla_trace::{Action, Red, SpecTraceMap, Trace};
use gents_protocol::document_query::{
    fields::agent_request as fields, Filter, Input, Query, Selection, Update,
};
use gents_protocol::operations::request::{RequestInterruptInput, RequestRef};
use gents_session::request::lifecycle::{
    renew_execution_lease_once_at_with_access, RenewalAttemptOutcome,
};
use gents_session::session::interrupt_marker::read_for_session_in_txn;
use gents_session::test_support::{AgentHome, ClaimedRequest, Stage};
use gents_store::config_client::{is_classified_transaction_conflict, ConfigAccess};
use gents_store::test_support::UNREACHED_DEADLINE;

const SESSION: &str = "contended-interrupt-session";
const REQUEST: &str = "contended-interrupt-request";

#[derive(Clone, Copy)]
enum Writer {
    Renew,
    OtherLatch,
}

const MAP: SpecTraceMap<Writer> = SpecTraceMap {
    spec: "InterruptLatchContention",
    constants: &[(
        "Attempts",
        gents_store::config_client::TRANSACT_CONFLICT_MAX_RETRIES + 1,
    )],
    action: |step| match step.action.as_str() {
        "Renew" => Action::Environment(Writer::Renew),
        "OtherLatch" => Action::Environment(Writer::OtherLatch),
        "Commit" => Action::Commit,
        "Open" => Action::Internal,
        other => panic!("unknown interrupt action {other}"),
    },
};

fn input(home: &AgentHome) -> RequestInterruptInput {
    RequestInterruptInput {
        request: RequestRef {
            node_did: home.node_did().into(),
            requester_did: None,
            request_id: REQUEST.into(),
        },
    }
}

fn operator(access: ConfigAccess) -> ConfigAccess {
    let ConfigAccess::Graphql(endpoint) = access else {
        panic!("HTTP operator access")
    };
    ConfigAccess::Graphql(gents::runtime::lifecycle_hooks::compose_endpoint(endpoint).unwrap())
}

async fn request_row(home: &AgentHome, doc: &str) -> serde_json::Value {
    let read = Query::select(
        Selection::new()
            .field(fields::LIFECYCLE_STATE)
            .field(fields::EXECUTION_GENERATION)
            .field(fields::EXECUTION_LEASE_EXPIRES_AT)
            .field(fields::TERMINALIZED_AT)
            .field(fields::INTERRUPT_GENERATION),
    )
    .filter(Filter::eq(fields::DOC_ID, doc))
    .render();
    home.agent_access.read(&read).await.unwrap()["data"]["AgentRequest"][0].clone()
}

async fn renew(home: &AgentHome, doc: &str, stage: Stage, index: usize) {
    if stage == Stage::Submitted {
        let expiry = format!("2099-01-01T00:00:{index:02}Z");
        let write = Update::doc(
            doc,
            Input::new().set(fields::EXECUTION_LEASE_EXPIRES_AT, expiry),
        )
        .render();
        home.agent_access
            .write_rendered("test.pending_request_write", &write)
            .await
            .unwrap();
    } else {
        let row = request_row(home, doc).await;
        let deadline = row["execution_lease_expires_at"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let outcome = renew_execution_lease_once_at_with_access(
            &home.agent_access,
            doc,
            row["execution_generation"].as_str().unwrap(),
            deadline,
            deadline - chrono::TimeDelta::milliseconds(1),
        )
        .await
        .unwrap();
        assert_eq!(outcome, RenewalAttemptOutcome::Committed);
    }
}

async fn replay(trace: &Trace, stage: Stage) {
    let home = Arc::new(
        AgentHome::start_with(
            "interrupt-trace",
            gents_automation::lifecycle_hooks::composition(),
        )
        .await,
    );
    let request = ClaimedRequest::client_signed(&home, &home.binding)
        .ids(REQUEST, SESSION)
        .deadline_secs(UNREACHED_DEADLINE.as_secs())
        .lease(UNREACHED_DEADLINE)
        .drive(stage)
        .await;
    let other_stamp = Arc::new(std::sync::OnceLock::new());
    let replay = MAP
        .drive(trace, home.graphql_endpoint(), None, {
            let home = home.clone();
            let doc = request.doc_id.clone();
            let stamp = other_stamp.clone();
            move |writer, index| {
                let (home, doc, stamp) = (home.clone(), doc.clone(), stamp.clone());
                async move {
                    match writer {
                        Writer::Renew => renew(&home, &doc, stage, index).await,
                        Writer::OtherLatch => {
                            let first = interrupt(&home.user_access, input(&home)).await.unwrap();
                            assert!(!first.already_interrupted);
                            stamp.set(first.interrupted_at.unwrap()).unwrap();
                        }
                    }
                }
            }
        })
        .await;
    let receipt = interrupt(
        &operator(replay.proxy.client_access(&home.user)),
        input(&home),
    )
    .await;
    let done = receipt.is_ok();
    let commits = replay.commits();
    let writes = trace
        .steps
        .iter()
        .filter(|s| matches!((MAP.action)(s), Action::Environment(_)))
        .count();
    let attempts = trace.constants["Attempts"].as_u64().unwrap() as usize;
    assert_eq!(
        commits,
        (writes + 1).min(attempts),
        "{} {stage:?}",
        trace.config
    );
    let row = request_row(&home, &request.doc_id).await;
    let marker = home
        .agent_access
        .transact_readonly("test.observe_interrupt", |txn| {
            let node = home.node_did();
            Box::pin(async move { read_for_session_in_txn(txn, node, SESSION).await })
        })
        .await
        .unwrap()
        .unwrap();
    let (stamp, bumps) = if stage == Stage::Submitted {
        let terminal = row["lifecycle_state"] == "interrupted";
        assert_eq!(marker.generation, 0);
        (row["terminalized_at"].as_str(), i64::from(terminal))
    } else {
        (
            marker.interrupted_at.as_deref(),
            marker.generation - row["interrupt_generation"].as_i64().unwrap(),
        )
    };
    if let Ok(receipt) = receipt {
        assert_eq!(receipt.already_interrupted, other_stamp.get().is_some());
        if stage != Stage::Submitted || !receipt.already_interrupted {
            assert_eq!(receipt.interrupted_at.as_deref(), stamp);
        }
    } else {
        let error = anyhow::Error::new(receipt.unwrap_err());
        assert!(is_classified_transaction_conflict(&error), "{error:#}");
        assert!(stamp.is_none());
    }
    if let Some(first) = other_stamp.get() {
        assert_eq!(stamp, Some(first.as_str()));
    }
    let red = match trace.config.as_str() {
        "MCInterruptLatchContentionExhausted" => Red::Premise,
        "MCInterruptLatchContentionTwoRenewalsWitness" => Red::Witness,
        _ => Red::Defect,
    };
    trace.check(
        red,
        &[
            ("NoConflictLoss", done),
            ("StampedOnce", bumps <= 1),
            ("DoneIsLatched", !done || (bumps == 1 && stamp.is_some())),
            ("TwoRenewalsSettled", !(done && commits == 3)),
        ],
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replays_interrupt_latch_contention() {
    for json in [
        include_str!(
            "../../proofs/tla/generated/traces/MCInterruptLatchContentionSingleAttempt.json"
        ),
        include_str!(
            "../../proofs/tla/generated/traces/MCInterruptLatchContentionStaleRebuild.json"
        ),
        include_str!("../../proofs/tla/generated/traces/MCInterruptLatchContentionExhausted.json"),
        include_str!(
            "../../proofs/tla/generated/traces/MCInterruptLatchContentionTwoRenewalsWitness.json"
        ),
    ] {
        let trace = MAP.load(json);
        replay(&trace, Stage::Claimed).await;
        if matches!(
            trace.config.as_str(),
            "MCInterruptLatchContentionTwoRenewalsWitness"
                | "MCInterruptLatchContentionStaleRebuild"
        ) {
            replay(&trace, Stage::Submitted).await;
        }
    }
}
