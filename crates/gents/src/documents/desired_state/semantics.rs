use anyhow::Result;
use serde_json::Value;

use super::plan::DesiredStateApplyPlan;
use super::{ConfigApplyTxn, ConfigurationNodeBinding};

/// Configuration meaning the generic apply defers to the documents owner.
/// The apply calls each hook at a fixed point: event grants are captured
/// before any write and granted after removals, every InferenceBackend
/// document is written or removed in plan order through this owner, and
/// event consumers are seeded after workspace selections.
#[async_trait::async_trait]
pub(crate) trait DesiredStateSemantics: Send + Sync {
    async fn capture_event_grants(
        &self,
        txn: &ConfigApplyTxn<'_>,
        plan: &DesiredStateApplyPlan,
    ) -> Result<Box<dyn PendingEventGrants>>;

    /// `existing` is the projected stored document when one is present.
    async fn write_inference_backend(
        &self,
        txn: &ConfigApplyTxn<'_>,
        update: &Value,
        existing: Option<&Value>,
    ) -> Result<()>;

    async fn remove_inference_backend(
        &self,
        txn: &ConfigApplyTxn<'_>,
        metadata_doc: &str,
        node_did: &str,
        backend_id: &str,
    ) -> Result<()>;

    /// Cursor creation belongs to the configuration transaction.
    async fn seed_event_consumers(
        &self,
        txn: &ConfigApplyTxn<'_>,
        plan: &DesiredStateApplyPlan,
        binding: Option<&ConfigurationNodeBinding>,
    ) -> Result<()>;
}

#[async_trait::async_trait]
pub(crate) trait PendingEventGrants: Send {
    async fn grant_changes(self: Box<Self>, txn: &ConfigApplyTxn<'_>) -> Result<()>;
}
