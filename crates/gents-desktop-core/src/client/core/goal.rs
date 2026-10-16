use anyhow::{Context, Result};
use chrono::Utc;
use gents_protocol::goal_control::{
    GoalOperation, GoalRevision, SessionGoalControl, SessionGoalControlReceipt,
    SessionGoalObservation, SESSION_GOAL_CONTROL_VERSION,
};
use gents_protocol::request_input::{QueueDelivery, RequestInput};

use super::writes::ensure_peer_chat_ready_at;
use super::ClientCore;
use crate::client::mutations::{prepare_request_submission, SubmitRequestOptions};

impl ClientCore {
    pub async fn session_goal_observation(
        &self,
        node_did: &str,
        session_id: &str,
    ) -> Result<Option<SessionGoalObservation>> {
        let record = self
            .sync_state
            .records()
            .into_iter()
            .find(|record| record.node_did == node_did)
            .context("no saved deployment route owns this node")?;
        gents::goal::load_session_goal_observation(
            &self.session_queue_access(node_did),
            &self.node_identity,
            node_did,
            session_id,
            Some(&self.requester_did(&record)),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn control_session_goal(
        &self,
        node_did: &str,
        session_id: &str,
        session_doc_id: Option<String>,
        expected: Option<GoalRevision>,
        operation: GoalOperation,
        agent_id: Option<&str>,
        input: RequestInput,
    ) -> Result<SessionGoalControlReceipt> {
        let peer = self
            .peer_record_for_chat_write(node_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(node_did, peer.as_ref(), Utc::now())?;
        let (signer, admission, requester_did) =
            self.request_authority(node_did, peer.as_ref()).await?;
        let command_id = uuid::Uuid::new_v4().to_string();
        let initial_request = if let GoalOperation::Create { objective, .. } = &operation {
            let input = self
                .prepare_user_message_input_with_delivery(
                    node_did,
                    session_id,
                    input,
                    QueueDelivery::Queue,
                )
                .await?;
            let snapshot = self.store.snapshot();
            let (_, request) = prepare_request_submission(
                self.node.as_ref(),
                snapshot.as_ref(),
                session_id,
                node_did,
                &requester_did,
                signer.as_ref(),
                admission,
                objective,
                agent_id,
                SubmitRequestOptions {
                    input,
                    retry_key: Some(format!("session-goal:{command_id}")),
                    ..Default::default()
                },
                None,
            )
            .await?;
            Some(Box::new(request))
        } else {
            None
        };
        let now = Utc::now();
        let command = SessionGoalControl {
            version: SESSION_GOAL_CONTROL_VERSION,
            command_id,
            requester_did,
            requester_peer_id: (signer.did() != node_did).then(|| self.local_peer_id().to_owned()),
            node_did: node_did.to_owned(),
            session_id: session_id.to_owned(),
            session_doc_id,
            issued_at: now.to_rfc3339(),
            expires_at: (now + chrono::Duration::minutes(5)).to_rfc3339(),
            expected,
            operation,
            initial_request,
            signature: Vec::new(),
        };
        gents::goal::submit_session_goal_control(
            &self.session_queue_access(node_did),
            signer.as_ref(),
            command,
        )
        .await
    }
}
