use std::sync::Arc;

use gents_protocol::goal_control::{
    GoalRevision, ObservedGoal, SessionGoalObservation, SESSION_GOAL_CONTROL_VERSION,
};

use super::*;
#[cfg(test)]
mod contract_tests;
use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::session::SessionOwnerRow;
use crate::store::identity::NodeIdentity;

pub(crate) const SOURCES: [&str; 3] = ["Goal", "AgentSession", "GoalObservation"];

pub(crate) async fn reconcile_goal_observations(
    node: &Arc<EmbeddedNode>,
    identity: &Arc<dyn NodeIdentity>,
    full: bool,
    changed: &[(&'static str, String)],
) -> Result<()> {
    let scopes = ConfigAccess::local(node.clone()).transact_readonly("goal.observation_sources", |txn| Box::pin(async move {
        let owner = escape_graphql_string(identity.did());
        let mut sessions = std::collections::BTreeSet::new();
        let mut full = full;
        for (collection, doc_id) in changed {
            anyhow::ensure!(SOURCES.contains(collection), "unknown goal observation source");
            let response = txn.execute(&format!(r#"{{ {collection}(filter: {{
                _docID: {{ _eq: "{}" }}, node_did: {{ _eq: "{owner}" }}
            }}) {{ session_id }} }}"#, escape_graphql_string(doc_id))).await?;
            let rows = response["data"][collection].as_array().context("goal observation source omitted rows")?;
            if rows.is_empty() { full = true; }
            for row in rows {
                if let Some(session) = row["session_id"].as_str().filter(|session| !session.trim().is_empty()) {
                    sessions.insert(session.to_owned());
                } else {
                    full = true;
                    tracing::warn!(collection, "goal observation source has no session identity");
                }
            }
        }
        let mut scopes = Vec::new();
        if full {
            let response = txn.execute(&format!(r#"{{ AgentSession(filter: {{ node_did: {{ _eq: "{owner}" }} }}) {{ session_id requester_did }} }}"#)).await?;
            let rows = response["data"]["AgentSession"].as_array().context("goal session sweep omitted rows")?;
            for row in rows {
                if let Some(session) = row["session_id"].as_str().filter(|session| !session.trim().is_empty()) {
                    scopes.push((session.to_owned(), row["requester_did"].as_str().map(str::to_owned)));
                } else {
                    tracing::warn!("ignoring a session without an identity in goal observation sweep");
                }
            }
        } else {
            for session_id in sessions {
                if let Some(session) = crate::session::load_agent_session_owner_in_txn(txn, identity.did(), &session_id).await? {
                    scopes.push((session.session.session_id, session.session.requester_did));
                }
            }
        }
        scopes.sort();
        scopes.dedup();
        Ok(scopes)
    })).await?;
    let mut failed = false;
    for (session, requester) in scopes {
        if let Err(error) =
            project_session_goal(node, identity, &session, requester.as_deref()).await
        {
            failed = true;
            tracing::warn!(session_id = session, %error, "session goal observation could not be projected");
        }
    }
    anyhow::ensure!(
        !failed,
        "one or more session goal observations remain unresolved"
    );
    Ok(())
}

#[derive(Deserialize)]
struct ObservationRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    node_did: String,
    session_id: String,
    session_doc_id: String,
    requester_did: Option<String>,
    observation_json: Option<String>,
}

async fn rows_in_txn(
    txn: &ConfigApplyTxn<'_>,
    session: &SessionOwnerRow,
) -> Result<Vec<ObservationRow>> {
    let response = txn.execute(&format!(
        r#"{{ GoalObservation(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }}) {{
            _docID node_did session_id session_doc_id requester_did observation_json
        }} }}"#,
        escape_graphql_string(&session.session.node_did),
        escape_graphql_string(&session.session.session_id),
    )).await?;
    let rows: Vec<ObservationRow> = serde_json::from_value(
        response
            .pointer("/data/GoalObservation")
            .cloned()
            .context("goal observation query omitted rows")?,
    )?;
    let rows = rows
        .into_iter()
        .filter(|row| row.session_doc_id == session.doc_id)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        rows.len() <= 1,
        "goal observation has conflicting physical documents"
    );
    Ok(rows)
}

async fn verified(
    identity: &dyn NodeIdentity,
    session: &SessionOwnerRow,
    row: &ObservationRow,
) -> Result<Option<SessionGoalObservation>> {
    if row.node_did != session.session.node_did
        || row.session_id != session.session.session_id
        || row.session_doc_id != session.doc_id
        || row.requester_did != session.session.requester_did
    {
        return Ok(None);
    }
    let Some(observation) = row
        .observation_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<SessionGoalObservation>(raw).ok())
    else {
        return Ok(None);
    };
    if observation
        .validate_scope(
            &session.session.node_did,
            &session.session.session_id,
            &session.doc_id,
            session.session.requester_did.as_deref(),
        )
        .is_err()
    {
        return Ok(None);
    }
    if !identity
        .verify(
            &observation.node_did,
            &observation.signing_payload()?,
            &observation.signature,
        )
        .await
        .unwrap_or(false)
    {
        return Ok(None);
    }
    Ok(Some(observation))
}

/// An absent or unverified projection is unknown. Only a verified envelope
/// containing `goal: None` establishes that the runtime observed no Goal.
pub async fn load_session_goal_observation(
    access: &ConfigAccess,
    verifier: &dyn NodeIdentity,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<SessionGoalObservation>> {
    Ok(
        match load_session_goal_observation_state(
            access,
            verifier,
            node_did,
            session_id,
            requester_did,
        )
        .await?
        {
            SessionGoalObservationState::Observed(observation) => Some(*observation),
            SessionGoalObservationState::NoReadableSession
            | SessionGoalObservationState::Unknown => None,
        },
    )
}

/// A missing projection for an existing readable session must not be treated
/// as a verified absence when capturing signed request intent.
#[derive(Debug)]
pub enum SessionGoalObservationState {
    NoReadableSession,
    Unknown,
    Observed(Box<SessionGoalObservation>),
}

pub fn bind_goal_input(
    input: gents_protocol::request_input::RequestInput,
    state: SessionGoalObservationState,
) -> Result<gents_protocol::request_input::RequestInput> {
    if input.goal_assignment.is_some() {
        return Ok(input);
    }
    let reference = match state {
        SessionGoalObservationState::NoReadableSession => None,
        SessionGoalObservationState::Unknown => {
            anyhow::bail!("Goal state is still syncing. Refresh this chat before sending.")
        }
        SessionGoalObservationState::Observed(observation) => observation.goal.and_then(|goal| {
            goal.assignment_root_request_doc_id.map(|root| {
                gents_protocol::request_input::GoalAssignmentInput {
                    goal_id: goal.snapshot.goal_id,
                    root_request_doc_id: Some(root),
                }
            })
        }),
    };
    Ok(bind_goal_reference(input, reference))
}

fn bind_goal_reference(
    mut input: gents_protocol::request_input::RequestInput,
    reference: Option<gents_protocol::request_input::GoalAssignmentInput>,
) -> gents_protocol::request_input::RequestInput {
    if input.goal_assignment.is_none() {
        input.goal_assignment = reference;
    }
    input
}

pub async fn prepare_goal_input(
    access: &ConfigAccess,
    verifier: &dyn NodeIdentity,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    input: gents_protocol::request_input::RequestInput,
) -> Result<gents_protocol::request_input::RequestInput> {
    if input.goal_assignment.is_some() {
        return Ok(input);
    }
    let state =
        load_session_goal_observation_state(access, verifier, node_did, session_id, requester_did)
            .await?;
    bind_goal_input(input, state)
}

/// Canonical reads require a locally owned destination. Remote destination
/// capture uses its runtime-signed observation through the same transaction.
pub async fn prepare_goal_input_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    agent_id: &str,
    requester_did: Option<&str>,
    input: gents_protocol::request_input::RequestInput,
) -> Result<gents_protocol::request_input::RequestInput> {
    if input.goal_assignment.is_some() {
        return Ok(input);
    }
    let Some(session) =
        crate::session::load_agent_session_row_in_txn(txn, node_did, session_id, requester_did)
            .await?
    else {
        return Ok(input);
    };
    anyhow::ensure!(
        session.session.agent_id == agent_id,
        "Goal input target Agent differs from its canonical session"
    );
    let reference = load_canonical_goal_in_txn(txn, node_did, session_id)
        .await?
        .and_then(|goal| {
            goal.assignment_root_request_doc_id.map(|root| {
                gents_protocol::request_input::GoalAssignmentInput {
                    goal_id: goal.goal_id,
                    root_request_doc_id: Some(root),
                }
            })
        });
    Ok(bind_goal_reference(input, reference))
}

pub async fn load_session_goal_observation_state(
    access: &ConfigAccess,
    verifier: &dyn NodeIdentity,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<SessionGoalObservationState> {
    access
        .transact_readonly("goal.read_observation", |txn| {
            Box::pin(load_session_goal_observation_state_in_txn(
                txn,
                verifier,
                node_did,
                session_id,
                requester_did,
            ))
        })
        .await
}

pub async fn load_session_goal_observation_state_in_txn(
    txn: &ConfigApplyTxn<'_>,
    verifier: &dyn NodeIdentity,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<SessionGoalObservationState> {
    let Some(session) =
        crate::session::load_agent_session_row_in_txn(txn, node_did, session_id, requester_did)
            .await?
    else {
        return Ok(SessionGoalObservationState::NoReadableSession);
    };
    match rows_in_txn(txn, &session).await?.first() {
        Some(row) => Ok(match verified(verifier, &session, row).await? {
            Some(observation) => SessionGoalObservationState::Observed(Box::new(observation)),
            None => SessionGoalObservationState::Unknown,
        }),
        None => Ok(SessionGoalObservationState::Unknown),
    }
}

pub(crate) async fn project_session_goal(
    node: &Arc<EmbeddedNode>,
    identity: &Arc<dyn NodeIdentity>,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<()> {
    ConfigAccess::transact_local_write(node, None, "goal.project_observation", |txn| Box::pin(async move {
        crate::graphql::fence_arrivals(txn, "AgentSession").await?;
        crate::graphql::fence_arrivals(txn, "Goal").await?;
        crate::graphql::fence_arrivals(txn, "GoalObservation").await?;
        let Some(session) = crate::session::load_agent_session_row_in_txn(
            txn, identity.did(), session_id, requester_did,
        ).await? else { return Ok(()); };
        let goal = load_canonical_goal_in_txn(txn, identity.did(), session_id).await?;
        let goal = match goal {
            Some(goal) => {
                let commit = crate::graphql::newest_document_composite_commit_in_txn(
                    txn, &goal.doc_id, "goal.project_observation",
                ).await?.context("goal has no unambiguous composite revision")?;
                Some(ObservedGoal {
                    assignment_root_request_doc_id: goal.assignment_root_request_doc_id.clone(),
                    revision: GoalRevision { document: goal.doc_id.clone(), composite: commit.cid },
                    snapshot: snapshot(&goal, Utc::now()),
                })
            }
            None => None,
        };
        let rows = rows_in_txn(txn, &session).await?;
        if let Some(row) = rows.first() {
            anyhow::ensure!(row.requester_did == session.session.requester_did,
                "goal observation immutable scope differs from canonical session");
            if let Some(previous) = verified(identity.as_ref(), &session, row).await? {
                if previous.goal.as_ref().map(|goal| &goal.revision)
                    == goal.as_ref().map(|goal| &goal.revision)
                    && previous.goal.as_ref().and_then(|goal| goal.assignment_root_request_doc_id.as_ref())
                        == goal.as_ref().and_then(|goal| goal.assignment_root_request_doc_id.as_ref()) {
                    return Ok(());
                }
            }
        }
        let mut observation = SessionGoalObservation {
            version: SESSION_GOAL_CONTROL_VERSION,
            node_did: session.session.node_did.clone(), session_id: session.session.session_id.clone(),
            session_doc_id: session.doc_id.clone(), requester_did: session.session.requester_did.clone(),
            observed_at: Utc::now().to_rfc3339(), goal, signature: Vec::new(),
        };
        observation.signature = identity.sign(&observation.signing_payload()?).await?;
        let input = serde_json::json!({
            "node_did": observation.node_did, "session_id": observation.session_id,
            "session_doc_id": observation.session_doc_id, "requester_did": observation.requester_did,
            "observation_json": serde_json::to_string(&observation)?,
        });
        let (document, variables, field) = match rows.first() {
            Some(row) => (
                "mutation($doc: ID!, $input: GoalObservationMutationInputArg!) { update_GoalObservation(docID: $doc, input: $input) { _docID } }",
                serde_json::json!({"doc": row.doc_id, "input": {"observation_json": input["observation_json"]}}),
                "update_GoalObservation",
            ),
            None => (
                "mutation($input: GoalObservationMutationInputArg!) { create_GoalObservation(input: $input) { _docID } }",
                serde_json::json!({"input": input}),
                "create_GoalObservation",
            ),
        };
        let response = txn.execute_with_variables(document, &variables).await?;
        let response = defra_node::QueryResponse::success(response["data"].clone());
        crate::graphql::single_mutation_document(&response, field)?
            .context("goal observation write omitted its document")?;
        Ok(())
    })).await
}
