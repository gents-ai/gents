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

pub(crate) async fn publish(
    _node: &Arc<EmbeddedNode>,
    _node_did: &str,
    _identities: &[Arc<dyn crate::store::identity::NodeIdentity>],
    _notice: RuntimeNotice,
) -> std::result::Result<MailboxItem, RuntimeNoticeError> {
    Err(RuntimeNoticeError::MissingClaimProvenance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::identity::NodeIdentity as _;

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
        let model = &crate::lean_vocab_test::lean_contract_snapshot().mailbox_runtime_notice_case;
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
