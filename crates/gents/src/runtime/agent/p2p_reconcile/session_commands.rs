use rapidhash::{HashMapExt, RapidHashMap};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::Node as EmbeddedNode;
use futures::{Stream, StreamExt};
use gents_protocol::session_input_edit::SessionInputEdit;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::store::graphql::escape_graphql_string;
use crate::store::identity::NodeIdentity;

#[derive(Deserialize)]
pub(super) struct CommandRow {
    #[serde(rename = "_docID")]
    pub(super) doc_id: String,
    pub(super) command_id: String,
    pub(super) requester_did: String,
    pub(super) requester_peer_id: Option<String>,
    pub(super) node_did: String,
    #[serde(default)]
    pub(super) agent_did: Option<String>,
    pub(super) session_id: String,
    #[serde(default)]
    pub(super) session_doc_id: Option<String>,
    pub(super) intent_json: String,
    pub(super) receipt_json: Option<String>,
}

pub(super) const FIELDS: &str = "_docID command_id requester_did requester_peer_id node_did session_id intent_json receipt_json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum CommandKind {
    InputEdit,
    GoalControl,
}

impl CommandKind {
    const ALL: [Self; 2] = [Self::InputEdit, Self::GoalControl];
    fn collection(self) -> &'static str {
        match self {
            Self::InputEdit => "AgentSessionInputEdit",
            Self::GoalControl => "AgentSessionGoalControl",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Sweep {
    Unanswered,
    /// Replication can overwrite a receipt without the command's notification
    /// arriving, so this mode also runs at startup, on resync, and after the
    /// subscription closes.
    Unverified,
}

pub(super) async fn unanswered_commands(
    kind: CommandKind,
    node: &EmbeddedNode,
    identity: &dyn NodeIdentity,
    sweep: Sweep,
) -> Result<Vec<String>> {
    let collection = kind.collection();
    let (receipt_filter, fields) = match sweep {
        Sweep::Unanswered => (", receipt_json: { _eq: null }", "_docID"),
        Sweep::Unverified => ("", "_docID intent_json receipt_json"),
    };
    let response = crate::store::graphql::graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ {collection}(filter: {{ node_did: {{ _eq: "{}" }}{receipt_filter} }}) {{ {fields} }} }}"#,
            escape_graphql_string(identity.did())
        ),
        "session.signed_commands",
    )
    .await?;
    if matches!(sweep, Sweep::Unanswered) {
        #[derive(Deserialize)]
        struct DocIdRow {
            #[serde(rename = "_docID")]
            doc_id: String,
        }
        return Ok(
            crate::store::graphql::rows::<DocIdRow>(&response, collection)?
                .into_iter()
                .map(|row| row.doc_id)
                .collect(),
        );
    }

    #[derive(Deserialize)]
    struct Row {
        #[serde(rename = "_docID")]
        doc_id: String,
        intent_json: Option<String>,
        receipt_json: Option<String>,
    }
    let mut unanswered = Vec::new();
    for row in crate::store::graphql::rows::<Row>(&response, collection)? {
        let Some(intent) = row.intent_json.as_deref() else {
            if row.receipt_json.is_none() {
                unanswered.push(row.doc_id);
            }
            continue;
        };
        let answered = if kind == CommandKind::GoalControl {
            super::session_goal_control::has_verified_receipt(
                identity,
                intent,
                row.receipt_json.as_deref(),
            )
            .await?
        } else {
            match serde_json::from_str::<SessionInputEdit>(intent) {
                Ok(command) => super::session_input_edit::verified_receipt(
                    identity,
                    &command,
                    row.receipt_json.as_deref(),
                )
                .await?
                .is_some(),
                Err(_) => row.receipt_json.is_some(),
            }
        };
        if !answered {
            unanswered.push(row.doc_id);
        }
    }
    Ok(unanswered)
}

pub(crate) async fn unanswered_goal_commands(
    node: &Arc<EmbeddedNode>,
    runtime: &dyn NodeIdentity,
    agent: &::identity::Did,
    _sweep: Sweep,
) -> Result<Vec<String>> {
    let query = format!(
        r#"{{ AgentSessionGoalControl(filter: {{ node_did: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }} }}) {{ _docID intent_json }} AgentSessionGoalReceipt {{ command_doc_id receipt_json relationships_pending }} }}"#,
        escape_graphql_string(runtime.did()),
        escape_graphql_string(agent.as_str())
    );
    let response = crate::ConfigAccess::transact_local_readonly(
        node,
        Some(agent.clone()),
        "goal_control.agent_sweep",
        |txn| {
            let query = &query;
            Box::pin(async move { txn.execute(query).await })
        },
    )
    .await?;
    let rows = response["data"]["AgentSessionGoalControl"]
        .as_array()
        .context("goal command observation omitted rows")?;
    let receipts = response["data"]["AgentSessionGoalReceipt"]
        .as_array()
        .context("goal receipt observation omitted rows")?;
    let mut receipts_by_command = RapidHashMap::new();
    for receipt in receipts {
        let id = receipt["command_doc_id"]
            .as_str()
            .context("goal receipt lacks command identity")?;
        anyhow::ensure!(
            receipts_by_command
                .insert(
                    id,
                    (
                        receipt["receipt_json"].as_str(),
                        receipt["relationships_pending"] == true
                    )
                )
                .is_none(),
            "goal receipt identity is ambiguous"
        );
    }
    let mut pending = Vec::new();
    for row in rows {
        let doc_id = row["_docID"]
            .as_str()
            .context("goal command lacks document identity")?;
        let (raw_receipt, relationships_pending) = receipts_by_command
            .get(doc_id)
            .copied()
            .unwrap_or((None, false));
        let answered = match row["intent_json"].as_str() {
            Some(intent) => {
                super::session_goal_control::has_verified_receipt(runtime, intent, raw_receipt)
                    .await?
            }
            None => raw_receipt.is_some(),
        };
        if !answered || relationships_pending {
            pending.push(doc_id.to_owned());
        }
    }
    Ok(pending)
}

/// Reconcile session commands without a runtime agent snapshot. Goal command
/// discovery requires a snapshot and is available through the runtime owner.
pub async fn run_session_command_reconciler(
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    cancel: CancellationToken,
) -> Result<()> {
    run_session_command_reconciler_with_agents(node, identity, None, cancel).await
}

pub(crate) async fn run_session_command_reconciler_with_agents(
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    agents: Option<
        tokio::sync::watch::Receiver<Arc<crate::runtime::snapshot::ActiveRuntimeSnapshot>>,
    >,
    cancel: CancellationToken,
) -> Result<()> {
    let watched = CommandKind::ALL
        .into_iter()
        .map(CommandKind::collection)
        .chain(crate::goal::GOAL_OBSERVATION_SOURCES.iter().copied());
    let changes = crate::subscribe_collections(&node, watched)?;
    let changes = futures::stream::unfold(changes, |mut changes| async move {
        changes.recv().await.map(|batch| (batch, changes))
    });
    let mut interval = tokio::time::interval(super::intervals::sweep_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let ticks = futures::stream::unfold(interval, |mut interval| async move {
        interval.tick().await;
        Some(((), interval))
    });
    futures::pin_mut!(changes, ticks);
    run_session_command_reconciler_observing(node, identity, cancel, changes, ticks, agents).await
}

pub(crate) async fn run_session_command_reconciler_observing<S, T>(
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    cancel: CancellationToken,
    mut changes: S,
    mut ticks: T,
    agents: Option<
        tokio::sync::watch::Receiver<Arc<crate::runtime::snapshot::ActiveRuntimeSnapshot>>,
    >,
) -> Result<()>
where
    S: Stream<Item = events::DocumentChangeBatch> + Unpin,
    T: Stream<Item = ()> + Unpin,
{
    let mut collections = RapidHashMap::new();
    for kind in CommandKind::ALL {
        let collection = node
            .get_collection(kind.collection())?
            .context("session command schema missing")?;
        collections.insert(collection.collection_id, kind);
    }
    let mut goal_sources = RapidHashMap::new();
    for source in crate::goal::GOAL_OBSERVATION_SOURCES {
        let collection = node
            .get_collection(source)?
            .context("goal observation schema missing")?;
        goal_sources.insert(collection.collection_id, source);
    }
    let mut subscription_open = true;
    let mut deferred: RapidHashMap<(CommandKind, String, Option<String>), DateTime<Utc>> =
        RapidHashMap::new();
    let mut ids = Vec::new();
    let mut failed = Vec::new();
    let mut full_sweep_owed = false;
    let mut sweep = Some(Sweep::Unverified);
    let mut changed_goals = Vec::new();
    let mut goal_sweep_owed = true;
    loop {
        if cancel.is_cancelled() {
            return Ok(());
        }
        goal_sweep_owed |= matches!(sweep, Some(Sweep::Unverified));
        if let Some(sweep) = sweep {
            let mut sweep_failed = false;
            for kind in CommandKind::ALL {
                if kind == CommandKind::GoalControl {
                    let selected = agents
                        .as_ref()
                        .map(|snapshot| {
                            snapshot.borrow().agents.keys().cloned().collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    for agent in selected {
                        let actor = ::identity::Did::new(&agent)?;
                        match unanswered_goal_commands(&node, identity.as_ref(), &actor, sweep)
                            .await
                        {
                            Ok(unanswered) => ids.extend(
                                unanswered
                                    .into_iter()
                                    .map(|id| (kind, id, Some(agent.clone()))),
                            ),
                            Err(error) => {
                                sweep_failed = true;
                                tracing::warn!(agent_did = %agent, %error, "goal command sweep failed");
                            }
                        }
                    }
                    continue;
                }
                match unanswered_commands(kind, &node, identity.as_ref(), sweep).await {
                    Ok(unanswered) => {
                        ids.extend(unanswered.into_iter().map(|id| (kind, id, None)));
                    }
                    Err(error) => {
                        sweep_failed = true;
                        tracing::warn!(collection = kind.collection(), %error, "session command sweep failed");
                    }
                }
            }
            if matches!(sweep, Sweep::Unverified) {
                full_sweep_owed = sweep_failed;
            }
        }
        ids.sort();
        ids.dedup();
        for (kind, id, actor) in ids.drain(..) {
            if cancel.is_cancelled() {
                return Ok(());
            }
            if kind == CommandKind::GoalControl
                && !agents.as_ref().is_some_and(|snapshot| {
                    actor
                        .as_ref()
                        .is_some_and(|did| snapshot.borrow().agents.contains_key(did))
                })
            {
                deferred.remove(&(kind, id, actor));
                continue;
            }
            let processed = match kind {
                CommandKind::InputEdit => {
                    super::session_input_edit::process_command(&node, &identity, &id).await
                }
                CommandKind::GoalControl => {
                    let selected = actor
                        .as_deref()
                        .context("goal command lacks its selected agent")?;
                    super::session_goal_control::process_command(
                        &node,
                        &identity,
                        &id,
                        &::identity::Did::new(selected)?,
                    )
                    .await
                }
            };
            match processed {
                Ok(Some(issued_at)) => {
                    deferred.insert((kind, id, actor), issued_at);
                }
                Ok(None) => {
                    deferred.remove(&(kind, id.clone(), actor));
                }
                Err(error) => {
                    deferred.remove(&(kind, id.clone(), actor.clone()));
                    tracing::warn!(collection = kind.collection(), command_doc_id = id, %error, "session command could not be processed");
                    failed.push((kind, id, actor));
                }
            }
        }
        if goal_sweep_owed || !changed_goals.is_empty() {
            changed_goals.sort();
            changed_goals.dedup();
            match crate::goal::reconcile_goal_observations(
                &node,
                &identity,
                goal_sweep_owed,
                &changed_goals,
            )
            .await
            {
                Ok(()) => goal_sweep_owed = false,
                Err(error) => {
                    goal_sweep_owed = true;
                    tracing::warn!(%error, "session goal observations could not be reconciled");
                }
            }
            changed_goals.clear();
        }
        let retry = deferred
            .values()
            .min()
            .map(|at| (*at - Utc::now()).to_std().unwrap_or_default());
        sweep = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
            tick = ticks.next() => {
                anyhow::ensure!(tick.is_some(), "session command sweep clock closed");
                goal_sweep_owed = true;
                ids.append(&mut failed);
                Some(if subscription_open && !full_sweep_owed { Sweep::Unanswered } else { Sweep::Unverified })
            }
            _ = tokio::time::sleep(retry.unwrap_or_default()), if retry.is_some() => {
                let now = Utc::now();
                ids.extend(deferred.iter().filter(|(_, at)| **at <= now).map(|(id, _)| id.clone()));
                None
            }
            event = changes.next(), if subscription_open => match event {
                Some(event) => {
                    let mut goal_command_changed = false;
                    for change in event.changes {
                        // Branchable collection-block updates have no document identity.
                        if change.doc_id.is_empty() {
                            continue;
                        }
                        if let Some(kind) = collections.get(&change.collection_id) {
                            if *kind == CommandKind::GoalControl { goal_command_changed = true; }
                            else { ids.push((*kind, change.doc_id.clone(), None)); }
                        }
                        if let Some(source) = goal_sources.get(&change.collection_id) {
                            changed_goals.push((*source, change.doc_id));
                        }
                    }
                    if event.resync_required { Some(Sweep::Unverified) }
                    else if goal_command_changed { Some(Sweep::Unanswered) }
                    else { None }
                }
                None => {
                    subscription_open = false;
                    Some(Sweep::Unverified)
                }
            },
        };
    }
}

#[cfg(test)]
mod notification_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::config_client::ConfigAccess;
    use gents_protocol::goal_control::{
        GoalOperation, GoalRevision, SessionGoalControl, SESSION_GOAL_CONTROL_VERSION,
    };

    #[tokio::test]
    async fn incomplete_intent_does_not_block_the_unverified_command_sweep() {
        let fixture = crate::goal::operator_resume_test_support::Fixture::new(&serde_json::json!({
            "status": "active", "sequence": 0, "blocked_audits": 0,
            "wrapup_requested": false, "wrapup_completed": false,
            "tokens_used": 0, "token_budget": 1000, "last_continued_from": null
        }))
        .await;
        let identity = &fixture.identity;
        let node = fixture.node.clone();
        let actor = ::identity::Did::new(&fixture.parent.agent_did).unwrap();
        let access = ConfigAccess::local_as(
            node.clone(),
            ::identity::Did::new(fixture.user.did()).unwrap(),
        );
        let session = access
            .transact_readonly("test.sweep.session", |txn| {
                Box::pin(async {
                    crate::session::load_agent_session_owner_in_txn(
                        txn,
                        identity.did(),
                        crate::goal::operator_resume_test_support::SESSION,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap();
        let mut command = SessionGoalControl {
            version: SESSION_GOAL_CONTROL_VERSION,
            command_id: "valid-command".into(),
            requester_did: fixture.user.did().into(),
            requester_peer_id: None,
            node_did: identity.did().into(),
            agent_did: fixture.parent.agent_did.clone(),
            agent_doc_id: crate::goal::agent_document_for_test(
                &access,
                identity.did(),
                &fixture.parent.agent_did,
            )
            .await,
            session_id: crate::goal::operator_resume_test_support::SESSION.into(),
            session_doc_id: Some(session.doc_id.clone()),
            issued_at: "2000-01-01T00:00:00Z".into(),
            expires_at: "2100-01-01T00:00:00Z".into(),
            expected: Some(GoalRevision {
                document: "goal-document".into(),
                composite: "goal-commit".into(),
            }),
            operation: GoalOperation::Pause {},
            initial_request: None,
            signature: Vec::new(),
        };
        command.signature = fixture
            .user
            .sign(&command.signing_payload().unwrap())
            .await
            .unwrap();
        let valid = serde_json::to_string(&command).unwrap();
        let mut expected = access.transact_write("test.sparse_goal_commands", |txn| Box::pin(async {
            let mut ids = Vec::new();
            for (id, intent) in [("incomplete-command", None), ("valid-command", Some(valid.as_str()))] {
                let response = txn.execute_with_variables("mutation($input: AgentSessionGoalControlMutationInputArg!) { create_AgentSessionGoalControl(input: $input) { _docID } }",
                    &serde_json::json!({"input": {"command_id": id, "node_did":identity.did(),
                        "requester_did": fixture.user.did(), "agent_did": fixture.parent.agent_did,
                        "session_id": crate::goal::operator_resume_test_support::SESSION, "session_doc_id": session.doc_id, "intent_json":intent}})).await?;
                ids.push(crate::store::graphql::created_doc_id(&response, "AgentSessionGoalControl")?);
            }
            Ok::<_, anyhow::Error>(ids)
        })).await.unwrap();
        let mut actual =
            unanswered_goal_commands(&node, identity.as_ref(), &actor, Sweep::Unverified)
                .await
                .unwrap();
        expected.sort();
        actual.sort();
        assert_eq!(actual, expected);
        node.shutdown().await.expect("node shutdown");
    }
}
