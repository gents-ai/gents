use super::{
    assignment_replaced, observe_goal_outcome_binding, outcome_published, stage_outcome,
    OutcomeStage, FIRE_FIELDS,
};
use anyhow::{Context, Result};
#[cfg(test)]
use defra_node::Node as EmbeddedNode;
use gents_protocol::trigger_delivery::TriggerFire;

/// Recover as the Agent's managed signer, as runtime startup does.
#[cfg(test)]
pub(crate) async fn recover_outcomes(
    node: &std::sync::Arc<EmbeddedNode>,
    owner: &str,
    agent: &dyn gents_store::identity::NodeIdentity,
) -> Result<usize> {
    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did())?,
        agent,
    )?;
    recover_outcomes_with_access(&access, owner, agent.did()).await
}

pub async fn recover_outcomes_with_access(
    access: &gents_store::config_client::ConfigAccess,
    owner: &str,
    agent_did: &str,
) -> Result<usize> {
    anyhow::ensure!(
        access.actor_did().as_deref() == Some(agent_did),
        "trigger recovery actor differs from access actor"
    );
    let response = {
        use gents_protocol::document_query::{
            fields::trigger_fire as fields, Filter, Query, Selection,
        };
        access
            .read(
                &Query::select(Selection::owned(FIRE_FIELDS))
                    .filter(Filter::eq(fields::OWNER_DID, owner))
                    .filter(Filter::eq(fields::EMIT_OUTCOME, true))
                    .render(),
            )
            .await?
    };
    let fires: Vec<TriggerFire> = serde_json::from_value(response["data"]["TriggerFire"].clone())?;
    let now = chrono::Utc::now().to_rfc3339();
    let mut recovered = 0;
    for fire in fires {
        recovered += access
            .transact_write("trigger.recover_outcomes", |txn| {
                let (fire, now) = (&fire, &now);
                Box::pin(async move {
                    recover_fire_outcome_in_txn(txn, owner, fire, now, Some(agent_did)).await
                })
            })
            .await?;
    }
    Ok(recovered)
}

#[cfg(test)]
pub(crate) async fn recover_outcomes_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
) -> Result<usize> {
    let response = {
        use gents_protocol::document_query::{
            fields::trigger_fire as fields, Filter, Query, Selection,
        };
        txn.run_local_response(
            &Query::select(Selection::owned(FIRE_FIELDS))
                .filter(Filter::eq(fields::OWNER_DID, owner))
                .filter(Filter::eq(fields::EMIT_OUTCOME, true))
                .render(),
        )
        .await?
    };
    let fires: Vec<TriggerFire> = gents_store::graphql::rows(&response, "TriggerFire")?;
    let now = chrono::Utc::now().to_rfc3339();
    let mut recovered = 0;
    for fire in fires {
        recovered += recover_fire_outcome_in_txn(txn, owner, &fire, &now, None).await?;
    }
    Ok(recovered)
}

async fn recover_fire_outcome_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    fire: &TriggerFire,
    now: &str,
    agent_did: Option<&str>,
) -> Result<usize> {
    if outcome_published(txn, fire).await? {
        return Ok(0);
    }
    let response = {
        use gents_protocol::document_query::{
            fields::agent_request as fields, Filter, Query, Selection,
        };
        txn.run_local_response(
            &Query::select(
                Selection::new()
                    .field(fields::REQUEST_ID)
                    .field(fields::AGENT_DID)
                    .field(fields::LIFECYCLE_STATE)
                    .field(fields::FAILURE_REASON),
            )
            .filter(Filter::eq(fields::NODE_DID, owner))
            .filter(Filter::eq(fields::REQUEST_ID, &fire.request_id))
            .render(),
        )
        .await?
    };
    let requests: Vec<gents_protocol::row::AgentRequestRow> =
        gents_store::graphql::rows(&response, "AgentRequest")?;
    if requests.is_empty() && agent_did.is_some() {
        return Ok(0);
    }
    anyhow::ensure!(
        requests.len() == 1,
        "Task fire has no unique admitted request during outcome recovery"
    );
    let request = &requests[0];
    if agent_did.is_some_and(|agent| request.agent_did.as_deref() != Some(agent)) {
        return Ok(0);
    }
    let status = request
        .lifecycle_state
        .context("Task request is missing lifecycle state")?;
    let binding = if fire.goal_assignment_applied {
        observe_goal_outcome_binding(txn, fire).await?
    } else {
        None
    };
    let reason = binding
        .as_ref()
        .map(|goal| goal.status.as_str())
        .unwrap_or_else(|| {
            request
                .failure_reason
                .as_deref()
                .filter(|reason| !reason.is_empty())
                .unwrap_or(status.as_str())
        });
    Ok(usize::from(
        stage_outcome(
            txn,
            fire,
            OutcomeStage {
                goal: binding.as_ref(),
                assignment_replaced: assignment_replaced(fire, binding.as_ref()),
                request_terminal: status.is_terminal(),
                terminal_state: status.as_str(),
                reason,
            },
            now,
        )
        .await?,
    ))
}
