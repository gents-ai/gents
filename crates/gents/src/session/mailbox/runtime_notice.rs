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
    #[error("runtime notice source collides with another scope")]
    SourceCollision,
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeNoticeError {
    #[error(transparent)]
    Scope(#[from] RuntimeNoticeScopeError),
    #[error(transparent)]
    Unavailable(#[from] anyhow::Error),
}

pub(crate) async fn publish(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    notice: RuntimeNotice,
) -> std::result::Result<MailboxItem, RuntimeNoticeError> {
    let session_id = notice.session_id.as_str();
    let owner = crate::config_client::ConfigAccess::transact_local_readonly(
        node,
        None,
        "mailbox.runtime_notice.session",
        move |txn| {
            Box::pin(async move {
                crate::session::load_agent_session_owner_in_txn(txn, node_did, session_id).await
            })
        },
    )
    .await?
    .ok_or(RuntimeNoticeScopeError::MissingSession)?;
    let response = crate::config_client::ConfigAccess::local(node.clone()).execute(&format!(
        r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{}" }}, request_id: {{ _eq: "{}" }} }}) {{ session_id agent_id requester_did }} }}"#,
        escape_graphql_string(node_did), escape_graphql_string(&notice.request_id),
    )).await.map_err(anyhow::Error::new)?;
    let requests = response["data"]["AgentRequest"]
        .as_array()
        .context("runtime notice request lookup")?;
    if requests.len() != 1 {
        return Err(RuntimeNoticeScopeError::InvalidRequest.into());
    }
    let request = &requests[0];
    if request["session_id"].as_str() != Some(session_id)
        || request["agent_id"].as_str() != Some(owner.session.agent_id.as_str())
        || request["requester_did"].as_str() != owner.session.requester_did.as_deref()
    {
        return Err(RuntimeNoticeScopeError::ScopeMismatch.into());
    }
    let context = MailboxStampContext {
        requester_did: owner
            .session
            .requester_did
            .unwrap_or_else(|| node_did.to_owned()),
        node_did: node_did.to_owned(),
        agent_id: owner.session.agent_id,
        session_id: Some(notice.session_id.clone()),
    };
    let kind = if notice.failed {
        MailboxKind::Failed
    } else {
        MailboxKind::Finished
    };
    // Acknowledging a terminal event must not cause it to reappear after a
    // runtime restart or a crash between database commit and the local receipt.
    if let Some(existing) = load_prefix_items(
        node,
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
            || existing.session_id.as_deref() != Some(session_id)
            || existing.request_id.as_deref() != Some(notice.request_id.as_str())
        {
            return Err(RuntimeNoticeScopeError::SourceCollision.into());
        }
        return Ok(existing);
    }
    stamp_create(
        node,
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
    )
    .await
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runtime_notice_requires_canonical_scope_and_does_not_reappear_after_ack() {
        let node = super::super::tests::test_node().await;
        let did = "did:test:agent";
        crate::session::create_session_with_id(&node, "original-session", "operator", did)
            .await
            .unwrap();
        let access = crate::config_client::ConfigAccess::local(node.clone());
        access.write("test.runtime_notice.request", r#"mutation { create_AgentRequest(input: {
            request_id: "original-request", purpose: "normal", node_did: "did:test:agent",
            agent_id: "operator", session_id: "original-session", content: "update",
            lifecycle_state: "completed", execution_origin: "interactive", created_at: "2026-01-01T00:00:00Z"
        }) { _docID } }"#).await.unwrap();
        let mut notice = RuntimeNotice {
            source_id: "service-update:fixture".into(),
            session_id: "original-session".into(),
            request_id: "missing-request".into(),
            title: "Runtime updated".into(),
            summary: "Original runtime identity retained".into(),
            failed: false,
        };
        assert!(publish(&node, did, notice.clone()).await.is_err());
        assert!(list_mailbox_items(&node, did, None)
            .await
            .unwrap()
            .is_empty());
        notice.request_id = "original-request".into();
        let first = publish(&node, did, notice.clone()).await.unwrap();
        assert_eq!(first.session_id.as_deref(), Some("original-session"));
        assert_eq!(first.request_id.as_deref(), Some("original-request"));
        dismiss_mailbox_item(&node, &first.doc_id, did)
            .await
            .unwrap();
        let replay = publish(&node, did, notice).await.unwrap();
        assert_eq!(first.doc_id, replay.doc_id);
        assert_eq!(replay.parsed_status(), Some(MailboxStatus::Dismissed));
        assert_eq!(list_mailbox_items(&node, did, None).await.unwrap().len(), 1);
    }
    #[tokio::test]
    async fn runtime_notice_preserves_desktop_requester_scope() {
        let node = super::super::tests::test_node().await;
        let access = crate::config_client::ConfigAccess::local(node.clone());
        access.write("test.runtime_notice.desktop_session", r#"mutation { create_AgentSession(input: {
            session_id: "desktop-session", node_did: "did:test:agent", requester_did: "did:test:owner",
            agent_id: "operator", created_at: "2026-01-01T00:00:00Z"
        }) { _docID } }"#).await.unwrap();
        access.write("test.runtime_notice.desktop_request", r#"mutation { create_AgentRequest(input: {
            request_id: "desktop-request", purpose: "normal", node_did: "did:test:agent", requester_did: "did:test:owner",
            agent_id: "operator", session_id: "desktop-session", content: "update",
            lifecycle_state: "completed", execution_origin: "interactive", created_at: "2026-01-01T00:00:00Z"
        }) { _docID } }"#).await.unwrap();
        let item = publish(
            &node,
            "did:test:agent",
            RuntimeNotice {
                source_id: "service-update:desktop-fixture".into(),
                session_id: "desktop-session".into(),
                request_id: "desktop-request".into(),
                title: "Runtime updated".into(),
                summary: "Node identity retained".into(),
                failed: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(item.requester_did, "did:test:owner");
        assert_eq!(item.session_id.as_deref(), Some("desktop-session"));
        assert_eq!(item.request_id.as_deref(), Some("desktop-request"));
        assert!(list_mailbox_items(&node, "did:test:agent", None)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            list_mailbox_items(&node, "did:test:owner", None)
                .await
                .unwrap()
                .len(),
            1
        );
    }
}
