use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use defra_node::Node as EmbeddedNode;

use super::delivery::{BackgroundReceiptBinding, DispatchStart};
use super::{ToolCallLifecycle, ToolCallState};
use crate::session::message::{SessionMessageCall, ToolCallTerminal};
use crate::store::config_client::{ConfigAccess, ConfigApplyTxn};

impl SessionMessageCall for ToolCallLifecycle {
    type ReceiptBinding = BackgroundReceiptBinding;
    type DispatchStart = DispatchStart;

    fn doc_id(&self) -> Option<&str> {
        ToolCallLifecycle::doc_id(self)
    }

    fn actor(&self) -> &identity::Did {
        ToolCallLifecycle::actor(self)
    }

    fn node_did(&self) -> &str {
        ToolCallLifecycle::node_did(self)
    }

    fn session_id(&self) -> &str {
        ToolCallLifecycle::session_id(self)
    }

    fn requester_did(&self) -> Option<&str> {
        ToolCallLifecycle::requester_did(self)
    }

    fn request_doc_id(&self) -> Option<&str> {
        ToolCallLifecycle::request_doc_id(self)
    }

    fn tool_call_id(&self) -> &str {
        ToolCallLifecycle::tool_call_id(self)
    }

    fn is_pending(&self) -> bool {
        self.state() == ToolCallState::Pending
    }

    fn is_running(&self) -> bool {
        self.state() == ToolCallState::Running
    }

    fn has_started(&self) -> bool {
        self.started_at().is_some()
    }

    fn unsuccessful_terminal(&self) -> Option<ToolCallTerminal> {
        match self.state() {
            ToolCallState::Failed => Some(ToolCallTerminal::Failed),
            ToolCallState::TimedOut => Some(ToolCallTerminal::TimedOut),
            ToolCallState::Cancelled => Some(ToolCallTerminal::Cancelled),
            ToolCallState::Pending | ToolCallState::Running | ToolCallState::Completed => None,
        }
    }

    fn access(&self) -> Result<ConfigAccess> {
        ToolCallLifecycle::access(self)
    }

    fn background_receipt_binding(&self) -> Result<BackgroundReceiptBinding> {
        ToolCallLifecycle::background_receipt_binding(self)
    }

    fn dispatch_start(&self) -> Result<DispatchStart> {
        ToolCallLifecycle::dispatch_start(self)
    }

    fn mark_started(&mut self, started_at: DateTime<Utc>) {
        ToolCallLifecycle::mark_started(self, started_at)
    }

    async fn reload(&self, node: &Arc<EmbeddedNode>, doc_id: &str) -> Result<Option<Self>> {
        let Some(mut durable) = ToolCallLifecycle::load_by_doc_id_as(
            node.clone(),
            ToolCallLifecycle::actor(self).clone(),
            doc_id,
            ToolCallLifecycle::node_did(self),
            ToolCallLifecycle::session_id(self),
            ToolCallLifecycle::requester_did(self),
        )
        .await?
        else {
            return Ok(None);
        };
        durable.inherit_signer(self)?;
        Ok(Some(durable))
    }

    async fn start_running_in_txn(
        txn: &ConfigApplyTxn<'_>,
        start: &DispatchStart,
    ) -> Result<Option<DateTime<Utc>>> {
        super::delivery::start_running_in_txn(txn, start, None).await
    }

    async fn publish_background_receipt_in_txn(
        txn: &ConfigApplyTxn<'_>,
        binding: &BackgroundReceiptBinding,
        receipt: &str,
    ) -> Result<bool> {
        super::delivery::publish_background_receipt_in_txn(txn, binding, receipt).await
    }

    async fn load_result_text(
        access: &ConfigAccess,
        tool_call_doc_id: &str,
        node_did: &str,
        session_id: &str,
        requester_did: Option<&str>,
    ) -> Result<Option<String>> {
        let read = super::query::load_tool_call_read(
            access,
            tool_call_doc_id,
            node_did,
            session_id,
            requester_did,
        )
        .await?;
        let Some(message) = read.result else {
            return Ok(None);
        };
        Ok(Some(super::query::render_tool_result(&message)?))
    }
}
