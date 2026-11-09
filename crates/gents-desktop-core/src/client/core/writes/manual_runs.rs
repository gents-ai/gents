use anyhow::Result;
use chrono::Utc;
use gents::request::lifecycle::TriggerLineage;
use gents_protocol::request_input::RequestInput;
use gents_protocol::session::{SessionTitle, SessionTitleSource};

use super::{
    agent_did_for_write, ensure_peer_chat_ready, observe_submitted_request, ClientCore,
};
use crate::client::mutations::{self, SubmitRequestOptions, SubmittedRequest};

impl ClientCore {
    pub async fn fire_task_now_for_node(
        &self,
        node_did: &str,
        task_id: &str,
        args: serde_json::Value,
    ) -> Result<SubmittedRequest> {
        let access = self.operator_access(node_did)?;
        let invocation = mutations::resolve_task_now_on(&access, node_did, task_id, args)
            .await
            .map_err(|error| self.record_mutation_error("resolve task", error))?;
        self.submit_manual_task_invocation(invocation).await
    }

    pub async fn fire_schedule_now_for_node(
        &self,
        node_did: &str,
        schedule_id: &str,
    ) -> Result<SubmittedRequest> {
        let access = self.operator_access(node_did)?;
        let invocation = mutations::resolve_schedule_now_on(&access, node_did, schedule_id)
            .await
            .map_err(|error| self.record_mutation_error("resolve schedule", error))?;
        self.submit_manual_task_invocation(invocation).await
    }

    async fn submit_manual_task_invocation(
        &self,
        invocation: mutations::ManualTaskInvocation,
    ) -> Result<SubmittedRequest> {
        let snapshot = self.store.snapshot();
        let peer_record = self
            .peer_record_for_chat_write(&invocation.node_did, Utc::now())
            .await?;
        ensure_peer_chat_ready(&invocation.node_did, peer_record.as_ref())?;
        let (signer, admission, requester_did) = self
            .request_authority(&invocation.node_did, peer_record.as_ref())
            .await?;
        let options = SubmitRequestOptions {
            input: RequestInput {
                initial_title: Some(SessionTitle {
                    text: invocation.session_title.clone(),
                    source: SessionTitleSource::Task,
                }),
                ..RequestInput::default()
            },
            trigger_lineage: TriggerLineage {
                trigger_kind: Some("manual".to_string()),
                ..TriggerLineage::default()
            },
            ..SubmitRequestOptions::default()
        };
        let agent_did = agent_did_for_write(Some(&invocation.agent_did));
        let access = self.operator_access(&invocation.node_did)?;
        let submitted = mutations::submit_task_request(
            snapshot.as_ref(),
            &access,
            &invocation.fire,
            &requester_did,
            signer.as_ref(),
            admission,
            &invocation.content,
            agent_did.as_deref(),
            options,
        )
        .await;
        match submitted {
            Ok(result) => {
                let result =
                    observe_submitted_request(&self.store, result, self.refresh_store()).await;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "manual_run",
                    node_did = %invocation.node_did,
                    task_id = %invocation.task_id,
                    request_id = %result.request_id,
                    requester_did,
                    "desktop canonical manual task run submitted through client authority"
                );
                Ok(result)
            }
            Err(error) => Err(self.record_mutation_error("fire task", error)),
        }
    }
}
