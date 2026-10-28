use anyhow::Result;
use chrono::Utc;
use gents_protocol::process_control::{ProcessStop, ProcessStopReceipt, PROCESS_STOP_VERSION};

use super::writes::ensure_peer_chat_ready_at;
use super::ClientCore;

impl ClientCore {
    pub async fn stop_background_process(
        &self,
        node_did: &str,
        session_id: &str,
        tool_call_id: &str,
        tool_call_doc_id: &str,
    ) -> Result<ProcessStopReceipt> {
        let peer = self
            .peer_record_for_chat_write(node_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(node_did, peer.as_ref(), Utc::now())?;
        let (signer, _, requester_did) = self.request_authority(node_did, peer.as_ref()).await?;
        let now = Utc::now();
        let command = ProcessStop {
            version: PROCESS_STOP_VERSION,
            command_id: uuid::Uuid::new_v4().to_string(),
            requester_did,
            requester_peer_id: (signer.did() != node_did).then(|| self.local_peer_id().to_owned()),
            node_did: node_did.to_owned(),
            session_id: session_id.to_owned(),
            tool_call_id: tool_call_id.to_owned(),
            tool_call_doc_id: tool_call_doc_id.to_owned(),
            issued_at: now.to_rfc3339(),
            expires_at: (now + chrono::Duration::minutes(5)).to_rfc3339(),
            signature: Vec::new(),
        };
        let command = gents::tool_control::prepare_process_stop(signer.as_ref(), command).await?;
        gents::tool_control::submit_prepared_process_stop(
            &self.session_queue_access(node_did),
            signer.as_ref(),
            &command,
        )
        .await
    }
}
