use std::sync::Arc;

use gents_protocol::goal_control::{
    GoalRevision, ObservedGoal, SessionGoalObservation, SESSION_GOAL_CONTROL_VERSION,
};

use super::*;
#[cfg(test)]
mod contract_tests;
use crate::store::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::store::config_client::agent_session::AgentSessionRowRead;
use crate::store::identity::NodeIdentity;
use gents_protocol::session::SessionOwnerRow;

pub(crate) const SOURCES: [&str; 3] = ["Goal", "AgentSession", "GoalObservation"];

pub(crate) async fn reconcile_goal_observations_with_identities(
    node: &Arc<EmbeddedNode>,
    identity: &Arc<dyn NodeIdentity>,
    agent_identities: &[Arc<dyn NodeIdentity>],
    full: bool,
    changed: &[(&'static str, String)],
) -> Result<()> {
    anyhow::ensure!(
        node.node_identity_did() == Some(identity.did()),
        "goal observation runtime identity differs from node"
    );
    let mut failed = false;
    for agent in agent_identities {
        let access = ConfigAccess::local_managed_agent(
            node.clone(),
            identity::Did::new(agent.did())?,
            agent.as_ref(),
        )?;
        let scopes = access.transact_readonly("goal.observation_sources", |txn| Box::pin(async move {
            let owner = escape_graphql_string(identity.did());
            let agent_did = escape_graphql_string(agent.did());
            let mut sessions = std::collections::BTreeSet::new();
            let mut full = full;
            for (collection, doc_id) in changed {
                anyhow::ensure!(SOURCES.contains(collection), "unknown goal observation source");
                let response = txn.execute(&format!(r#"{{ {collection}(filter: {{ _docID: {{ _eq: "{}" }}, node_did: {{ _eq: "{owner}" }}, agent_did: {{ _eq: "{agent_did}" }} }}) {{ session_id }} }}"#, escape_graphql_string(doc_id))).await?;
                let rows = response["data"][collection].as_array().context("goal observation source omitted rows")?;
                if rows.is_empty() { full = true; }
                for row in rows {
                    if let Some(session) = row["session_id"].as_str().filter(|session| !session.trim().is_empty()) {
                        sessions.insert(session.to_owned());
                    } else { full = true; }
                }
            }
            let mut scopes = Vec::new();
            if full {
                let response = txn.execute(&format!(r#"{{ AgentSession(filter: {{ node_did: {{ _eq: "{owner}" }}, agent_did: {{ _eq: "{agent_did}" }} }}) {{ session_id requester_did }} }}"#)).await?;
                let rows = response["data"]["AgentSession"].as_array().context("goal session sweep omitted rows")?;
                for row in rows {
                    if let Some(session) = row["session_id"].as_str().filter(|session| !session.trim().is_empty()) {
                        scopes.push((session.to_owned(), row["requester_did"].as_str().map(str::to_owned)));
                    }
                }
            } else {
                for session_id in sessions {
                    if let Some(session) = crate::store::config_client::agent_session::load_agent_session_owner_in_txn(txn, identity.did(), &session_id).await? {
                        if session.session.agent_did == agent.did() {
                            scopes.push((session.session.session_id, session.session.requester_did));
                        }
                    }
                }
            }
            scopes.sort(); scopes.dedup();
            Ok::<_, anyhow::Error>(scopes)
        })).await?;
        for (session, requester) in scopes {
            if let Err(error) =
                project_session_goal(node, identity, agent, &session, requester.as_deref()).await
            {
                failed = true;
                tracing::warn!(session_id = session, agent_did = agent.did(), %error, "session goal observation could not be projected");
            }
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
            &session.session.agent_did,
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
            &session.session.agent_did,
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

/// Collapses [`SessionGoalObservationState`] to the verified envelope, for
/// runtime callers that act only on one. Client views and sends use the
/// three-state read: `None` here does not mean the session has no Goal.
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
            | SessionGoalObservationState::Unknown(_) => None,
        },
    )
}

/// Why a readable session's Goal state is not known (Lean
/// `SessionObservation.UnknownCause`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SessionGoalUnknownCause {
    ReadIncomplete,
    ProjectionMissing,
    ProjectionUnverified,
}

/// One read transaction's Goal state (Lean `SessionObservation.State`). A
/// missing projection for an existing readable session is never a verified
/// absence when capturing signed request intent.
#[derive(Debug)]
pub enum SessionGoalObservationState {
    NoReadableSession,
    Unknown(SessionGoalUnknownCause),
    Observed(Box<SessionGoalObservation>),
}

/// The projection row read with its session (Lean
/// `SessionObservation.ProjectionRead`).
#[derive(Debug)]
pub enum SessionGoalProjectionRead {
    Missing,
    Unverified,
    Verified(Box<SessionGoalObservation>),
}

/// Lean `SessionObservation.observe`. The projection is consulted only for a
/// present session row.
pub fn observe_session_goal(
    session: &AgentSessionRowRead,
    projection: SessionGoalProjectionRead,
) -> SessionGoalObservationState {
    match (session, projection) {
        (AgentSessionRowRead::Omitted, _) => {
            SessionGoalObservationState::Unknown(SessionGoalUnknownCause::ReadIncomplete)
        }
        (AgentSessionRowRead::Absent, _) => SessionGoalObservationState::NoReadableSession,
        (AgentSessionRowRead::Present(_), SessionGoalProjectionRead::Missing) => {
            SessionGoalObservationState::Unknown(SessionGoalUnknownCause::ProjectionMissing)
        }
        (AgentSessionRowRead::Present(_), SessionGoalProjectionRead::Unverified) => {
            SessionGoalObservationState::Unknown(SessionGoalUnknownCause::ProjectionUnverified)
        }
        (AgentSessionRowRead::Present(_), SessionGoalProjectionRead::Verified(observation)) => {
            SessionGoalObservationState::Observed(observation)
        }
    }
}

/// Lean `SessionObservation.bind`.
pub fn bind_goal_input(
    input: gents_protocol::request_input::RequestInput,
    state: SessionGoalObservationState,
) -> Result<gents_protocol::request_input::RequestInput> {
    if input.goal_assignment.is_some() {
        return Ok(input);
    }
    let reference = match state {
        SessionGoalObservationState::NoReadableSession => None,
        SessionGoalObservationState::Unknown(_) => {
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

/// The client's send preparation: one read transaction scopes the
/// observation to the session row's requester provenance (or `actor_did` when
/// no row is readable) and binds the input from it.
pub async fn prepare_client_goal_input(
    access: &ConfigAccess,
    verifier: &dyn NodeIdentity,
    node_did: &str,
    session_id: &str,
    actor_did: &str,
    input: gents_protocol::request_input::RequestInput,
) -> Result<gents_protocol::request_input::RequestInput> {
    if input.goal_assignment.is_some() {
        return Ok(input);
    }
    let state =
        load_client_session_goal_observation_state(access, verifier, node_did, session_id, actor_did)
            .await?;
    bind_goal_input(input, state)
}

/// The client's read of one session's Goal state in one transaction. An
/// existing row's requester provenance scopes its projection; a client never
/// reads it under another requester.
pub async fn load_client_session_goal_observation_state(
    access: &ConfigAccess,
    verifier: &dyn NodeIdentity,
    node_did: &str,
    session_id: &str,
    actor_did: &str,
) -> Result<SessionGoalObservationState> {
    anyhow::ensure!(!actor_did.trim().is_empty(), "Goal observation requires its client actor");
    access
        .transact_readonly("goal.read_client_observation", |txn| {
            Box::pin(async move {
                let session = crate::store::config_client::agent_session::read_agent_session_owner_in_txn(
                    txn, node_did, session_id,
                )
                .await?;
                if let AgentSessionRowRead::Present(row) = &session {
                    anyhow::ensure!(
                        row.session.requester_did.is_some(),
                        "existing session has no requester provenance"
                    );
                }
                observe_session_row_in_txn(txn, verifier, session).await
            })
        })
        .await
}

/// Canonical reads require a locally owned destination. Remote destination
/// capture uses its agent-signed observation through the same transaction.
pub async fn prepare_goal_input_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    input: gents_protocol::request_input::RequestInput,
) -> Result<gents_protocol::request_input::RequestInput> {
    if input.goal_assignment.is_some() {
        return Ok(input);
    }
    let Some(session) = crate::store::config_client::agent_session::load_agent_session_row_in_txn(
        txn,
        node_did,
        session_id,
        requester_did,
    )
    .await?
    else {
        return Ok(input);
    };
    anyhow::ensure!(
        session.session.agent_did == agent_did,
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
    let session = match crate::store::config_client::agent_session::read_agent_session_owner_in_txn(
        txn, node_did, session_id,
    )
    .await?
    {
        AgentSessionRowRead::Present(row)
            if row.session.requester_did.as_deref() != requester_did =>
        {
            AgentSessionRowRead::Absent
        }
        read => read,
    };
    observe_session_row_in_txn(txn, verifier, session).await
}

async fn observe_session_row_in_txn(
    txn: &ConfigApplyTxn<'_>,
    verifier: &dyn NodeIdentity,
    session: AgentSessionRowRead,
) -> Result<SessionGoalObservationState> {
    let projection = match &session {
        AgentSessionRowRead::Present(row) => match rows_in_txn(txn, row).await?.first() {
            Some(projection) => match verified(verifier, row, projection).await? {
                Some(observation) => SessionGoalProjectionRead::Verified(Box::new(observation)),
                None => SessionGoalProjectionRead::Unverified,
            },
            None => SessionGoalProjectionRead::Missing,
        },
        AgentSessionRowRead::Absent | AgentSessionRowRead::Omitted => {
            SessionGoalProjectionRead::Missing
        }
    };
    Ok(observe_session_goal(&session, projection))
}

/// Publishes `session_id`'s Goal projection signed by `signer`, its selected
/// Agent. The runtime's reconciler is the production caller; tests on a served
/// runtime call it to publish the projection a client then reads.
pub async fn project_session_goal(
    node: &Arc<EmbeddedNode>,
    identity: &Arc<dyn NodeIdentity>,
    signer: &Arc<dyn NodeIdentity>,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(
        node.node_identity_did() == Some(identity.did()),
        "goal observation runtime identity differs from node"
    );
    let access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(signer.did())?,
        signer.as_ref(),
    )?;
    access.transact_write("goal.project_observation", |txn| {
        let signer = &signer;
        Box::pin(async move {
        crate::store::graphql::fence_arrivals(txn, "AgentSession").await?;
        crate::store::graphql::fence_arrivals(txn, "Goal").await?;
        crate::store::graphql::fence_arrivals(txn, "GoalObservation").await?;
        let Some(session) = crate::store::config_client::agent_session::load_agent_session_row_in_txn(
            txn, identity.did(), session_id, requester_did,
        ).await? else { return Ok(()); };
        anyhow::ensure!(session.session.agent_did == signer.did(), "goal observation signer differs from session Agent");
        let goal = load_canonical_goal_in_txn(txn, identity.did(), session_id).await?;
        let goal = match goal {
            Some(goal) => {
                let commit = crate::store::graphql::newest_document_composite_commit_in_txn(
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
            node_did: session.session.node_did.clone(), agent_did: session.session.agent_did.clone(), session_id: session.session.session_id.clone(),
            session_doc_id: session.doc_id.clone(), requester_did: session.session.requester_did.clone(),
            observed_at: Utc::now().to_rfc3339(), goal, signature: Vec::new(),
        };
        observation.signature = signer.sign(&observation.signing_payload()?).await?;
        let input = serde_json::json!({
            "node_did": observation.node_did, "session_id": observation.session_id,
            "session_doc_id": observation.session_doc_id, "requester_did": observation.requester_did,
            "agent_did": observation.agent_did,
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
        let written = crate::store::graphql::single_mutation_document(&response, field)?
            .context("goal observation write omitted its document")?;
        if rows.is_empty() {
            let doc_id = written["_docID"].as_str().context("goal observation lacks document identity")?;
            txn.add_resource_relationship("GoalObservation", doc_id, "parent", "sessions", &session.doc_id).await?;
        }
        Ok(())
    })}).await
}
