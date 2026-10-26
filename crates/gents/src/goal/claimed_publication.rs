pub(super) mod wait_observation;

use super::*;
use crate::config_client::ConfigApplyTxn;
use crate::identity::{NodeIdentity, RegisteredIdentity};
use crate::lifecycle::materialize::{sign_request, RequestSigner};
use crate::lifecycle::queue::{
    goal_continuation_agent, goal_continuation_identity, prepare_goal_continuation,
};
use crate::request_admission::{verify_runtime_local_control_receipt, SIGNED_REQUEST_FIELDS};
use gents_protocol::error_chain::ErrorChain;
use gents_protocol::row::AgentRequestRow;

pub(crate) async fn publish_claimed_continuation(
    node: &std::sync::Arc<EmbeddedNode>,
    observed: &GoalDocument,
    parent_request_id: &str,
    content: &str,
    wrapup: bool,
) -> Result<Option<GoalResumeReceipt>> {
    let identity = RegisteredIdentity::from_registered_did(&observed.node_did, None)?;
    let identity = &identity;
    let stopped = std::sync::Mutex::new(None);
    let stopped_ref = &stopped;
    // Preserve the automatic queue writer's node actor; target identity signs
    let receipt = crate::config_client::ConfigAccess::transact_local_write(
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

pub(crate) async fn stop_claimed_continuation_for_unavailable_agent(
    node: &std::sync::Arc<EmbeddedNode>,
    observed: &GoalDocument,
    parent_request_id: &str,
    reason: &str,
) -> Result<bool> {
    crate::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "goal.stop_unavailable_claimed_continuation",
        |txn| Box::pin(async move {
            let Some(goal) = load_canonical_goal_in_txn(txn, &observed.node_did, &observed.session_id).await? else {
                return Ok(false);
            };
            if goal.doc_id != observed.doc_id || goal.status != observed.status
                || goal.continuation_sequence() != observed.continuation_sequence()
                || goal.last_continued_from_request_id != observed.last_continued_from_request_id
                || goal.last_continued_from_request_id.as_deref() != Some(parent_request_id)
                || gate_claimed_goal_continuation(
                    GoalAgentObservation::Unavailable, true, false,
                    &goal.state().ok_or(GoalError::GoalHasUnknownStatus2)?,
                ) != GoalClaimedDecision::Stop
            {
                return Ok(false);
            }
            let did = escape_graphql_string(&goal.node_did);
            let session = escape_graphql_string(&goal.session_id);
            let response = txn.execute(&format!(r#"{{ AgentRequest(filter: {{
                node_did: {{ _eq: "{did}" }}, session_id: {{ _eq: "{session}" }}
            }}, order: [{{ created_at: DESC }}, {{ request_id: DESC }}]) {{ {SIGNED_REQUEST_FIELDS} }} }}"#)).await?;
            let requests: Vec<AgentRequestRow> = serde_json::from_value(
                response.pointer("/data/AgentRequest").cloned()
                    .ok_or(GoalError::ClaimedStopRequestQueryOmittedRows)?
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

type StoppedReason = std::sync::Mutex<Option<String>>;

async fn stage_claimed_continuation(
    txn: &ConfigApplyTxn<'_>,
    identity: &dyn NodeIdentity,
    observed: &GoalDocument,
    parent_request_id: &str,
    content: &str,
    wrapup: bool,
    stopped: &StoppedReason,
) -> Result<Option<GoalResumeReceipt>> {
    *stopped.lock().unwrap_or_else(|poison| poison.into_inner()) = None;
    if identity.did() != observed.node_did {
        return Err(GoalError::ClaimedPublicationRequiresGoalOwnerS);
    }
    let Some(goal) =
        load_canonical_goal_in_txn(txn, &observed.node_did, &observed.session_id).await?
    else {
        return Ok(None);
    };
    if goal.doc_id != observed.doc_id {
        return Ok(None);
    }
    let did = escape_graphql_string(&goal.node_did);
    let session = escape_graphql_string(&goal.session_id);
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{
        node_did: {{ _eq: "{did}" }}, session_id: {{ _eq: "{session}" }}
    }}, order: [{{ created_at: DESC }}, {{ request_id: DESC }}]) {{ {SIGNED_REQUEST_FIELDS} }} }}"#
        ))
        .await?;
    let requests: Vec<AgentRequestRow> = serde_json::from_value(
        response
            .pointer("/data/AgentRequest")
            .cloned()
            .ok_or(GoalError::ClaimedParentQueryOmittedRows)?,
    )?;
    let parents: Vec<_> = requests
        .iter()
        .filter(|row| row.request_id == parent_request_id)
        .collect();
    if parents.len() != 1 {
        return Err(GoalError::ClaimedPredecessorMustUniquelyBelongGoal);
    }
    let parent_row = parents[0];
    let parent = crate::watcher::AgentRequest::try_from(parent_row.clone())?;
    let behavior = goal_continuation_agent(txn, &parent).await?;
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
            .ok_or(GoalError::ClaimedReceiptQueryOmittedRows)?,
    )?;
    if children.len() > 1 {
        return Err(GoalError::AmbiguousClaimedContinuationReceipt);
    }
    let session_hop = match children.first() {
        Some(child) => child
            .request_hop
            .and_then(|hop| u32::try_from(hop).ok())
            .ok_or(GoalError::ClaimedContinuationReceiptLacksHop)?,
        None => {
            crate::session::load_session_current_hop_in_txn(txn, &goal.node_did, &goal.session_id)
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
    create.input.goal_assignment = Some(continuation_assignment(
        &goal,
        &parent,
        children.first().and_then(|child| child.doc_id.as_deref()),
    )?);
    let expected = GoalBackedRequestFingerprint::from_create(&create)?;

    if let Some(child) = children.first() {
        verify_runtime_local_control_receipt(
            child,
            &goal.node_did,
            parent_request_id,
            parent.requester_did.as_deref().unwrap_or(&goal.node_did),
        )?;
        let actual: GoalBackedRequestFingerprint =
            serde_json::from_value(serde_json::to_value(child)?)?;
        if actual != expected {
            return Err(GoalError::ClaimedContinuationReceiptConflictsObservedPublication);
        }
        return Ok(Some(GoalResumeReceipt {
            goal_status: goal
                .parsed_status()
                .ok_or(GoalError::GoalHasUnknownStatus2)?,
            goal_id: goal.goal_id,
            request_id: child.request_id.clone(),
            doc_id: child
                .doc_id
                .clone()
                .ok_or(GoalError::ClaimedReceiptHasNoDocumentID)?,
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
            .ok_or(GoalError::GoalPredecessorLacksPhysicalDocumentIdentity)?,
        &goal.node_did,
        &goal.session_id,
        parent_row.requester_did.as_deref(),
    )
    .await
    {
        wait_observation::WaitEvidence::Absent => {}
        wait_observation::WaitEvidence::Running => return Ok(None),
        wait_observation::WaitEvidence::Unavailable(error) => return Err(error.into()),
        wait_observation::WaitEvidence::Invalid(error) => {
            *stopped.lock().unwrap_or_else(|poison| poison.into_inner()) =
                stop_claimed_continuation_in_txn(
                    txn,
                    &goal,
                    sequence,
                    parent_request_id,
                    &format!("invalid Goal wait evidence: {}", ErrorChain(&error)),
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
            .ok_or(GoalError::GoalPredecessorLacksDocumentID)?,
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
    let response = txn
        .execute(&format!(
            r#"mutation {{ update_Goal(filter: {{
        _docID: {{ _eq: "{doc_id}" }}, node_did: {{ _eq: "{did}" }},
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
        .execute(&create.graphql_mutation().map_err(|message| GoalError::RequestFields { message })?)
        .await?;
    let child = response
        .pointer("/data/create_AgentRequest")
        .or_else(|| response.pointer("/data/add_AgentRequest"))
        .ok_or(GoalError::ClaimedChildCreateOmittedResult)?;
    let child_doc = child
        .get("_docID")
        .or_else(|| child.get(0).and_then(|row| row.get("_docID")))
        .and_then(serde_json::Value::as_str)
        .ok_or(GoalError::ClaimedChildCreateOmittedDocumentID)?;
    if goal.assignment_root_request_doc_id.is_none() {
        bind_goal_first_request_in_txn(
            txn,
            &goal,
            child_doc,
            create.input.goal_assignment.as_ref(),
        )
        .await?;
    }
    Ok(Some(GoalResumeReceipt {
        goal_status: goal
            .parsed_status()
            .ok_or(GoalError::GoalHasUnknownStatus2)?,
        goal_id: goal.goal_id,
        request_id: create.request_id,
        doc_id: child_doc.to_owned(),
        created: true,
    }))
}

async fn stop_claimed_continuation_in_txn(
    txn: &ConfigApplyTxn<'_>,
    goal: &GoalDocument,
    sequence: i64,
    parent_request_id: &str,
    reason: &str,
    record_failure: bool,
    now: DateTime<Utc>,
) -> Result<Option<String>> {
    let state = goal.state().ok_or(GoalError::GoalHasUnknownStatus2)?;
    let failure_field = if record_failure {
        format!(r#", last_failure: "{}""#, escape_graphql_string(reason))
    } else {
        String::new()
    };
    let updated_at = escape_graphql_string(&now.to_rfc3339());
    let action = if state.status == GoalStatus::Active {
        GoalAction::Pause
    } else {
        GoalAction::WrapupAbandoned
    };
    let Some(post) = state.step(action) else {
        return Ok(None);
    };
    let fields = if action == GoalAction::Pause {
        format!(
            r#"status: "{}", active_time_seconds: {}, active_started_at: null{failure_field}, updated_at: "{updated_at}""#,
            post.status.as_str(),
            goal.current_active_time_seconds(now),
        )
    } else {
        format!(r#"wrapup_completed: true{failure_field}, updated_at: "{updated_at}""#)
    };
    let doc_id = escape_graphql_string(&goal.doc_id);
    let did = escape_graphql_string(&goal.node_did);
    let status = escape_graphql_string(&goal.status);
    let parent_id = escape_graphql_string(parent_request_id);
    let response = txn
        .execute(&format!(
            r#"mutation {{ update_Goal(filter: {{
        _docID: {{ _eq: "{doc_id}" }}, node_did: {{ _eq: "{did}" }},
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
