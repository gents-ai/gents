use super::{MailboxAction, MailboxItem, MailboxStatus};
use crate::config_client::ConfigApplyTxn;
use crate::graphql::{escape_graphql_string, rows, single_mutation_document};
use crate::watcher::AgentRequest;
use anyhow::{ensure, Context, Result};

pub(crate) async fn claim_reply_in_txn(
    txn: &ConfigApplyTxn<'_>,
    request: &AgentRequest,
    now: &str,
) -> Result<()> {
    // An event observing a mailbox row is not the user's reply to that row.
    if request.execution_origin.as_deref() != Some("interactive") {
        return Ok(());
    }
    let Some(source) = request.caused_by_source_doc_id.as_deref() else {
        return Ok(());
    };
    let query = format!(
        r#"{{ MailboxItem(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
        escape_graphql_string(source),
        super::MAILBOX_FIELDS,
    );
    let response = txn.execute_local_response(&query).await?;
    let items = rows::<MailboxItem>(&response, super::MAILBOX_COLLECTION)?;
    let Some(item) = items.first() else {
        // Event and control requests also carry source documents.
        return Ok(());
    };
    if item.parsed_action() != Some(MailboxAction::StartRequest) {
        return Ok(());
    }
    let query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
        escape_graphql_string(&request.doc_id),
        crate::request_admission::SIGNED_REQUEST_FIELDS,
    );
    let response = txn.execute_local_response(&query).await?;
    let records = rows::<gents_protocol::row::AgentRequestRow>(&response, "AgentRequest")?;
    let record = records
        .into_iter()
        .next()
        .context("mailbox reply request disappeared")?;
    let authenticated = crate::request_admission::verify_request_receipt_signature(&record).is_ok();
    let signed_request = AgentRequest::try_from(record)?;
    let now_time = chrono::Utc::now();
    let deadline_valid = match item.deadline_at.as_deref() {
        None => true,
        Some(deadline) => {
            chrono::DateTime::parse_from_rfc3339(deadline).is_ok_and(|deadline| deadline > now_time)
        }
    };
    validate_reply_claim(item, &signed_request, authenticated, deadline_valid).map_err(
        |error| crate::lifecycle::ClaimAdmissionError::MailboxReplyRejected {
            item_id: item.doc_id.clone(),
            reason: error.to_string(),
        },
    )?;
    ensure!(
        signed_request.agent_did == request.agent_did
            && signed_request.requester_did == request.requester_did
            && signed_request.behavior_id == request.behavior_id
            && signed_request.session_id == request.session_id,
        "mailbox reply changed during admission"
    );
    if item.parsed_status() == Some(MailboxStatus::Acted) {
        return Ok(());
    }
    let mutation = format!(
        r#"mutation {{ update_MailboxItem(
            filter: {{ _docID: {{ _eq: "{}" }}, status: {{ _eq: "open" }} }},
            input: {{ status: "acted", resolved_doc_id: "{}", resolved_at: "{}", updated_at: "{}" }}
        ) {{ _docID }} }}"#,
        escape_graphql_string(&item.doc_id),
        escape_graphql_string(&request.doc_id),
        escape_graphql_string(now),
        escape_graphql_string(now),
    );
    let response = txn.execute_local_response(&mutation).await?;
    ensure!(
        single_mutation_document(&response, "update_MailboxItem")?.is_some(),
        "mailbox reply claim did not consume an open item"
    );
    Ok(())
}

pub(super) fn validate_reply_claim(
    item: &MailboxItem,
    request: &AgentRequest,
    authenticated: bool,
    deadline_valid: bool,
) -> Result<()> {
    ensure!(
        item.parsed_action() == Some(MailboxAction::StartRequest),
        "mailbox item does not accept a request reply"
    );
    ensure!(authenticated, "mailbox reply is not authenticated");
    ensure!(
        request.execution_origin.as_deref() == Some("interactive"),
        "mailbox reply must be interactive"
    );
    ensure!(
        deadline_valid
            || (item.parsed_status() == Some(MailboxStatus::Acted)
                && item.resolved_doc_id.as_deref() == Some(request.doc_id.as_str())),
        "mailbox reply deadline has expired"
    );
    ensure!(
        !request.doc_id.is_empty(),
        "mailbox reply is missing its request document ID"
    );
    ensure!(
        request
            .requester_did
            .as_deref()
            .is_some_and(|did| !did.is_empty() && did == item.requester_did),
        "mailbox reply requester does not match"
    );
    ensure!(
        !request.agent_did.is_empty() && request.agent_did == item.target_agent_did,
        "mailbox reply target agent does not match"
    );
    ensure!(
        !item.target_behavior_id.is_empty() && request.behavior_id == item.target_behavior_id,
        "mailbox reply target behavior does not match"
    );
    ensure!(
        item.session_id
            .as_ref()
            .is_none_or(|session| session == &request.session_id),
        "mailbox reply session does not match"
    );
    ensure!(
        !item.doc_id.is_empty()
            && request.caused_by_source_doc_id.as_deref() == Some(item.doc_id.as_str()),
        "mailbox reply source does not match"
    );
    ensure!(
        match item.parsed_status() {
            Some(MailboxStatus::Open) => true,
            Some(MailboxStatus::Acted) =>
                item.resolved_doc_id.as_deref() == Some(request.doc_id.as_str()),
            _ => false,
        },
        "mailbox item is closed or was consumed by another request"
    );
    Ok(())
}
