//! Publish an already claimed continuation through the existing Goal transaction.
mod wait_observation;

use super::*;
use crate::config_client::ConfigApplyTxn;
use crate::identity::{AgentIdentity, RegisteredIdentity};
use crate::lifecycle::materialize::{sign_request, RequestSigner};
use crate::lifecycle::queue::{
    goal_continuation_behavior, goal_continuation_identity, prepare_goal_continuation,
};
use crate::request_admission::{verify_runtime_local_control_receipt, SIGNED_REQUEST_FIELDS};
use gents_protocol::row::AgentRequestRow;

pub(crate) async fn publish_claimed_continuation(
    node: &EmbeddedNode,
    observed: &GoalDocument,
    parent_request_id: &str,
    content: &str,
    wrapup: bool,
) -> Result<Option<GoalResumeReceipt>> {
    let identity = RegisteredIdentity::from_registered_did(&observed.agent_did, None)?;
    let identity = &identity;
    let stopped = std::sync::Mutex::new(None);
    let stopped_ref = &stopped;
    // Preserve the automatic queue writer's node actor; target identity signs
    // the child independently of the database actor.
    let receipt = crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "goal.publish_claimed_continuation",
        move |txn| {
            Box::pin(async move {
                stage_claimed_continuation(
                    txn,
                    identity,
                    observed,
                    parent_request_id,
                    content,
                    wrapup,
                    stopped_ref,
                )
                .await
            })
        },
    )
    .await?;
    if let Some(reason) = stopped
        .into_inner()
        .unwrap_or_else(|poison| poison.into_inner())
    {
        tracing::warn!(goal_id = %observed.goal_id, %parent_request_id, %reason,
            "stopped Goal automation on invalid wait evidence");
    }
    Ok(receipt)
}

/// Stop only the observed claim while its parent is still the idle session
/// head. Publication writes the same Goal row, so a conflicting child commit
/// retries this transaction and must pass these observations again.
pub(crate) async fn stop_claimed_continuation_for_unavailable_behavior(
    node: &EmbeddedNode,
    observed: &GoalDocument,
    parent_request_id: &str,
    reason: &str,
) -> Result<bool> {
    crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "goal.stop_unavailable_claimed_continuation",
        |txn| Box::pin(async move {
            let Some(goal) = load_canonical_goal_in_txn(txn, &observed.agent_did, &observed.session_id).await? else {
                return Ok(false);
            };
            if goal.doc_id != observed.doc_id || goal.status != observed.status
                || goal.continuation_sequence() != observed.continuation_sequence()
                || goal.last_continued_from_request_id != observed.last_continued_from_request_id
                || goal.last_continued_from_request_id.as_deref() != Some(parent_request_id)
                || gate_claimed_goal_continuation(
                    GoalBehaviorObservation::Unavailable, true, false,
                    &goal.state().context("goal has an unknown status")?,
                ) != GoalClaimedDecision::Stop
            {
                return Ok(false);
            }
            let did = escape_graphql_string(&goal.agent_did);
            let session = escape_graphql_string(&goal.session_id);
            let response = txn.execute(&format!(r#"{{ AgentRequest(filter: {{
                agent_did: {{ _eq: "{did}" }}, session_id: {{ _eq: "{session}" }}
            }}, order: [{{ created_at: DESC }}, {{ request_id: DESC }}]) {{ {SIGNED_REQUEST_FIELDS} }} }}"#)).await?;
            let requests: Vec<AgentRequestRow> = serde_json::from_value(
                response.pointer("/data/AgentRequest").cloned()
                    .context("claimed stop request query omitted rows")?
            )?;
            if !goal_session_is_idle(&requests)
                || !latest_goal_request(&goal, &requests).is_some_and(|parent|
                    parent.request_id == parent_request_id
                        && parent.lifecycle_state.is_some_and(RequestLifecycleState::is_terminal))
                || requests.iter().any(|request|
                    request.caused_by_trigger_kind.as_deref() == Some(GOAL_TRIGGER_KIND)
                        && request.caused_by_trigger_id.as_deref() == Some(goal.goal_id.as_str())
                        && request.caused_by_parent_request_id.as_deref() == Some(parent_request_id))
            {
                return Ok(false);
            }
            Ok(stop_claimed_continuation_in_txn(
                txn, &goal, observed.continuation_sequence(), parent_request_id,
                reason, false, Utc::now(),
            ).await?.is_some())
        }),
    ).await
}

/// The invalid-evidence stop staged by the attempt that committed, if any.
type StoppedReason = std::sync::Mutex<Option<String>>;

async fn stage_claimed_continuation(
    txn: &ConfigApplyTxn<'_>,
    identity: &dyn AgentIdentity,
    observed: &GoalDocument,
    parent_request_id: &str,
    content: &str,
    wrapup: bool,
    stopped: &StoppedReason,
) -> Result<Option<GoalResumeReceipt>> {
    // A conflicted attempt's stop never committed.
    *stopped.lock().unwrap_or_else(|poison| poison.into_inner()) = None;
    anyhow::ensure!(
        identity.did() == observed.agent_did,
        "claimed publication requires the goal owner's signing identity"
    );
    let Some(goal) =
        load_canonical_goal_in_txn(txn, &observed.agent_did, &observed.session_id).await?
    else {
        return Ok(None);
    };
    if goal.doc_id != observed.doc_id {
        return Ok(None);
    }
    let did = escape_graphql_string(&goal.agent_did);
    let session = escape_graphql_string(&goal.session_id);
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{
        agent_did: {{ _eq: "{did}" }}, session_id: {{ _eq: "{session}" }}
    }}, order: [{{ created_at: DESC }}, {{ request_id: DESC }}]) {{ {SIGNED_REQUEST_FIELDS} }} }}"#
        ))
        .await?;
    let requests: Vec<AgentRequestRow> = serde_json::from_value(
        response
            .pointer("/data/AgentRequest")
            .cloned()
            .context("claimed parent query omitted rows")?,
    )?;
    let parents: Vec<_> = requests
        .iter()
        .filter(|row| row.request_id == parent_request_id)
        .collect();
    anyhow::ensure!(
        parents.len() == 1,
        "claimed predecessor must uniquely belong to the goal owner and session"
    );
    let parent_row = parents[0];
    let parent = crate::watcher::AgentRequest::try_from(parent_row.clone())?;
    let behavior = goal_continuation_behavior(txn, &parent).await?;
    let sequence = observed.continuation_sequence();
    let now = Utc::now();
    let created_at = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let key = goal_continuation_identity(&goal.goal_id, parent_request_id, sequence)?.retry_key;
    let key = escape_graphql_string(&key);
    let response = txn.execute(&format!(r#"{{ AgentRequest(filter: {{ retry_key: {{ _eq: "{key}" }} }}) {{ {SIGNED_REQUEST_FIELDS} }} }}"#)).await?;
    let children: Vec<AgentRequestRow> = serde_json::from_value(
        response
            .pointer("/data/AgentRequest")
            .cloned()
            .context("claimed receipt query omitted rows")?,
    )?;
    anyhow::ensure!(
        children.len() <= 1,
        "ambiguous claimed continuation receipt"
    );
    // A first publication writes the session's current hop. A replay takes the
    // published continuation's own signed hop: the session has moved on since,
    // and its rows cannot reproduce the set the first publication read.
    let session_hop = match children.first() {
        Some(child) => child
            .subagent_depth
            .and_then(|hop| u32::try_from(hop).ok())
            .context("claimed continuation receipt lacks its hop")?,
        None => {
            crate::session::load_session_current_hop_in_txn(txn, &goal.agent_did, &goal.session_id)
                .await?
        }
    };
    let mut create = prepare_goal_continuation(
        &parent,
        behavior,
        &goal.goal_id,
        content,
        sequence,
        wrapup,
        &created_at,
        session_hop,
    )?;
    let expected = GoalBackedRequestFingerprint::from_create(&create)?;

    if let Some(child) = children.first() {
        verify_runtime_local_control_receipt(
            child,
            &goal.agent_did,
            parent_request_id,
            parent.requester_did.as_deref().unwrap_or(&goal.agent_did),
        )?;
        let actual: GoalBackedRequestFingerprint =
            serde_json::from_value(serde_json::to_value(child)?)?;
        anyhow::ensure!(
            actual == expected,
            "claimed continuation receipt conflicts with the observed publication"
        );
        return Ok(Some(GoalResumeReceipt {
            goal_status: goal.parsed_status().context("goal has an unknown status")?,
            goal_id: goal.goal_id,
            request_id: child.request_id.clone(),
            doc_id: child
                .doc_id
                .clone()
                .context("claimed receipt has no document ID")?,
            created: false,
        }));
    }
    if goal.status != observed.status
        || goal.continuation_sequence() != sequence
        || goal.last_continued_from_request_id != observed.last_continued_from_request_id
    {
        return Ok(None);
    }
    if !matches!(
        goal.parsed_status(),
        Some(GoalStatus::Active | GoalStatus::BudgetLimited)
    ) || goal.wrapup_completed.unwrap_or(false)
        || goal.last_continued_from_request_id.as_deref() != Some(parent_request_id)
        || !parent_row
            .lifecycle_state
            .is_some_and(RequestLifecycleState::is_terminal)
        || !goal_session_is_idle(&requests)
        || !latest_goal_request(&goal, &requests).is_some_and(|row| row.doc_id == parent_row.doc_id)
    {
        return Ok(None);
    }
    match wait_observation::observe_intentional_wait(
        txn,
        parent_row
            .doc_id
            .as_deref()
            .context("goal predecessor lacks physical document identity")?,
        &goal.agent_did,
        &goal.session_id,
        parent_row.requester_did.as_deref(),
    )
    .await
    {
        wait_observation::WaitEvidence::Absent => {}
        wait_observation::WaitEvidence::Running => return Ok(None),
        // No Goal write: the claim stays and the next reconciliation retries.
        wait_observation::WaitEvidence::Unavailable(error) => return Err(error),
        wait_observation::WaitEvidence::Invalid(error) => {
            *stopped.lock().unwrap_or_else(|poison| poison.into_inner()) =
                stop_claimed_continuation_in_txn(
                    txn,
                    &goal,
                    sequence,
                    parent_request_id,
                    &format!("invalid Goal wait evidence: {error:#}"),
                    true,
                    now,
                )
                .await?;
            return Ok(None);
        }
    }
    sign_request(&mut create, RequestSigner::Identity(identity)).await?;
    if let Some(binding) = crate::graph_pipeline::graph_binding_for_request_in_txn(
        txn,
        parent_row
            .doc_id
            .as_deref()
            .context("goal predecessor lacks document ID")?,
    )
    .await?
    {
        crate::graph_pipeline::fence_graph_publication_in_txn(
            txn,
            &binding.run_id,
            &binding.revision_digest,
        )
        .await?;
    }

    let doc_id = escape_graphql_string(&goal.doc_id);
    let status = escape_graphql_string(&goal.status);
    let parent_id = escape_graphql_string(parent_request_id);
    let timestamp = escape_graphql_string(&now.to_rfc3339());
    // Updating the timestamp makes this a real write on the same Goal row as
    // pause/completion/resume, so competing transactions cannot both publish.
    let response = txn
        .execute(&format!(
            r#"mutation {{ update_Goal(filter: {{
        _docID: {{ _eq: "{doc_id}" }}, agent_did: {{ _eq: "{did}" }},
        status: {{ _eq: "{status}" }}, continuation_sequence: {{ _eq: {sequence} }},
        last_continued_from_request_id: {{ _eq: "{parent_id}" }}
    }}, input: {{ updated_at: "{timestamp}" }}) {{ _docID }} }}"#
        ))
        .await?;
    if !response
        .pointer("/data/update_Goal")
        .is_some_and(mutation_returned_rows)
    {
        return Ok(None);
    }
    let response = txn
        .execute(&create.graphql_mutation().map_err(anyhow::Error::msg)?)
        .await?;
    let child = response
        .pointer("/data/create_AgentRequest")
        .or_else(|| response.pointer("/data/add_AgentRequest"))
        .context("claimed child create omitted result")?;
    let child_doc = child
        .get("_docID")
        .or_else(|| child.get(0).and_then(|row| row.get("_docID")))
        .and_then(serde_json::Value::as_str)
        .context("claimed child create omitted document ID")?;
    Ok(Some(GoalResumeReceipt {
        goal_status: goal.parsed_status().context("goal has an unknown status")?,
        goal_id: goal.goal_id,
        request_id: create.request_id,
        doc_id: child_doc.to_owned(),
        created: true,
    }))
}

/// Uses the existing Goal pause/abandon transitions under the publication
/// claim guard. Readiness stops preserve retry provenance; invalid wait
/// evidence records its failure because that claim cannot be recovered.
async fn stop_claimed_continuation_in_txn(
    txn: &ConfigApplyTxn<'_>,
    goal: &GoalDocument,
    sequence: i64,
    parent_request_id: &str,
    reason: &str,
    record_failure: bool,
    now: DateTime<Utc>,
) -> Result<Option<String>> {
    let state = goal.state().context("goal has an unknown status")?;
    let failure_field = if record_failure {
        format!(r#", last_failure: "{}""#, escape_graphql_string(reason))
    } else {
        String::new()
    };
    let updated_at = escape_graphql_string(&now.to_rfc3339());
    let fields = if let Some(post) = state.step(GoalAction::Pause) {
        format!(
            r#"status: "{}", active_time_seconds: {}, active_started_at: null{failure_field}, updated_at: "{updated_at}""#,
            post.status.as_str(),
            goal.current_active_time_seconds(now),
        )
    } else if state.step(GoalAction::WrapupAbandoned).is_some() {
        format!(r#"wrapup_completed: true{failure_field}, updated_at: "{updated_at}""#)
    } else {
        return Ok(None);
    };
    let doc_id = escape_graphql_string(&goal.doc_id);
    let did = escape_graphql_string(&goal.agent_did);
    let status = escape_graphql_string(&goal.status);
    let parent_id = escape_graphql_string(parent_request_id);
    let response = txn
        .execute(&format!(
            r#"mutation {{ update_Goal(filter: {{
        _docID: {{ _eq: "{doc_id}" }}, agent_did: {{ _eq: "{did}" }},
        status: {{ _eq: "{status}" }}, continuation_sequence: {{ _eq: {sequence} }},
        last_continued_from_request_id: {{ _eq: "{parent_id}" }}
    }}, input: {{ {fields} }}) {{ _docID }} }}"#
        ))
        .await?;
    Ok(response
        .pointer("/data/update_Goal")
        .is_some_and(mutation_returned_rows)
        .then(|| reason.to_owned()))
}

#[cfg(test)]
mod contract_tests;
