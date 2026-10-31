use anyhow::{Context, Result};
use chrono::Utc;
use gents::store::config_client::ConfigAccess;

use super::super::bootstrap::normalize_required;
use super::{ensure_peer_chat_ready_at, ClientCore};
use crate::client::mutations;

impl ClientCore {
    pub async fn dismiss_mailbox_item(
        &self,
        doc_id: &str,
    ) -> Result<gents::session::mailbox::MailboxItem> {
        let result = gents::session::mailbox::dismiss_mailbox_item(
            &self.node,
            doc_id,
            self.node_identity.did(),
        )
        .await;
        match result {
            Ok(item) => {
                self.refresh_store().await?;
                self.clear_mutation_error();
                Ok(item)
            }
            Err(error) => Err(self.record_mutation_error("dismiss mailbox item", error)),
        }
    }

    /// A client replica cannot establish an idle source or complete history;
    /// editing requires the runtime's authenticated transaction endpoint.
    pub async fn fork_session_before_message(
        &self,
        node_did: &str,
        session_id: &str,
        message_key: &str,
        client_did: &str,
        child_session_id: &str,
    ) -> Result<String> {
        anyhow::ensure!(
            client_did == self.node_identity.did(),
            "the saved edit belongs to another native client"
        );
        let node_did = normalize_required("node_did", node_did)?;
        let session_id = normalize_required("session_id", session_id)?;
        anyhow::ensure!(
            child_session_id == child_session_id.trim(),
            "child_session_id must retain its exact canonical identity"
        );
        let child_session_id = normalize_required("child_session_id", child_session_id)?;
        anyhow::ensure!(!message_key.trim().is_empty(), "message_key is required");
        let peer = self
            .peer_record_for_chat_write(node_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(node_did, peer.as_ref(), Utc::now())?;
        let requester = self.requester_did(
            peer.as_ref()
                .context("chat target has no current owned peer route")?,
        );
        let access = self.operator_graphql(node_did)
            .map(ConfigAccess::Graphql)
            .context("Editing a message requires a direct runtime connection. Reconnect the local runtime and try again.")?;
        gents::session::fork_edit_on(
            &access,
            gents::session::ForkParams {
                source_session_id: session_id,
                boundary: gents::session::ForkBoundary::BeforeMessageKey(message_key),
                caller_node_did: node_did,
                caller_requester_did: Some(&requester),
                target_agent_id: None,
            },
            child_session_id,
            client_did,
        )
        .await
        .map_err(Into::into)
    }

    pub async fn set_session_archived(
        &self,
        node_did: &str,
        session_id: &str,
        expected_revision: u64,
        archived: bool,
    ) -> Result<gents_protocol::session::SessionArchiveReceipt> {
        let access = self.operator_access(node_did)?;
        let result = gents::session::set_session_archived_on(
            &access,
            node_did,
            session_id,
            &self.node_identity,
            expected_revision,
            archived,
        )
        .await;
        match result {
            Ok(receipt) => {
                if self.refresh_node(node_did).await?.is_none() {
                    self.refresh_store().await?;
                }
                self.clear_mutation_error();
                Ok(receipt)
            }
            Err(error) => Err(self.record_mutation_error(
                if archived {
                    "archive session"
                } else {
                    "restore session"
                },
                error,
            )),
        }
    }

    pub async fn rename_session(
        &self,
        node_did: &str,
        session_id: &str,
        title: &str,
    ) -> Result<()> {
        let snapshot = self.store.snapshot();
        let result = mutations::rename_session(
            &self.node,
            snapshot.as_ref(),
            node_did,
            self.node_identity.did(),
            session_id,
            title,
        )
        .await;
        match result {
            Ok(()) => {
                if self.refresh_node(node_did).await?.is_none() {
                    self.refresh_store().await?;
                }
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    action = "chat_rename",
                    row_id = %session_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("rename session", error)),
        }
    }
}
