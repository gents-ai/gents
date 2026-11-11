use super::*;

/// Local runtime event, never a user prompt or a replacement session row.
#[derive(Debug, Clone)]
pub struct RuntimeNotice {
    pub source_id: String,
    pub session_id: String,
    pub request_id: String,
    pub title: String,
    pub summary: String,
    pub failed: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeNoticeScopeError {
    #[error("runtime notice session is missing")]
    MissingSession,
    #[error("runtime notice request is missing or ambiguous")]
    InvalidRequest,
    #[error("runtime notice request and session scope disagree")]
    ScopeMismatch,
    #[error("runtime notice session has no recipient")]
    MissingRecipient,
    #[error("runtime notice source collides with another scope")]
    SourceCollision,
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeNoticeError {
    #[error(transparent)]
    Scope(#[from] RuntimeNoticeScopeError),
    #[error(transparent)]
    Unavailable(#[from] anyhow::Error),
    #[error("runtime notice is deferred until its admitted request provenance is available")]
    MissingClaimProvenance,
}

/// The receipt's request label only locates the signed AgentRequest as its
/// held target Agent sees it. The recipient is that record's verified client
/// signer, and only once its native claim committed (`claimed_at` is written
/// solely by the claim transaction after client authorization).
pub async fn publish(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    identities: &[Arc<dyn gents_store::identity::NodeIdentity>],
    notice: RuntimeNotice,
) -> std::result::Result<MailboxItem, RuntimeNoticeError> {
    let query = {
        use gents_protocol::document_query::{
            fields::agent_request as fields, Filter, Query, Selection,
        };
        Query::select(
            Selection::owned(gents_protocol::request_admission::SIGNED_REQUEST_FIELDS)
                .field(fields::CLAIMED_AT),
        )
        .filter(Filter::eq(fields::NODE_DID, node_did))
        .filter(Filter::eq(fields::REQUEST_ID, &notice.request_id))
        .limit(2)
        .render()
    };
    let mut claimed = None;
    for identity in identities {
        let access = gents_store::config_client::ConfigAccess::local_managed_agent(
            node.clone(),
            identity::Did::new(identity.did()).map_err(anyhow::Error::new)?,
            identity.as_ref(),
        )
        .map_err(anyhow::Error::new)?;
        let response = access.read(&query).await.map_err(anyhow::Error::new)?;
        let rows = response["data"]["AgentRequest"]
            .as_array()
            .context("runtime notice request lookup")?;
        let row = match rows.as_slice() {
            [] => continue,
            [row] => row,
            _ => return Err(RuntimeNoticeScopeError::InvalidRequest.into()),
        };
        if claimed.is_some() {
            return Err(RuntimeNoticeScopeError::InvalidRequest.into());
        }
        let committed = row["claimed_at"].as_str().is_some_and(|at| !at.is_empty());
        let row: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(row.clone()).context("decode runtime notice request")?;
        claimed = Some((access, identity.did().to_owned(), row, committed));
    }
    let Some((access, agent_did, row, committed)) = claimed else {
        return Err(RuntimeNoticeError::MissingClaimProvenance);
    };
    gents_store::identity::request_receipt::verify_request_receipt_signature(&row)
        .map_err(|_| RuntimeNoticeScopeError::InvalidRequest)?;
    let admission = gents_store::identity::request_receipt::row_admission(&row)
        .map_err(|_| RuntimeNoticeScopeError::InvalidRequest)?;
    if row.node_did.as_deref() != Some(node_did)
        || row.agent_did.as_deref() != Some(agent_did.as_str())
        || row.session_id.as_deref() != Some(notice.session_id.as_str())
    {
        return Err(RuntimeNoticeScopeError::ScopeMismatch.into());
    }
    if admission.kind != gents_protocol::request_admission::AgentRequestAdmissionKind::Client {
        return Err(RuntimeNoticeScopeError::MissingRecipient.into());
    }
    if !committed {
        return Err(RuntimeNoticeError::MissingClaimProvenance);
    }
    let context = MailboxStampContext {
        requester_did: admission.signer_did,
        node_did: node_did.to_owned(),
        agent_did,
        session_id: Some(notice.session_id.clone()),
    };
    let kind = if notice.failed {
        MailboxKind::Failed
    } else {
        MailboxKind::Finished
    };
    // An acknowledged terminal event must not reappear after a restart or a
    // crash between the database commit and the local publication marker.
    if let Some(existing) = load_prefix_items(
        &access,
        &context.requester_did,
        MailboxSourceKind::Runtime,
        &notice.source_id,
        kind,
    )
    .await?
    .into_iter()
    .next()
    {
        if existing.node_did != node_did
            || existing.session_id.as_deref() != Some(notice.session_id.as_str())
            || existing.request_id.as_deref() != Some(notice.request_id.as_str())
        {
            return Err(RuntimeNoticeScopeError::SourceCollision.into());
        }
        return Ok(existing);
    }
    stamp_create_with_access(
        node,
        &access,
        &context,
        FileMailboxItemArgs {
            kind,
            action: MailboxAction::Ack,
            title: notice.title,
            summary: Some(notice.summary),
            payload: None,
            source_kind: MailboxSourceKind::Runtime,
            source_id: notice.source_id,
            session_id: Some(notice.session_id),
            request_id: Some(notice.request_id),
            cause_doc_id: None,
            expected_collection: None,
            parent_item_id: None,
            deadline_at: None,
        },
        MAILBOX_CLOSE_COLLECTIONS,
    )
    .await
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_store::identity::NodeIdentity as _;

    #[tokio::test]
    async fn runtime_notice_without_claim_is_deferred() {
        let node = super::super::tests::test_node().await;
        let agent = super::super::tests::agent_identity();
        let notice = RuntimeNotice {
            source_id: "service-update:operation".into(),
            session_id: "logical-session".into(),
            request_id: "logical-request".into(),
            title: "Update complete".into(),
            summary: "Completed".into(),
            failed: false,
        };
        let result = publish(
            &node,
            super::super::tests::root_identity().did(),
            &[agent],
            notice,
        )
        .await;
        let model = &gents_lean_vocab::lean_contract_snapshot().mailbox_runtime_notice_case;
        assert_eq!(
            matches!(result, Err(RuntimeNoticeError::MissingClaimProvenance)),
            model["deferred"].as_bool().unwrap()
        );
        let access = super::super::tests::agent_access(&node);
        let rows = access.execute("{ MailboxItem { _docID } }").await.unwrap();
        assert_eq!(
            !rows["data"]["MailboxItem"].as_array().unwrap().is_empty(),
            model["document"].as_bool().unwrap()
        );
        assert_eq!(model["recipient"], false);
        node.shutdown().await.unwrap();
    }
}
