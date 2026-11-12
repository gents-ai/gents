use anyhow::{Context, Result};
use gents_protocol::operations::session::{
    SessionArchiveInput, SessionForkBoundary, SessionForkInput, SessionRef, SessionRenameInput,
};
use gents_store::config_client::ConfigAccess;

use super::{ensure_peer_chat_ready, ClientCore};
use crate::error::session_operation_failure;

impl ClientCore {
    /// Dismisses the mailbox item `doc_id` on `node_did`, the node whose own
    /// store lists it.
    pub async fn dismiss_mailbox_item(
        &self,
        node_did: &str,
        doc_id: &str,
    ) -> Result<gents_session::session::mailbox::MailboxItem> {
        let access = self.operator_access(node_did)?;
        let result = gents_session::operations::mailbox::dismiss(&access, doc_id)
            .await
            .map_err(crate::error::mailbox_operation_failure);
        match result {
            Ok(item) => {
                self.refresh_node(node_did).await?;
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
        let node_did = node_did.trim();
        let peer = self.peer_record_for_chat_write(node_did)?;
        ensure_peer_chat_ready(node_did, peer.as_ref())?;
        let access = self.operator_graphql(node_did)
            .map(ConfigAccess::Graphql)
            .context("Editing a message requires a direct runtime connection. Reconnect the local runtime and try again.")?;
        gents_session::operations::session::fork(
            &access,
            &SessionForkInput {
                source: SessionRef {
                    node_did: node_did.to_owned(),
                    session_id: session_id.trim().to_owned(),
                    requester_did: Some(self.node_identity.did().to_owned()),
                },
                boundary: SessionForkBoundary::BeforeMessage {
                    message_key: message_key.to_owned(),
                    child_session_id: child_session_id.to_owned(),
                    client_did: client_did.to_owned(),
                },
                target_agent_did: None,
            },
        )
        .await
        .map(|receipt| receipt.session_id)
        .map_err(session_operation_failure)
    }

    pub async fn set_session_archived(
        &self,
        node_did: &str,
        session_id: &str,
        expected_revision: u64,
        archived: bool,
    ) -> Result<gents_protocol::session::SessionArchiveReceipt> {
        let access = self.operator_access(node_did)?;
        let result = gents_session::operations::session::set_archived(
            &access,
            &self.node_identity,
            &SessionArchiveInput {
                session: SessionRef {
                    node_did: node_did.to_owned(),
                    session_id: session_id.to_owned(),
                    requester_did: None,
                },
                expected_revision: Some(expected_revision),
                archived,
            },
        )
        .await;
        match result {
            Ok(receipt) => {
                self.refresh_node(node_did).await?;
                self.clear_mutation_error();
                Ok(receipt)
            }
            Err(error) => Err(self.record_mutation_error(
                if archived {
                    "archive session"
                } else {
                    "restore session"
                },
                session_operation_failure(error),
            )),
        }
    }

    pub async fn rename_session(
        &self,
        node_did: &str,
        session_id: &str,
        title: &str,
    ) -> Result<()> {
        let result = gents_session::operations::session::rename(
            &self.session_queue_access(node_did)?,
            &SessionRenameInput {
                session: SessionRef {
                    node_did: node_did.to_owned(),
                    session_id: session_id.to_owned(),
                    requester_did: None,
                },
                title: title.to_owned(),
            },
        )
        .await;
        match result {
            Ok(_) => {
                self.refresh_node(node_did).await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    action = "chat_rename",
                    row_id = %session_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => {
                Err(self.record_mutation_error("rename session", session_operation_failure(error)))
            }
        }
    }
}
