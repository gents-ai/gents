use std::sync::Arc;

use gents::test_support::tla_trace::{Action, Red, SpecTraceMap, Trace};
use gents_protocol::document_query::{
    fields::{agent_request as request, agent_session as session},
    Filter, Input, Query, Selection, Update,
};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose, RequestScope,
};
use gents_session::request::lifecycle::{
    materialize::sign_request, reserve_goal_session, submit_client_request, RequestSigner,
};
use gents_store::config_client::is_classified_transaction_conflict;
use gents_store::identity::NodeIdentity;

use crate::support::agent_requests::{canonical_now, RequestActors};
use crate::support::trial_store;

const SESSION: &str = "contended-client-session";

async fn rename_agent(actors: &RequestActors, name: String) {
    use gents_documents::apply_desired_state_plan;
    use gents_documents::{
        read_desired_state_record_in_txn, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    let node_did = actors.node_did().to_owned();
    let agent_did = actors.agent_did().to_owned();
    actors
        .fixture
        .access
        .transact_write("test.contention.rename_agent", |txn| {
            let (node_did, agent_did, name) = (&node_did, &agent_did, &name);
            Box::pin(async move {
                let (_, mut agent) = read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Agent,
                    node_did,
                    agent_did,
                )
                .await?
                .expect("selected Agent");
                agent["display_name"] = name.as_str().into();
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: gents_store::collection::Collection::Agent,
                    add: agent.clone(),
                    update: agent,
                }])?;
                apply_desired_state_plan(
                    txn,
                    &plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
                .map(|_| ())
            })
        })
        .await
        .expect("rename the selected Agent as its User");
}

#[derive(Clone, Copy)]
enum Writer {
    Publish,
    ChangeBinding,
}

const MAP: SpecTraceMap<Writer> = SpecTraceMap {
    action: |step| match step.action.as_str() {
        "AgentPublish" => Action::Environment(Writer::Publish),
        "BindingChange" => Action::Environment(Writer::ChangeBinding),
        "Compose" => Action::Internal,
        "ReserveCommit" | "SubmitCommit" => {
            assert_eq!(step.actor, Some(1), "one authored request");
            Action::Commit
        }
        action => panic!("unknown submission action {action}"),
    },
};

async fn replay(trace: &Trace) {
    let db = trial_store("client-submission-contention").await;
    let actors = Arc::new(RequestActors::new(&db).await);
    let user = &actors.fixture.user;
    let reserves = trace.constants["Reserves"].as_bool().unwrap();
    let published = trace.constants["PublishedBinding"].as_bool().unwrap();
    let mut request = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "contended-request",
        RequestScope::new(actors.node_did(), user.did(), actors.agent_did(), SESSION),
        "submitted while the Agent is writing",
        "interactive",
        canonical_now(),
        AgentRequestAdmissionRecord::client(user.did()),
    );
    if !reserves {
        let doc = actors
            .create_session_with(serde_json::json!({
                "session_id": SESSION, "node_did": actors.node_did(), "requester_did": user.did(),
                "agent_did": actors.agent_did(), "relationships_pending": !published,
                "created_at": request.created_at,
            }))
            .await;
        request.session_doc_id = Some(doc);
        request.agent_doc_id = Some(actors.agent_doc.clone());
        sign_request(&mut request, RequestSigner::Identity(user))
            .await
            .unwrap();
    }
    let replay = MAP
        .drive(trace, db.graphql_endpoint(), {
            let actors = actors.clone();
            let session_doc = request.session_doc_id.clone();
            move |action, index| {
                let (actors, session_doc) = (actors.clone(), session_doc.clone());
                async move {
                    match action {
                        Writer::Publish if reserves => {
                            rename_agent(&actors, format!("Renamed {index}")).await
                        }
                        Writer::Publish => {
                            actors
                                .seed_session_observation(
                                    SESSION,
                                    &gents_protocol::session::SessionObservation {
                                        last_activity_at: format!("2099-01-01T00:00:{index:02}Z"),
                                        preview: None,
                                        reply_preview: None,
                                        latest_request: None,
                                    },
                                )
                                .await
                        }
                        Writer::ChangeBinding => {
                            let write = Update::doc(
                                session_doc.as_deref().unwrap(),
                                Input::new().set(session::REQUESTER_DID, actors.agent_did()),
                            )
                            .render();
                            let changed = actors
                                .agent_access
                                .write_rendered("test.change_published_binding", &write)
                                .await;
                            assert!(
                                changed.is_err(),
                                "immutable session binding accepted a write: {changed:?}"
                            );
                        }
                    }
                }
            }
        })
        .await;
    let access = replay.proxy.client_access(user);
    let result = if reserves {
        reserve_goal_session(&access, &request).await.map(Some)
    } else {
        submit_client_request(&access, &request)
            .await
            .map(|()| None)
    };
    let done = result.is_ok();
    let requests = actors
        .fixture
        .access
        .read(
            &Query::select(
                Selection::doc_id()
                    .field(request::AGENT_DID)
                    .field(request::REQUESTER_DID)
                    .field(request::SESSION_DOC_ID),
            )
            .filter(Filter::eq(request::SESSION_ID, SESSION))
            .render(),
        )
        .await
        .unwrap();
    let rows = requests["data"]["AgentRequest"].as_array().unwrap();
    let sessions = actors
        .fixture
        .access
        .read(
            &Query::select(
                Selection::doc_id()
                    .field(session::AGENT_DID)
                    .field(session::REQUESTER_DID),
            )
            .filter(Filter::eq(session::SESSION_ID, SESSION))
            .render(),
        )
        .await
        .unwrap();
    let [bound] = sessions["data"]["AgentSession"]
        .as_array()
        .unwrap()
        .as_slice()
    else {
        panic!("expected one session: {sessions}");
    };
    assert_eq!(bound["requester_did"], user.did());
    assert_eq!(bound["agent_did"], actors.agent_did());
    match result {
        Ok(Some(doc)) => assert_eq!(bound["_docID"], doc),
        Ok(None) => assert_eq!(rows.len(), 1),
        Err(error) => {
            let error = anyhow::Error::new(error);
            assert!(is_classified_transaction_conflict(&error), "{error:#}");
            assert!(rows.is_empty());
        }
    }
    let commits = replay.commits();
    let writes = trace
        .steps
        .iter()
        .filter(|s| s.action == "AgentPublish")
        .count();
    let attempts = trace.constants["Attempts"].as_u64().unwrap() as usize;
    assert_eq!(commits, (writes + 1).min(attempts), "{}", trace.config);
    let red = match trace.config.as_str() {
        "MCClientSubmissionContentionShortBudget" => Red::Premise,
        "MCClientSubmissionContentionTwoPublicationsWitness" => Red::Witness,
        _ => Red::Defect,
    };
    trace.check(
        red,
        &[
            ("NoConflictLoss", done),
            ("CreatedOnce", rows.len() <= 1),
            (
                "SubmittedUnderCurrentBinding",
                rows.iter().all(|row| {
                    row["session_doc_id"] == bound["_docID"]
                        && row["agent_did"] == bound["agent_did"]
                        && row["requester_did"] == bound["requester_did"]
                }),
            ),
            ("ReservationRebuiltTwice", !(reserves && commits == 3)),
        ],
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replays_client_submission_contention() {
    for json in [
        include_str!("../../proofs/tla/generated/traces/MCClientSubmissionContentionReserveSingleAttempt.json"),
        include_str!("../../proofs/tla/generated/traces/MCClientSubmissionContentionSingleAttempt.json"),
        include_str!("../../proofs/tla/generated/traces/MCClientSubmissionContentionShortBudget.json"),
        include_str!("../../proofs/tla/generated/traces/MCClientSubmissionContentionPublishedBindingChanges.json"),
        include_str!("../../proofs/tla/generated/traces/MCClientSubmissionContentionTwoPublicationsWitness.json"),
    ] {
        replay(&Trace::load("ClientSubmissionContention", json)).await;
    }
}
