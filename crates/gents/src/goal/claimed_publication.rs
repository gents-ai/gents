mod wait_observation;

use super::*;
use crate::request::lifecycle::materialize::{sign_request, RequestSigner};
use crate::request::lifecycle::queue::{
    goal_continuation_agent, goal_continuation_identity, prepare_goal_continuation,
};
use crate::store::config_client::ConfigApplyTxn;
use crate::store::identity::request_receipt::verify_runtime_local_control_receipt;
use crate::store::identity::NodeIdentity;
use gents_protocol::request_admission::SIGNED_REQUEST_FIELDS;
use gents_protocol::row::AgentRequestRow;

pub(crate) async fn publish_claimed_continuation_with_identity(
    node: &std::sync::Arc<EmbeddedNode>,
    identity: &dyn NodeIdentity,
    observed: &GoalDocument,
    parent_request_id: &str,
    content: &str,
    wrapup: bool,
) -> Result<Option<GoalResumeReceipt>> {
    let stopped = std::sync::Mutex::new(None);
    let stopped_ref = &stopped;
    let access = crate::ConfigAccess::local_with_optional_agent_signer(
        node.clone(),
        identity::Did::new(identity.did())?,
        identity.managed_agent_signer(),
    )?;
    let receipt = access
        .transact_write("goal.publish_claimed_continuation", move |txn| {
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
        })
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

pub(crate) async fn stop_claimed_continuation_for_unavailable_agent_from_access(
    access: &crate::ConfigAccess,
    observed: &GoalDocument,
    parent_request_id: &str,
    reason: &str,
) -> Result<bool> {
    access
        .transact_write("goal.stop_unavailable_claimed_continuation", |txn| {
            Box::pin(async move {
                let Some(goal) =
                    load_canonical_goal_in_txn(txn, &observed.node_did, &observed.session_id)
                        .await?
                else {
                    return Ok(false);
                };
                if goal.doc_id != observed.doc_id
                    || goal.status != observed.status
                    || goal.continuation_sequence() != observed.continuation_sequence()
                    || goal.last_continued_from_request_id
                        != observed.last_continued_from_request_id
                    || goal.last_continued_from_request_id.as_deref() != Some(parent_request_id)
                    || gate_claimed_goal_continuation(
                        GoalAgentObservation::Unavailable,
                        true,
                        false,
                        &goal.state().context("goal has an unknown status")?,
                    ) != GoalClaimedDecision::Stop
                {
                    return Ok(false);
                }
                let fields = super::request_head::request_selection_fields();
                let did = escape_graphql_string(&goal.node_did);
                let session = escape_graphql_string(&goal.session_id);
                let response = txn
                    .execute(&format!(
                        r#"{{ AgentRequest(filter: {{
                node_did: {{ _eq: "{did}" }}, session_id: {{ _eq: "{session}" }}
            }}, order: [{{ created_at: DESC }}, {{ request_id: DESC }}]) {{ {fields} }} }}"#
                    ))
                    .await?;
                let requests: Vec<AgentRequestRow> = serde_json::from_value(
                    response
                        .pointer("/data/AgentRequest")
                        .cloned()
                        .context("claimed stop request query omitted rows")?,
                )?;
                if !goal_session_is_idle(&requests)
                    || !latest_goal_request(&goal, &requests).is_some_and(|parent| {
                        parent.request_id == parent_request_id
                            && parent
                                .lifecycle_state
                                .is_some_and(RequestLifecycleState::is_terminal)
                    })
                    || requests.iter().any(|request| {
                        request.caused_by_trigger_kind.as_deref() == Some(GOAL_TRIGGER_KIND)
                            && request.caused_by_trigger_id.as_deref()
                                == Some(goal.goal_id.as_str())
                            && request.caused_by_parent_request_id.as_deref()
                                == Some(parent_request_id)
                    })
                {
                    return Ok(false);
                }
                Ok(stop_claimed_continuation_in_txn(
                    txn,
                    &goal,
                    observed.continuation_sequence(),
                    parent_request_id,
                    reason,
                    false,
                    Utc::now(),
                )
                .await?
                .is_some())
            })
        })
        .await
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
    anyhow::ensure!(
        txn.actor_did() == Some(identity.did()),
        "claimed publication transaction must act as its signing agent"
    );
    let Some(goal) =
        load_canonical_goal_in_txn(txn, &observed.node_did, &observed.session_id).await?
    else {
        return Ok(None);
    };
    if goal.doc_id != observed.doc_id {
        return Ok(None);
    }
    let fields = super::request_head::request_selection_fields();
    let did = escape_graphql_string(&goal.node_did);
    let session = escape_graphql_string(&goal.session_id);
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{
        node_did: {{ _eq: "{did}" }}, session_id: {{ _eq: "{session}" }}
    }}, order: [{{ created_at: DESC }}, {{ request_id: DESC }}]) {{ {fields} }} }}"#
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
    let parent = gents_protocol::row::AgentRequest::try_from(parent_row.clone())?;
    anyhow::ensure!(
        identity.did() == parent.agent_did,
        "claimed publication requires the selected agent's signing identity"
    );
    let session = crate::store::config_client::agent_session::load_agent_session_row_in_txn(
        txn,
        &goal.node_did,
        &goal.session_id,
        parent.requester_did.as_deref(),
    )
    .await?
    .context("claimed publication session is unavailable")?;
    anyhow::ensure!(
        session.session.agent_did == identity.did(),
        "claimed publication requires the session's selected agent"
    );
    let (agent_doc, _) = crate::documents::read_desired_state_record_in_txn(
        txn,
        crate::Collection::Agent,
        &goal.node_did,
        identity.did(),
    )
    .await?
    .context("claimed publication Agent is unavailable")?;
    let behavior = goal_continuation_agent(txn, &parent).await?;
    let sequence = observed.continuation_sequence();
    let now = Utc::now();
    let created_at = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let key = goal_continuation_identity(&goal.goal_id, parent_request_id, sequence)?.retry_key;
    let response = {
        use gents_protocol::document_query::{
            fields::agent_request as request, Filter, Query, Selection,
        };
        let query = Query::select(Selection::owned(SIGNED_REQUEST_FIELDS))
            .filter(Filter::eq(request::RETRY_KEY, &key))
            .render();
        txn.run(&query).await?
    };
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
    let session_hop = match children.first() {
        Some(child) => child
            .request_hop
            .and_then(|hop| u32::try_from(hop).ok())
            .context("claimed continuation receipt lacks its hop")?,
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
    create.session_doc_id = Some(session.doc_id.clone());
    create.agent_doc_id = Some(agent_doc.clone());
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
            parent.requester_did.as_deref().unwrap_or(&parent.agent_did),
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
    if crate::session::mailbox::required_question_in_txn(txn, parent_row).await? {
        return Ok(None);
    }
    match wait_observation::observe_intentional_wait(
        txn,
        parent_row
            .doc_id
            .as_deref()
            .context("goal predecessor lacks physical document identity")?,
        &goal.node_did,
        &goal.session_id,
        parent_row.requester_did.as_deref(),
    )
    .await
    {
        wait_observation::WaitEvidence::Absent => {}
        wait_observation::WaitEvidence::Running => return Ok(None),
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
    let response = {
        use gents_protocol::document_query::{fields::goal as goal_fields, Filter, Input, Update};
        let filter = Filter::eq(goal_fields::DOC_ID, &goal.doc_id)
            .and(Filter::eq(goal_fields::NODE_DID, &goal.node_did))
            .and(Filter::eq(goal_fields::STATUS, &goal.status))
            .and(Filter::eq(goal_fields::CONTINUATION_SEQUENCE, sequence))
            .and(Filter::eq(
                goal_fields::LAST_CONTINUED_FROM_REQUEST_ID,
                parent_request_id,
            ));
        let input = Input::new().set(goal_fields::UPDATED_AT, now.to_rfc3339());
        txn.run(&Update::matching(filter, input).render()).await?
    };
    if !response
        .pointer("/data/update_Goal")
        .is_some_and(mutation_returned_rows)
    {
        return Ok(None);
    }
    let child_doc = crate::request::lifecycle::stage_runtime_request_in_txn(
        txn,
        &create,
        &session.doc_id,
        &agent_doc,
    )
    .await?;
    if goal.assignment_root_request_doc_id.is_none() {
        bind_goal_first_request_in_txn(
            txn,
            &goal,
            &child_doc,
            create.input.goal_assignment.as_ref(),
        )
        .await?;
    }
    Ok(Some(GoalResumeReceipt {
        goal_status: goal.parsed_status().context("goal has an unknown status")?,
        goal_id: goal.goal_id,
        request_id: create.request_id,
        doc_id: child_doc,
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
    let state = goal.state().context("goal has an unknown status")?;
    let action = if state.status == GoalStatus::Active {
        GoalAction::Pause
    } else {
        GoalAction::WrapupAbandoned
    };
    let Some(post) = state.step(action) else {
        return Ok(None);
    };
    let input = {
        use gents_protocol::document_query::{fields::goal as goal_fields, Input};
        let input = if action == GoalAction::Pause {
            Input::new()
                .set(goal_fields::STATUS, post.status.as_str())
                .set(
                    goal_fields::ACTIVE_TIME_SECONDS,
                    goal.current_active_time_seconds(now),
                )
                .set_null(goal_fields::ACTIVE_STARTED_AT)
        } else {
            Input::new().set(goal_fields::WRAPUP_COMPLETED, true)
        };
        let input = if record_failure {
            input.set(goal_fields::LAST_FAILURE, reason)
        } else {
            input
        };
        input.set(goal_fields::UPDATED_AT, now.to_rfc3339())
    };
    let response = {
        use gents_protocol::document_query::{fields::goal as goal_fields, Filter, Update};
        let filter = Filter::eq(goal_fields::DOC_ID, &goal.doc_id)
            .and(Filter::eq(goal_fields::NODE_DID, &goal.node_did))
            .and(Filter::eq(goal_fields::STATUS, &goal.status))
            .and(Filter::eq(goal_fields::CONTINUATION_SEQUENCE, sequence))
            .and(Filter::eq(
                goal_fields::LAST_CONTINUED_FROM_REQUEST_ID,
                parent_request_id,
            ));
        txn.run(&Update::matching(filter, input).render()).await?
    };
    Ok(response
        .pointer("/data/update_Goal")
        .is_some_and(mutation_returned_rows)
        .then(|| reason.to_owned()))
}

#[cfg(test)]
mod contract_tests;
