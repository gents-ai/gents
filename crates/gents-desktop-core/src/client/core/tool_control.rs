use anyhow::Result;
use gents_protocol::process_control::ToolControlReceipt;

use super::ClientCore;

impl ClientCore {
    pub async fn tool_control_receipt(
        &self,
        node_did: &str,
        intent_doc_id: &str,
        tool_call_doc_id: &str,
        action: &str,
    ) -> Result<Option<ToolControlReceipt>> {
        Ok(gents::tools::call::control::client::read_receipt(
            &self.operator_access(node_did)?,
            node_did,
            intent_doc_id,
            tool_call_doc_id,
            action,
        )
        .await?)
    }
}
