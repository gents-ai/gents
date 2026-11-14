use anyhow::{bail, Context, Result};
use gents_desktop_core::client::ClientCore;

use super::super::types::{MailboxItemObservation, MailboxItemObservationRequest, MailboxItemView};

pub async fn observe_mailbox_item(
    core: &ClientCore,
    request: &MailboxItemObservationRequest,
) -> Result<Option<MailboxItemObservation>> {
    let requester = core.node_identity().did();
    anyhow::ensure!(
        request.client_did == requester,
        "the mailbox reader changed"
    );
    anyhow::ensure!(
        !request.item_id.trim().is_empty(),
        "a mailbox item is required"
    );
    use gents_protocol::operations::mailbox::MailboxOperationError;
    let item = gents_session::operations::mailbox::get(
        &core.operator_access(&request.node_did)?,
        &request.item_id,
    )
    .await;
    match item {
        Ok(item) => Some(item),
        Err(MailboxOperationError::ItemMissing) => None,
        Err(error) => return Err(gents_desktop_core::error::mailbox_operation_failure(error)),
    }
    .map(|item| {
        anyhow::ensure!(
            item.node_did == request.node_did,
            "mailbox item belongs to another runtime"
        );
        mailbox_observation(&item)
    })
    .transpose()
}

fn mailbox_observation(
    item: &gents_session::session::mailbox::MailboxItem,
) -> Result<MailboxItemObservation> {
    let terminal = item
        .parsed_status()
        .context("unknown MailboxItem status")?
        .is_terminal();
    let row: gents_protocol::row::MailboxItemRow =
        serde_json::from_value(serde_json::to_value(item)?)?;
    Ok(MailboxItemObservation {
        item: MailboxItemView::from(&row),
        terminal,
    })
}

/// The open item `item_id` as `node_did`'s own store lists it.
pub fn start_mailbox_request(
    core: &ClientCore,
    node_did: &str,
    item_id: &str,
) -> Result<MailboxItemView> {
    let snapshot = core.stores().snapshot(node_did);
    let row = snapshot
        .mailbox_items
        .iter()
        .find(|row| row.doc_id == item_id)
        .context("MailboxItem not found")?;
    if row.status != "open" {
        bail!("MailboxItem is no longer open");
    }
    if !matches!(row.action.as_str(), "start_request" | "write_document") {
        bail!("MailboxItem action does not open a compose surface");
    }
    Ok(MailboxItemView::from(row))
}

pub(crate) fn question_reply_content(
    core: &ClientCore,
    item_id: Option<&str>,
    session_id: Option<&str>,
    node_did: &str,
    agent_did: Option<&str>,
    answer: &gents_protocol::mailbox_question::MailboxQuestionAnswer,
) -> Result<gents_protocol::operations::mailbox::PreparedMailboxReply> {
    use gents_protocol::operations::mailbox::{MailboxReply, MailboxReplyRoute};
    let item_id = item_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .context("an answer requires causedBySourceDocId")?;
    let snapshot = core.stores().snapshot(node_did);
    let row = snapshot
        .mailbox_items
        .iter()
        .find(|row| row.doc_id == item_id)
        .context("MailboxItem not found")?;
    gents_session::operations::mailbox::prepare_reply(
        row.into(),
        MailboxReply::Answer(answer.clone()),
        &MailboxReplyRoute {
            session_id: Some(session_id.unwrap_or_default().to_owned()),
            node_did: Some(node_did.to_owned()),
            agent_did: agent_did
                .map(str::trim)
                .filter(|agent| !agent.is_empty())
                .map(ToOwned::to_owned),
        },
    )
    .map_err(gents_desktop_core::error::mailbox_operation_failure)
}

pub async fn dismiss_mailbox(core: &ClientCore, node_did: &str, item_id: &str) -> Result<()> {
    core.dismiss_mailbox_item(node_did, item_id)
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_observation_preserves_closed_items_and_requester_provenance() {
        let mut item: gents_session::session::mailbox::MailboxItem =
            serde_json::from_value(serde_json::json!({
                "_docID": "question", "item_key": "key", "requester_did": "reader",
                "node_did": "node", "status": "open", "kind": "ask",
                "action": "start_request", "title": "Question", "source_kind": "session",
                "source_id": "session", "session_id": "session", "target_node_did": "node",
                "target_agent_did": "agent", "created_at": "2026-10-28T00:00:00Z"
            }))
            .unwrap();
        for status in gents_session::session::mailbox::MailboxStatus::ALL {
            item.status = status.as_str().to_owned();
            let observation = mailbox_observation(&item).unwrap();
            assert_eq!(observation.item.item_id, "question");
            assert_eq!(observation.item.status, status.as_str());
            assert_eq!(observation.item.session_id.as_deref(), Some("session"));
            assert_eq!(observation.terminal, status.is_terminal());
            assert_eq!(observation.item.requester_did, "reader");
        }
        item.status = "unrecognized".into();
        assert!(mailbox_observation(&item).is_err());
        assert!(
            serde_json::from_value::<MailboxItemObservationRequest>(serde_json::json!({
                "clientDid": "reader", "nodeDid": "node", "itemId": "question", "requesterDid": "another-reader"
            }))
            .is_err()
        );
    }
}
