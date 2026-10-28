use rapidhash::{HashMapExt, RapidHashMap};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use futures::{Stream, StreamExt};
use gents_protocol::session_input_edit::SessionInputEdit;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::graphql::escape_graphql_string;
use crate::identity::NodeIdentity;

#[derive(Deserialize)]
pub(super) struct CommandRow {
    #[serde(rename = "_docID")]
    pub(super) doc_id: String,
    pub(super) command_id: String,
    pub(super) requester_did: String,
    pub(super) requester_peer_id: Option<String>,
    pub(super) node_did: String,
    pub(super) session_id: String,
    pub(super) intent_json: String,
    pub(super) receipt_json: Option<String>,
}

pub(super) const FIELDS: &str = "_docID command_id requester_did requester_peer_id node_did session_id intent_json receipt_json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum CommandKind {
    InputEdit,
    GoalControl,
    ProcessStop,
}

impl CommandKind {
    const ALL: [Self; 3] = [Self::InputEdit, Self::GoalControl, Self::ProcessStop];
    fn collection(self) -> &'static str {
        match self {
            Self::InputEdit => "AgentSessionInputEdit",
            Self::GoalControl => "AgentSessionGoalControl",
            Self::ProcessStop => "AgentSessionProcessStop",
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Sweep {
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
    let response = crate::graphql::graphql_with_transaction_retry(
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
        return Ok(crate::graphql::rows::<DocIdRow>(&response, collection)?
            .into_iter()
            .map(|row| row.doc_id)
            .collect());
    }

    #[derive(Deserialize)]
    struct Row {
        #[serde(rename = "_docID")]
        doc_id: String,
        intent_json: Option<String>,
        receipt_json: Option<String>,
    }
    let mut unanswered = Vec::new();
    for row in crate::graphql::rows::<Row>(&response, collection)? {
        let Some(intent) = row.intent_json.as_deref() else {
            if row.receipt_json.is_none() {
                unanswered.push(row.doc_id);
            }
            continue;
        };
        let answered = if kind == CommandKind::ProcessStop {
            super::session_process_stop::verified_receipt(
                identity,
                intent,
                row.receipt_json.as_deref(),
            )
            .await?
            .is_some_and(|receipt| !receipt.outcome.needs_execution())
        } else if kind == CommandKind::GoalControl {
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

#[cfg(test)]
pub async fn run_session_command_reconciler(
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    cancel: CancellationToken,
) -> Result<()> {
    run_session_command_reconciler_with_registry(
        node,
        identity,
        crate::hook::BackgroundExecutionRegistry::default(),
        cancel,
    )
    .await
}

pub(crate) async fn run_session_command_reconciler_with_registry(
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    executions: crate::hook::BackgroundExecutionRegistry,
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
    run_session_command_reconciler_observing_with_registry(
        node, identity, executions, cancel, changes, ticks,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn run_session_command_reconciler_observing<S, T>(
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    cancel: CancellationToken,
    changes: S,
    ticks: T,
) -> Result<()>
where
    S: Stream<Item = events::DocumentChangeBatch> + Unpin,
    T: Stream<Item = ()> + Unpin,
{
    run_session_command_reconciler_observing_with_registry(
        node,
        identity,
        crate::hook::BackgroundExecutionRegistry::default(),
        cancel,
        changes,
        ticks,
    )
    .await
}

async fn run_session_command_reconciler_observing_with_registry<S, T>(
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    executions: crate::hook::BackgroundExecutionRegistry,
    cancel: CancellationToken,
    mut changes: S,
    mut ticks: T,
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
    let mut deferred: RapidHashMap<(CommandKind, String), DateTime<Utc>> = RapidHashMap::new();
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
                match unanswered_commands(kind, &node, identity.as_ref(), sweep).await {
                    Ok(unanswered) => {
                        ids.extend(unanswered.into_iter().map(|id| (kind, id)));
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
        for (kind, id) in ids.drain(..) {
            if cancel.is_cancelled() {
                return Ok(());
            }
            let processed = match kind {
                CommandKind::InputEdit => {
                    super::session_input_edit::process_command(&node, &identity, &id).await
                }
                CommandKind::GoalControl => {
                    super::session_goal_control::process_command(&node, &identity, &id).await
                }
                CommandKind::ProcessStop => {
                    super::session_process_stop::process_command(&node, &identity, &executions, &id)
                        .await
                }
            };
            match processed {
                Ok(Some(issued_at)) => {
                    deferred.insert((kind, id), issued_at);
                }
                Ok(None) => {
                    deferred.remove(&(kind, id.clone()));
                }
                Err(error) => {
                    deferred.remove(&(kind, id.clone()));
                    tracing::warn!(collection = kind.collection(), command_doc_id = id, %error, "session command could not be processed");
                    failed.push((kind, id));
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
                    for change in event.changes {
                        // Branchable collection-block updates have no document identity.
                        if change.doc_id.is_empty() {
                            continue;
                        }
                        if let Some(kind) = collections.get(&change.collection_id) {
                            ids.push((*kind, change.doc_id.clone()));
                        }
                        if let Some(source) = goal_sources.get(&change.collection_id) {
                            changed_goals.push((*source, change.doc_id));
                        }
                    }
                    event.resync_required.then_some(Sweep::Unverified)
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
    use crate::config_client::ConfigAccess;
    use crate::identity::KeyIdentity;
    use gents_protocol::goal_control::{
        GoalOperation, GoalRevision, SessionGoalControl, SESSION_GOAL_CONTROL_VERSION,
    };

    #[tokio::test]
    async fn incomplete_intent_does_not_block_the_unverified_command_sweep() {
        let temp = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(temp.path().join("node.key"), None).unwrap();
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let mut command = SessionGoalControl {
            version: SESSION_GOAL_CONTROL_VERSION,
            command_id: "valid-command".into(),
            requester_did: identity.did().into(),
            requester_peer_id: None,
            node_did: identity.did().into(),
            session_id: "session".into(),
            session_doc_id: Some("session-document".into()),
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
        command.signature = identity
            .sign(&command.signing_payload().unwrap())
            .await
            .unwrap();
        let valid = serde_json::to_string(&command).unwrap();
        let mut expected = ConfigAccess::local(node.clone()).transact_write("test.sparse_goal_commands", |txn| Box::pin(async {
            let mut ids = Vec::new();
            for (id, intent) in [("incomplete-command", None), ("valid-command", Some(valid.as_str()))] {
                let response = txn.execute_with_variables("mutation($input: AgentSessionGoalControlMutationInputArg!) { create_AgentSessionGoalControl(input: $input) { _docID } }",
                    &serde_json::json!({"input": {"command_id": id, "node_did":identity.did(),
                        "requester_did": identity.did(), "session_id":"session", "intent_json":intent}})).await?;
                ids.push(crate::graphql::created_doc_id(&response, "AgentSessionGoalControl")?);
            }
            Ok::<_, anyhow::Error>(ids)
        })).await.unwrap();
        let mut actual = unanswered_commands(
            CommandKind::GoalControl,
            &node,
            &identity,
            Sweep::Unverified,
        )
        .await
        .unwrap();
        expected.sort();
        actual.sort();
        assert_eq!(actual, expected);
        node.shutdown().await;
    }
}
