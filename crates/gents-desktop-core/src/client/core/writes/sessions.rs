use anyhow::{Context, Result};
use chrono::Utc;
use gents::store::config_client::ConfigAccess;

use super::super::bootstrap::normalize_required;
use super::{ensure_peer_chat_ready_at, ClientCore};
use crate::client::mutations;

fn mailbox_node_for_document<'a>(
    rows: &'a [gents_protocol::row::MailboxItemRow],
    doc_id: &str,
) -> Result<&'a str> {
    let mut matching = rows.iter().filter(|item| item.doc_id == doc_id);
    let item = matching.next().context("MailboxItem not found")?;
    anyhow::ensure!(
        matching.next().is_none(),
        "MailboxItem is ambiguous across runtimes"
    );
    Ok(&item.node_did)
}

impl ClientCore {
    pub async fn dismiss_mailbox_item(
        &self,
        doc_id: &str,
    ) -> Result<gents::session::mailbox::MailboxItem> {
        let snapshot = self.store.snapshot();
        let node_did = mailbox_node_for_document(&snapshot.mailbox_items, doc_id)?;
        let access = self.operator_access(node_did)?;
        let result = gents::session::mailbox::dismiss_mailbox_item_on(&access, doc_id).await;
        match result {
            Ok(item) => {
                if self.refresh_node(node_did).await?.is_none() {
                    self.refresh_store().await?;
                }
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
                target_agent_did: None,
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
        let requester = gents::session::client_requester_scope(
            &self.session_queue_access(node_did)?,
            node_did,
            session_id,
            self.node_identity.did(),
        )
        .await?;
        let result = mutations::rename_session(
            &self.session_queue_access(node_did)?,
            snapshot.as_ref(),
            node_did,
            &requester,
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

#[cfg(test)]
#[test]
fn mailbox_document_must_resolve_to_one_runtime() {
    let row: gents_protocol::row::MailboxItemRow = serde_json::from_value(serde_json::json!({
        "_docID":"item", "item_key":"key", "requester_did":"client", "node_did":"node-a",
        "status":"open", "kind":"flag", "action":"ack", "title":"title",
        "source_kind":"agent", "source_id":"source", "target_node_did":"node-a",
        "target_agent_did":"agent", "created_at":"now"
    }))
    .unwrap();
    assert_eq!(
        mailbox_node_for_document(std::slice::from_ref(&row), "item").unwrap(),
        "node-a"
    );
    let mut duplicate = row.clone();
    duplicate.node_did = "node-b".into();
    assert!(mailbox_node_for_document(&[row, duplicate], "item").is_err());
    assert!(mailbox_node_for_document(&[], "missing").is_err());
}
