use anyhow::{Context, Result};
use gents_protocol::output::rows::{
    output_segment_create_variables, transcript_message_create_variables,
};

use super::ConfigApplyTxn;
use crate::store::graphql::escape_graphql_string;

mod ownership;
pub(crate) use ownership::{
    publish_agent_session_child_relationships_with_access,
    recover_agent_session_child_relationships_with_access,
};

#[cfg(test)]
mod auxiliary_binding_contract_tests;

/// Capture the physical session while creating a child so its creator can
/// restore the native parent relationship after the document commits.
pub(crate) async fn session_child_binding_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<(String, String)> {
    anyhow::ensure!(
        txn.actor_did().is_some(),
        "session child creation requires an explicit actor"
    );
    let scope = gents_protocol::session::session_scope_filter(node_did, session_id, requester_did);
    let response = txn
        .execute(&format!(
            "{{ AgentSession(filter: {{ {scope} }}, limit: 2) {{ _docID agent_did }} }}"
        ))
        .await?;
    let rows = response["data"]["AgentSession"]
        .as_array()
        .context("session child parent lookup omitted its authorized result")?;
    anyhow::ensure!(
        rows.len() == 1,
        "session child requires one visible physical session"
    );
    let session_doc = rows[0]["_docID"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("session child parent is missing its document ID")?;
    let agent = rows[0]["agent_did"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("session child parent is missing its selected agent")?;
    Ok((session_doc.to_owned(), agent.to_owned()))
}

#[derive(Clone, Copy)]
pub(crate) enum SessionChildSource<'a> {
    Request(gents_protocol::request_admission::RequestPurpose),
    Output(&'a gents_protocol::output::OutputSource),
    Capture(&'a gents_protocol::rendered_request::CaptureScope),
}

pub(crate) async fn session_child_binding_for_request_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    request_doc_id: &str,
    source: SessionChildSource<'_>,
) -> Result<(String, String)> {
    use gents_protocol::output::OutputSource;
    use gents_protocol::request_admission::RequestPurpose;
    let purpose = match source {
        SessionChildSource::Request(purpose) => purpose,
        SessionChildSource::Output(OutputSource::ProviderTurn { scope, .. }) => {
            scope.kind.request_purpose()
        }
        SessionChildSource::Output(_) => RequestPurpose::Normal,
        SessionChildSource::Capture(scope) => scope.kind.request_purpose(),
    };
    if !purpose.is_auxiliary() {
        return agent_session_child_binding_in_txn(txn, node_did, session_id, requester_did).await;
    }
    async fn request_row(
        txn: &ConfigApplyTxn<'_>,
        doc_id: &str,
    ) -> Result<gents_protocol::row::AgentRequestRow> {
        let response = txn
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ {} }} }}"#,
                escape_graphql_string(doc_id),
                gents_protocol::request_admission::SIGNED_REQUEST_FIELDS,
            ))
            .await?;
        let rows = response["data"]["AgentRequest"]
            .as_array()
            .context("auxiliary request lookup omitted rows")?;
        anyhow::ensure!(rows.len() == 1, "auxiliary request is missing or ambiguous");
        let row: gents_protocol::row::AgentRequestRow = serde_json::from_value(rows[0].clone())?;
        anyhow::ensure!(
            row.doc_id.as_deref() == Some(doc_id),
            "auxiliary request physical identity differs"
        );
        Ok(row)
    }
    let auxiliary = request_row(txn, request_doc_id)
        .await
        .context("load auxiliary child request")?;
    anyhow::ensure!(
        auxiliary.purpose == Some(purpose)
            && auxiliary.node_did.as_deref() == Some(node_did)
            && auxiliary.session_id.as_deref() == Some(session_id)
            && auxiliary.requester_did.as_deref() == requester_did
            && auxiliary.agent_did.as_deref() == txn.actor_did(),
        "auxiliary child differs from its request or selected actor"
    );
    let parent_doc = auxiliary
        .caused_by_parent_request_doc_id
        .as_deref()
        .context("auxiliary request lacks its physical parent")?;
    let parent = request_row(txn, parent_doc)
        .await
        .context("load auxiliary parent request")?;
    crate::store::identity::request_receipt::verify_historical_auxiliary_receipt(
        &auxiliary, &parent,
    )?;
    let (session_doc, agent) = agent_session_child_binding_in_txn(
        txn,
        node_did,
        session_id,
        parent.requester_did.as_deref(),
    )
    .await?;
    anyhow::ensure!(
        auxiliary.agent_did.as_deref() == Some(agent.as_str())
            && parent
                .session_doc_id
                .as_deref()
                .is_none_or(|doc| doc == session_doc),
        "auxiliary parent differs from its physical session"
    );
    Ok((session_doc, agent))
}

pub(crate) async fn output_segment_bound_variables(
    txn: &ConfigApplyTxn<'_>,
    segment: &gents_protocol::output::OutputSegment,
) -> Result<serde_json::Value> {
    let (session_doc, agent) = session_child_binding_for_request_in_txn(
        txn,
        &segment.node_did,
        &segment.session_id,
        segment.requester_did.as_deref(),
        &segment.request_doc_id,
        SessionChildSource::Output(&segment.source),
    )
    .await?;
    let mut variables = output_segment_create_variables(segment)?;
    variables["input"]["session_doc_id"] = session_doc.into();
    variables["input"]["agent_did"] = agent.into();
    variables["input"]["creator_did"] = txn
        .actor_did()
        .context("session child creator missing")?
        .into();
    variables["input"]["parent_linked"] = false.into();
    Ok(variables)
}

pub(crate) async fn transcript_message_bound_variables(
    txn: &ConfigApplyTxn<'_>,
    message: &gents_protocol::output::TranscriptMessage,
) -> Result<serde_json::Value> {
    let (session_doc, agent) = agent_session_child_binding_in_txn(
        txn,
        &message.node_did,
        &message.session_id,
        message.requester_did.as_deref(),
    )
    .await?;
    transcript_message_variables_with_binding(
        message,
        session_doc,
        agent,
        txn.actor_did().context("session child creator missing")?,
    )
}

pub(crate) async fn fork_message_bound_variables(
    txn: &ConfigApplyTxn<'_>,
    message: &gents_protocol::output::TranscriptMessage,
) -> Result<serde_json::Value> {
    let (session_doc, agent) = session_child_binding_in_txn(
        txn,
        &message.node_did,
        &message.session_id,
        message.requester_did.as_deref(),
    )
    .await?;
    transcript_message_variables_with_binding(
        message,
        session_doc,
        agent,
        txn.actor_did().context("session child creator missing")?,
    )
}

/// A fork copy is a child of its destination session, bound by that session
/// rather than by a request the destination never ran.
pub(crate) async fn fork_output_segment_bound_variables(
    txn: &ConfigApplyTxn<'_>,
    segment: &gents_protocol::output::OutputSegment,
) -> Result<serde_json::Value> {
    let (session_doc, agent) = session_child_binding_in_txn(
        txn,
        &segment.node_did,
        &segment.session_id,
        segment.requester_did.as_deref(),
    )
    .await?;
    let mut variables = output_segment_create_variables(segment)?;
    variables["input"]["session_doc_id"] = session_doc.into();
    variables["input"]["agent_did"] = agent.into();
    variables["input"]["creator_did"] = txn
        .actor_did()
        .context("session child creator missing")?
        .into();
    variables["input"]["parent_linked"] = false.into();
    Ok(variables)
}

fn transcript_message_variables_with_binding(
    message: &gents_protocol::output::TranscriptMessage,
    session_doc: String,
    agent: String,
    creator: &str,
) -> Result<serde_json::Value> {
    let mut variables = transcript_message_create_variables(message)?;
    variables["input"]["session_doc_id"] = session_doc.into();
    variables["input"]["agent_did"] = agent.into();
    variables["input"]["creator_did"] = creator.into();
    variables["input"]["parent_linked"] = false.into();
    Ok(variables)
}

pub(crate) async fn agent_session_child_binding_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<(String, String)> {
    let binding = session_child_binding_in_txn(txn, node_did, session_id, requester_did).await?;
    anyhow::ensure!(
        txn.actor_did() == Some(binding.1.as_str()),
        "runtime session child requires its selected agent actor"
    );
    Ok(binding)
}
