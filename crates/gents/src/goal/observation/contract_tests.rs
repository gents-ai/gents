use super::*;
use crate::goal::operator_resume::support::{Fixture, SESSION};
use futures::FutureExt;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
struct Contracts {
    goal_observation_cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    scope: Scope,
    goal: Option<Reference>,
    verified: bool,
    accepted: bool,
    recovery: Option<RecoveryTrace>,
}

#[derive(Deserialize)]
struct RecoveryTrace {
    before: Option<Reference>,
    after: Option<Reference>,
}

#[derive(Deserialize)]
struct Scope {
    node: String,
    session: String,
    session_document: String,
    requester: Option<String>,
}

#[derive(Deserialize)]
struct Reference {
    node: String,
    session: String,
    document: String,
    composite: String,
}

async fn fixture() -> Fixture {
    let before = json!({"status":"active","sequence":0,"last_continued_from":null,
        "latest_request":10,"children":[],"blocked_audits":0,"wrapup_requested":false,
        "wrapup_completed":false,"tokens_used":37,"token_budget":1000});
    let f = Fixture::new(&before).await;
    crate::session::ensure_session_with_agent_id_and_requester_did(
        &f.node,
        SESSION,
        f.identity.did(),
        "contract-behavior",
        Some(f.identity.did()),
    )
    .await
    .unwrap();
    f
}

async fn session(f: &Fixture) -> SessionOwnerRow {
    ConfigAccess::local(f.node.clone())
        .transact_readonly("test.observed_session", |txn| {
            Box::pin(async {
                crate::session::load_agent_session_owner_in_txn(txn, f.identity.did(), SESSION)
                    .await
            })
        })
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn generated_goal_observations_drive_signature_and_scope_validation() {
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(contracts.goal_observation_cases.len(), 288);
    let f = fixture().await;
    let current = session(&f).await;
    let native = load_canonical_goal(&f.node, f.identity.did(), SESSION)
        .await
        .unwrap()
        .unwrap();
    let map = |value: &str| match value {
        "node" | "requester" => f.identity.did().to_owned(),
        "session" => SESSION.to_owned(),
        "session-document" => current.doc_id.clone(),
        other => other.to_owned(),
    };
    for case in contracts.goal_observation_cases {
        let mut observation = SessionGoalObservation {
            version: SESSION_GOAL_CONTROL_VERSION,
            node_did: map(&case.scope.node),
            session_id: map(&case.scope.session),
            session_doc_id: map(&case.scope.session_document),
            requester_did: case.scope.requester.as_deref().map(map),
            observed_at: Utc::now().to_rfc3339(),
            goal: case.goal.map(|reference| {
                let mut snapshot = snapshot(&native, Utc::now());
                snapshot.node_did = map(&reference.node);
                snapshot.session_id = map(&reference.session);
                ObservedGoal {
                    assignment_root_request_doc_id: native.assignment_root_request_doc_id.clone(),
                    revision: GoalRevision {
                        document: reference.document,
                        composite: reference.composite,
                    },
                    snapshot,
                }
            }),
            signature: Vec::new(),
        };
        observation.signature = f
            .identity
            .sign(&observation.signing_payload().unwrap())
            .await
            .unwrap();
        if !case.verified {
            observation.signature[0] ^= 1;
        }
        let row = ObservationRow {
            doc_id: "observation-document".into(),
            node_did: observation.node_did.clone(),
            session_id: observation.session_id.clone(),
            session_doc_id: observation.session_doc_id.clone(),
            requester_did: observation.requester_did.clone(),
            observation_json: Some(serde_json::to_string(&observation).unwrap()),
        };
        assert_eq!(
            verified(f.identity.as_ref(), &current, &row)
                .await
                .unwrap()
                .is_some(),
            case.accepted,
            "observation: {observation:?}"
        );
    }
    f.node.shutdown().await;
}

async fn durable_rows(f: &Fixture) -> Value {
    ConfigAccess::local(f.node.clone())
        .execute("{ GoalObservation { _docID _version { cid } observation_json } }")
        .await
        .unwrap()["data"]["GoalObservation"]
        .clone()
}

struct ObservedTicks {
    waiting: Arc<tokio::sync::Notify>,
    ticks: futures::channel::mpsc::UnboundedReceiver<()>,
}

impl futures::Stream for ObservedTicks {
    type Item = ();

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<()>> {
        let result = std::pin::Pin::new(&mut self.ticks).poll_next(context);
        if result.is_pending() {
            self.waiting.notify_one();
        }
        result
    }
}

#[tokio::test]
async fn generated_goal_rescans_recover_lost_notifications() {
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    let traces = contracts
        .goal_observation_cases
        .into_iter()
        .filter_map(|case| case.recovery)
        .collect::<Vec<_>>();
    assert_eq!(
        traces.len(),
        2,
        "Lean emits Goal replacement and removal traces"
    );
    for trace in traces {
        for closed_feed in [false, true] {
            let f = fixture().await;
            let identity: Arc<dyn NodeIdentity> = f.identity.clone();
            let access = ConfigAccess::local(f.node.clone());
            let cancel = CancellationToken::new();
            let (changes_tx, changes) = futures::channel::mpsc::unbounded();
            let changes_tx = (!closed_feed).then_some(changes_tx);
            let (ticks_tx, ticks) = futures::channel::mpsc::unbounded();
            let waiting = Arc::new(tokio::sync::Notify::new());
            let run = crate::agent::p2p_reconcile::session_commands::run_session_command_reconciler_observing(
                f.node.clone(), identity, cancel.clone(), changes,
                ObservedTicks { waiting: waiting.clone(), ticks },
            );
            tokio::pin!(run);
            tokio::select! {
                result = &mut run => panic!("reconciler stopped before its first wait: {result:?}"),
                () = waiting.notified() => {}
                () = tokio::time::sleep(std::time::Duration::from_secs(10)) => panic!("reconciler did not reach its first wait"),
            }
            let previous = load_session_goal_observation(
                &access,
                f.identity.as_ref(),
                f.identity.did(),
                SESSION,
                Some(f.identity.did()),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(previous.goal.is_some(), trace.before.is_some());
            let old_revision = previous.goal.as_ref().unwrap().revision.clone();
            access.transact_write("test.missed_goal_notification", |txn| Box::pin(async {
                let (document, variables) = if trace.after.is_some() {
                    ("mutation($doc: ID!, $input: GoalMutationInputArg!) { update_Goal(docID: $doc, input: $input) { _docID } }",
                     json!({"doc":f.goal.doc_id,"input":{"tokens_used":38}}))
                } else {
                    ("mutation($doc: ID!) { delete_Goal(docID: $doc) { _docID } }",
                     json!({"doc":f.goal.doc_id}))
                };
                txn.execute_with_variables(document, &variables).await?;
                Ok::<_, anyhow::Error>(())
            })).await.unwrap();
            let canonical = access
                .transact_readonly("test.current_goal_revision", |txn| {
                    Box::pin(async {
                        let Some(goal) =
                            load_canonical_goal_in_txn(txn, f.identity.did(), SESSION).await?
                        else {
                            return Ok::<_, anyhow::Error>(None);
                        };
                        let commit =
                            crate::store::graphql::newest_document_composite_commit_in_txn(
                                txn,
                                &goal.doc_id,
                                "test.current_goal_revision",
                            )
                            .await?
                            .unwrap();
                        Ok(Some(GoalRevision {
                            document: goal.doc_id,
                            composite: commit.cid,
                        }))
                    })
                })
                .await
                .unwrap();
            assert_eq!(canonical.is_some(), trace.after.is_some());
            let stale = load_session_goal_observation(
                &access,
                f.identity.as_ref(),
                f.identity.did(),
                SESSION,
                Some(f.identity.did()),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(
                stale.goal.unwrap().revision,
                old_revision,
                "lost notification must leave the previous projection until the sweep"
            );
            let _ = waiting.notified().now_or_never();
            ticks_tx.unbounded_send(()).unwrap();
            tokio::select! {
                result = &mut run => panic!("reconciler stopped during durable refresh: {result:?}"),
                () = waiting.notified() => {}
                () = tokio::time::sleep(std::time::Duration::from_secs(10)) => panic!("reconciler did not finish its durable refresh"),
            }
            let refreshed = load_session_goal_observation(
                &access,
                f.identity.as_ref(),
                f.identity.did(),
                SESSION,
                Some(f.identity.did()),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(refreshed.goal.as_ref().map(|goal| &goal.revision), canonical.as_ref(),
                "periodic durable observation must select the canonical Goal without a notification");
            assert_eq!(refreshed.goal.is_some(), trace.after.is_some());
            cancel.cancel();
            run.await.unwrap();
            drop(changes_tx);
            f.node.shutdown().await;
        }
    }
}

#[tokio::test]
async fn invalid_session_does_not_starve_other_goal_observations() {
    let f = fixture().await;
    let identity: Arc<dyn NodeIdentity> = f.identity.clone();
    let access = ConfigAccess::local(f.node.clone());
    access
        .write(
            "test.incomplete_goal_session",
            &format!(
                r#"mutation {{ create_AgentSession(input: {{
        node_did: "{}", session_id: "bad-session"
    }}) {{ _docID }} }}"#,
                escape_graphql_string(identity.did())
            ),
        )
        .await
        .unwrap();
    assert!(reconcile_goal_observations(&f.node, &identity, true, &[])
        .await
        .is_err());
    assert!(load_session_goal_observation(
        &access,
        identity.as_ref(),
        identity.did(),
        SESSION,
        Some(identity.did())
    )
    .await
    .unwrap()
    .unwrap()
    .goal
    .is_some());
    let settled = durable_rows(&f).await;
    assert!(reconcile_goal_observations(&f.node, &identity, true, &[])
        .await
        .is_err());
    assert_eq!(durable_rows(&f).await, settled);
    f.node.shutdown().await;
}

#[tokio::test]
async fn goal_projection_is_signed_scoped_and_settles_without_writes() {
    let f = fixture().await;
    let identity: Arc<dyn NodeIdentity> = f.identity.clone();
    let access = ConfigAccess::local(f.node.clone());
    assert!(load_session_goal_observation(
        &access,
        identity.as_ref(),
        identity.did(),
        SESSION,
        Some(identity.did())
    )
    .await
    .unwrap()
    .is_none());
    project_session_goal(&f.node, &identity, SESSION, Some(identity.did()))
        .await
        .unwrap();
    let first = load_session_goal_observation(
        &access,
        identity.as_ref(),
        identity.did(),
        SESSION,
        Some(identity.did()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        first.goal.as_ref().unwrap().snapshot.goal_id,
        f.goal.goal_id
    );
    let settled = durable_rows(&f).await;
    project_session_goal(&f.node, &identity, SESSION, Some(identity.did()))
        .await
        .unwrap();
    assert_eq!(durable_rows(&f).await, settled);
    let id = settled[0]["_docID"].as_str().unwrap();
    access.write("test.forged_goal_observation", &format!(r#"mutation {{ update_GoalObservation(docID: "{}", input: {{ observation_json: "{}" }}) {{ _docID }} }}"#,
        escape_graphql_string(id), escape_graphql_string("{}"))).await.unwrap();
    assert!(load_session_goal_observation(
        &access,
        identity.as_ref(),
        identity.did(),
        SESSION,
        Some(identity.did())
    )
    .await
    .unwrap()
    .is_none());
    project_session_goal(&f.node, &identity, SESSION, Some(identity.did()))
        .await
        .unwrap();
    assert!(load_session_goal_observation(
        &access,
        identity.as_ref(),
        identity.did(),
        SESSION,
        Some(identity.did())
    )
    .await
    .unwrap()
    .unwrap()
    .goal
    .is_some());
    let goal = load_canonical_goal(&f.node, identity.did(), SESSION)
        .await
        .unwrap()
        .unwrap();
    delete_goal(&f.node, &goal).await.unwrap();
    project_session_goal(&f.node, &identity, SESSION, Some(identity.did()))
        .await
        .unwrap();
    let empty = load_session_goal_observation(
        &access,
        identity.as_ref(),
        identity.did(),
        SESSION,
        Some(identity.did()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(empty.goal.is_none());
    let settled = durable_rows(&f).await;
    project_session_goal(&f.node, &identity, SESSION, Some(identity.did()))
        .await
        .unwrap();
    assert_eq!(durable_rows(&f).await, settled);
    f.node.shutdown().await;
}

#[tokio::test]
async fn retained_signed_observation_refreshes_its_root_without_changing_goal_revision() {
    let f = fixture().await;
    let identity: Arc<dyn NodeIdentity> = f.identity.clone();
    let access = ConfigAccess::local(f.node.clone());
    access.write("test.goal_physical_root", &format!(
        "mutation {{update_Goal(docID: \"{}\", input: {{assignment_root_request_doc_id: \"{}\"}}) {{_docID}}}}",
        escape_graphql_string(&f.goal.doc_id), escape_graphql_string(&f.parent.doc_id))).await.unwrap();
    project_session_goal(&f.node, &identity, SESSION, Some(f.identity.did()))
        .await
        .unwrap();
    let mut retained = load_session_goal_observation(
        &access,
        f.identity.as_ref(),
        f.identity.did(),
        SESSION,
        Some(f.identity.did()),
    )
    .await
    .unwrap()
    .unwrap();
    let revision = retained.goal.as_ref().unwrap().revision.clone();
    assert_eq!(
        retained
            .goal
            .as_ref()
            .unwrap()
            .assignment_root_request_doc_id
            .as_deref(),
        Some(f.parent.doc_id.as_str())
    );
    retained
        .goal
        .as_mut()
        .unwrap()
        .assignment_root_request_doc_id = None;
    retained.signature = f
        .identity
        .sign(&retained.signing_payload().unwrap())
        .await
        .unwrap();
    assert!(serde_json::to_value(retained.goal.as_ref().unwrap())
        .unwrap()
        .get("assignment_root_request_doc_id")
        .is_none());
    let rows = durable_rows(&f).await;
    let doc = rows[0]["_docID"].as_str().unwrap();
    access.write("test.retained_goal_observation", &format!(
        "mutation {{update_GoalObservation(docID: \"{}\", input: {{observation_json: \"{}\"}}) {{_docID}}}}",
        escape_graphql_string(doc),escape_graphql_string(&serde_json::to_string(&retained).unwrap()))).await.unwrap();
    assert!(load_session_goal_observation(
        &access,
        f.identity.as_ref(),
        f.identity.did(),
        SESSION,
        Some(f.identity.did())
    )
    .await
    .unwrap()
    .unwrap()
    .goal
    .unwrap()
    .assignment_root_request_doc_id
    .is_none());
    project_session_goal(&f.node, &identity, SESSION, Some(f.identity.did()))
        .await
        .unwrap();
    let refreshed = load_session_goal_observation(
        &access,
        f.identity.as_ref(),
        f.identity.did(),
        SESSION,
        Some(f.identity.did()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(refreshed.goal.as_ref().unwrap().revision, revision);
    assert_eq!(
        refreshed
            .goal
            .as_ref()
            .unwrap()
            .assignment_root_request_doc_id
            .as_deref(),
        Some(f.parent.doc_id.as_str())
    );
    let stable = durable_rows(&f).await;
    project_session_goal(&f.node, &identity, SESSION, Some(f.identity.did()))
        .await
        .unwrap();
    assert_eq!(durable_rows(&f).await, stable);
    let mut substituted = refreshed;
    substituted
        .goal
        .as_mut()
        .unwrap()
        .assignment_root_request_doc_id = Some("another-physical-request".into());
    access.write("test.substituted_goal_root", &format!(
        "mutation {{update_GoalObservation(docID: \"{}\", input: {{observation_json: \"{}\"}}) {{_docID}}}}",
        escape_graphql_string(doc),escape_graphql_string(&serde_json::to_string(&substituted).unwrap()))).await.unwrap();
    assert!(load_session_goal_observation(
        &access,
        f.identity.as_ref(),
        f.identity.did(),
        SESSION,
        Some(f.identity.did())
    )
    .await
    .unwrap()
    .is_none());
    f.node.shutdown().await;
}
