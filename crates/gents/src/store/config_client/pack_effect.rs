use std::sync::Arc;

use serde_json::Value;

use super::{
    ConfigApplyTxn, PackEvidence, PackMutation, PackRelationship, PackRelationshipEvidence,
};

pub(super) struct PackInterpreterApproval {
    pub scope: super::PackApprovalScope,
    pub approver: Arc<dyn PackEffectApprover>,
}

#[derive(Debug, thiserror::Error)]
pub enum PackApprovalError {
    #[error("pack approval transport failed: {0}")]
    Transport(String),
    #[error("pack effect was refused: {0}")]
    Refused(String),
}

#[async_trait::async_trait]
pub trait PackEffectApprover: Send + Sync {
    async fn approve_graph(
        &self,
        txn: &ConfigApplyTxn<'_>,
        mutation: &PackMutation,
        history: Option<Value>,
    ) -> Result<PackEvidence, PackApprovalError>;
    async fn approve_relationship(
        &self,
        txn: &ConfigApplyTxn<'_>,
        relationship: &PackRelationship,
    ) -> Result<PackRelationshipEvidence, PackApprovalError>;
    async fn next_timestamp(
        &self,
        txn: &ConfigApplyTxn<'_>,
        kind: &'static str,
    ) -> Result<String, PackApprovalError>;
    async fn graph_applied(&self, result: &Value) -> Result<(), PackApprovalError>;
    async fn relationship_applied(&self) -> Result<(), PackApprovalError>;
    async fn client_finished(&self, txn: &ConfigApplyTxn<'_>) -> Result<(), PackApprovalError>;
}

#[async_trait::async_trait]
pub trait PackReplayTransport: Send + Sync {
    async fn read(&self, document: &str, variables: &Value) -> Result<Value, PackApprovalError>;
    async fn apply_graph(&self, mutation: &PackMutation) -> Result<Value, PackApprovalError>;
    async fn apply_relationship(
        &self,
        relationship: &PackRelationship,
    ) -> Result<(), PackApprovalError>;
    async fn next_timestamp(&self, kind: &'static str) -> Result<String, PackApprovalError>;
    async fn commit(&self) -> Result<(), PackApprovalError>;
    async fn abort(&self) -> Result<(), PackApprovalError>;
}

#[derive(Clone)]
pub(super) struct PackReplayHandle(pub Arc<dyn PackReplayTransport>);

impl std::fmt::Debug for PackReplayHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PackReplayHandle(..)")
    }
}

impl PartialEq for PackReplayHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for PackReplayHandle {}
