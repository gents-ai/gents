//! Publish an already claimed continuation through the existing Goal transaction.
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
    // Preserve the automatic queue writer's node actor; target identity signs
    // the child independently of the database actor.
    let txn = ConfigApplyTxn::begin_local(node, None).await?;
    match stage_claimed_continuation(
        &txn,
        &identity,
        observed,
        parent_request_id,
        content,
        wrapup,
    )
    .await
    {
        Ok(Some(receipt)) => {
            if receipt.created {
                txn.commit().await?;
            } else {
                txn.discard().await?;
            }
            Ok(Some(receipt))
        }
        Ok(None) => {
            txn.discard().await?;
            Ok(None)
        }
        Err(error) => {
            let _ = txn.discard().await;
            Err(error)
        }
    }
}

async fn stage_claimed_continuation(
    txn: &ConfigApplyTxn<'_>,
    identity: &dyn AgentIdentity,
    observed: &GoalDocument,
    parent_request_id: &str,
    content: &str,
    wrapup: bool,
) -> Result<Option<GoalResumeReceipt>> {
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
    let mut create = prepare_goal_continuation(
        &parent,
        behavior,
        &goal.goal_id,
        content,
        sequence,
        wrapup,
        &created_at,
    )?;
    let expected = GoalBackedRequestFingerprint::from_create(&create)?;
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
    if let Some(child) = children.first() {
        verify_runtime_local_control_receipt(child, &goal.agent_did, parent_request_id)?;
        let actual: GoalBackedRequestFingerprint =
            serde_json::from_value(serde_json::to_value(child)?)?;
        anyhow::ensure!(
            actual == expected,
            "claimed continuation receipt conflicts with the observed publication"
        );
        return Ok(Some(GoalResumeReceipt {
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
    ) || goal.last_continued_from_request_id.as_deref() != Some(parent_request_id)
        || !parent_row
            .lifecycle_state
            .is_some_and(RequestLifecycleState::is_terminal)
        || !goal_session_is_idle(&requests)
        || !latest_goal_request(&goal, &requests).is_some_and(|row| row.doc_id == parent_row.doc_id)
    {
        return Ok(None);
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
        goal_id: goal.goal_id,
        request_id: create.request_id,
        doc_id: child_doc.to_owned(),
        created: true,
    }))
}

#[cfg(test)]
mod contract_tests;
