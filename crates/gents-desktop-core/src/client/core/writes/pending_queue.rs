use anyhow::{Context, Result};
use chrono::Utc;
use gents::store::config_client::ConfigAccess;

use super::{ensure_peer_chat_ready_at, ClientCore};
use crate::client::observe::ObservedStore;
use crate::client::store::{ClientStore, ClientStoreRows};

#[cfg(test)]
mod tests;

/// A validated applied receipt is the command result. The local projection is
/// a separate replica observation; if it is unavailable, the selected-session
/// refresh and normal observer path reconcile it without inviting replay.
async fn merge_accepted_edit_projection(
    access: &ConfigAccess,
    store: &ObservedStore,
    receipt: gents_protocol::session_input_edit::SessionInputEditReceipt,
    selected_doc_ids: &[String],
    node_did: &str,
    session_id: &str,
    requester_did: &str,
) -> gents_protocol::session_input_edit::SessionInputEditReceipt {
    let result = async {
        let mut doc_ids = selected_doc_ids.to_vec();
        doc_ids.extend(receipt.request_doc_ids.iter().cloned());
        let mut unique =
            rapidhash::RapidHashSet::with_capacity_and_hasher(doc_ids.len(), Default::default());
        doc_ids.retain(|doc_id| unique.insert(doc_id.clone()));
        if doc_ids.is_empty() {
            return anyhow::Result::<()>::Ok(());
        }
        let captured = store.capture_read();
        let rows = crate::client::load_request_documents_on(access, &doc_ids).await?;
        for row in &rows {
            anyhow::ensure!(
                row.node_did.as_deref() == Some(node_did)
                    && row.session_id.as_deref() == Some(session_id)
                    && row.requester_did.as_deref() == Some(requester_did),
                "pending edit request patch crossed its node, session, or requester scope"
            );
        }
        let patch = ClientStore::from_rows(ClientStoreRows {
            requests: rows,
            ..ClientStoreRows::default()
        });
        if store.merge_chat_patch_at_read(&captured, patch).is_none() {
            store.request_resync(Some(node_did));
            anyhow::bail!("accepted edit projection read became stale before publication");
        }
        anyhow::Result::<()>::Ok(())
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(
            command_id = %receipt.command_id,
            node_did,
            session_id,
            error = %error,
            "accepted pending edit awaits local projection refresh"
        );
    }
    receipt
}

fn normalize_pending_edit_skills(edit: &mut gents::request::lifecycle::PendingQueueEdit) {
    for message in &mut edit.messages {
        if let Some(authored) = &mut message.authored_input {
            let selection =
                gents::documents::skills::prompt_slash_skill_selection(&message.content);
            if !selection.selected_skill_ids.is_empty() {
                authored.selected_skill_ids = selection.selected_skill_ids;
                message.content = selection.prompt;
            }
        }
    }
}

impl ClientCore {
    pub async fn prepare_user_message_input(
        &self,
        node_did: &str,
        session_id: &str,
        input: gents_protocol::request_input::RequestInput,
    ) -> Result<gents_protocol::request_input::RequestInput> {
        self.prepare_user_message_input_with_delivery(
            node_did,
            session_id,
            input,
            gents_protocol::request_input::QueueDelivery::Steer,
        )
        .await
    }

    pub async fn prepare_user_message_input_with_delivery(
        &self,
        node_did: &str,
        session_id: &str,
        input: gents_protocol::request_input::RequestInput,
        delivery: gents_protocol::request_input::QueueDelivery,
    ) -> Result<gents_protocol::request_input::RequestInput> {
        let input = self.prepare_goal_input(node_did, session_id, input).await?;
        gents::request::lifecycle::prepare_user_message_input(
            &self
                .operator_observation_graphql(node_did)
                .map(ConfigAccess::Graphql)
                .unwrap_or_else(|| ConfigAccess::local(self.node_arc())),
            node_did,
            session_id,
            input,
            delivery,
        )
        .await
    }

    pub async fn pending_user_queue(
        &self,
        node_did: &str,
        session_id: &str,
    ) -> Result<gents::request::lifecycle::PendingQueueSnapshot> {
        Ok(self
            .pending_user_queue_projection(node_did, session_id, &[])
            .await?
            .pending)
    }

    pub async fn pending_user_queue_projection(
        &self,
        node_did: &str,
        session_id: &str,
        observed_doc_ids: &[String],
    ) -> Result<gents::request::lifecycle::PendingQueueProjection> {
        let record = self
            .sync_state
            .records()
            .into_iter()
            .find(|record| record.node_did == node_did)
            .context("no saved deployment route owns this node")?;
        gents::request::lifecycle::pending_user_queue_projection(
            &self
                .operator_observation_graphql(node_did)
                .map(ConfigAccess::Graphql)
                .unwrap_or_else(|| ConfigAccess::local(self.node_arc())),
            node_did,
            session_id,
            &self.requester_did(&record),
            observed_doc_ids,
        )
        .await
    }

    pub async fn edit_pending_queue(
        &self,
        node_did: &str,
        session_id: &str,
        mut edit: gents::request::lifecycle::PendingQueueEdit,
    ) -> Result<gents_protocol::session_input_edit::SessionInputEditReceipt> {
        normalize_pending_edit_skills(&mut edit);
        self.apply_pending_queue_edit(node_did, session_id, edit, None, None)
            .await
    }

    pub async fn steer_pending_message(
        &self,
        node_did: &str,
        session_id: &str,
        expected_request_doc_ids: Vec<String>,
        request_doc_id: &str,
        request_id: Option<&str>,
    ) -> Result<gents_protocol::session_input_edit::SessionInputEditReceipt> {
        let snapshot = self.pending_user_queue(node_did, session_id).await?;
        let edit = gents::request::lifecycle::pending_steer_edit(
            &snapshot,
            expected_request_doc_ids,
            request_doc_id,
        )?;
        self.apply_pending_queue_edit(node_did, session_id, edit, Some(request_doc_id), request_id)
            .await
    }

    async fn apply_pending_queue_edit(
        &self,
        node_did: &str,
        session_id: &str,
        edit: gents::request::lifecycle::PendingQueueEdit,
        steer: Option<&str>,
        request_id: Option<&str>,
    ) -> Result<gents_protocol::session_input_edit::SessionInputEditReceipt> {
        let peer = self
            .peer_record_for_chat_write(node_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(node_did, peer.as_ref(), Utc::now())?;
        if edit
            .messages
            .iter()
            .any(|message| message.authored_input.is_some())
        {
            crate::local_runtime::ensure_authored_queue_edit_supported(peer.as_ref()).await?;
        }
        let (signer, admission, _) = self.request_authority(node_did, peer.as_ref()).await?;
        let selected_doc_ids = edit.selected_request_doc_ids.clone();
        let access = self.session_queue_access(node_did);
        let result = match steer {
            Some(request_doc_id) => {
                gents::request::lifecycle::submit_pending_user_steer(
                    &access,
                    signer.as_ref(),
                    admission,
                    Some(self.local_peer_id()),
                    node_did,
                    session_id,
                    edit,
                    request_doc_id,
                    request_id,
                )
                .await
            }
            None => {
                gents::request::lifecycle::submit_pending_user_edit(
                    &access,
                    signer.as_ref(),
                    admission,
                    Some(self.local_peer_id()),
                    node_did,
                    session_id,
                    edit,
                )
                .await
            }
        };
        match result {
            Ok(receipt) => {
                let requester_did = peer
                    .as_ref()
                    .map(|record| self.requester_did(record))
                    .unwrap_or_else(|| self.node_identity.did().to_string());
                let receipt = merge_accepted_edit_projection(
                    &self.session_queue_access(node_did),
                    self.store.as_ref(),
                    receipt,
                    &selected_doc_ids,
                    node_did,
                    session_id,
                    &requester_did,
                )
                .await;
                self.clear_mutation_error();
                Ok(receipt)
            }
            Err(error) => Err(self.record_mutation_error("edit pending messages", error)),
        }
    }
}
