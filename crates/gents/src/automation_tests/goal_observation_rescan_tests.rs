use std::sync::Arc;

use futures::FutureExt;
use gents_automation::goal::operator_resume::support::{Fixture, SESSION};
use gents_automation::goal::*;
use gents_store::identity::NodeIdentity;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
struct Contracts {
    goal_observation_cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    recovery: Option<RecoveryTrace>,
}

#[derive(Deserialize)]
struct RecoveryTrace {
    before: Option<serde_json::Value>,
    after: Option<serde_json::Value>,
}

async fn fixture() -> Fixture {
    let before = json!({"status":"active","sequence":0,"last_continued_from":null,
        "latest_request":10,"children":[],"blocked_audits":0,"wrapup_requested":false,
        "wrapup_completed":false,"tokens_used":37,"token_budget":1000});
    Fixture::new(&before).await
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
            let access = f.agent_access();
            let cancel = CancellationToken::new();
            let (changes_tx, changes) = futures::channel::mpsc::unbounded();
            let changes_tx = (!closed_feed).then_some(changes_tx);
            let (ticks_tx, ticks) = futures::channel::mpsc::unbounded();
            let waiting = Arc::new(tokio::sync::Notify::new());
            let unavailable = gents_automation::triggers::runtime_snapshot::UnavailableAgent::new(
                gents_protocol::node_readiness::AgentReadinessUnavailableReason::BackendNotConfigured,
                "fixture",
            ).with_watcher_identity(f.agent.clone());
            let snapshot = Arc::new(gents_automation::triggers::runtime_snapshot::ActiveRuntimeSnapshot {
                generation: 1,
                node: None,
                local_did: f.identity.did().into(),
                default_agent_did: f.agent.did().into(),
                agents: Default::default(),
                tool_surfaces: Default::default(),
                tool_configs: Default::default(),
                backend_admission_configs: Default::default(),
                unavailable_agents: rapidhash::RapidHashMap::from_iter([(
                    f.agent.did().to_owned(),
                    unavailable,
                )]),
                active_schedules: Default::default(),
                unavailable_schedules: Default::default(),
                active_event_triggers: Default::default(),
                unavailable_event_triggers: Default::default(),
                active_tasks: Default::default(),
                dispatchers: Default::default(),
                agent_executor_capacities: Default::default(),
                agent_executor_queue_capacities: Default::default(),
            });
            let (_snapshot_tx, snapshot_rx) = tokio::sync::watch::channel(snapshot);
            let run =
                crate::runtime::agent::session_commands::run_session_command_reconciler_observing(
                    f.node.clone(),
                    identity,
                    cancel.clone(),
                    changes,
                    ObservedTicks {
                        waiting: waiting.clone(),
                        ticks,
                    },
                    Some(snapshot_rx),
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
                Some(f.user.did()),
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
                        let commit = gents_store::graphql::newest_document_composite_commit_in_txn(
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
                Some(f.user.did()),
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
                Some(f.user.did()),
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
            f.node.shutdown().await.expect("node shutdown");
        }
    }
}

