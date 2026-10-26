use super::util::Result;
use gents_desktop_core::client::ClientCore;

use super::super::types::MailboxItemView;
use super::util::BridgeCommandError;

pub fn list_mailbox(core: &ClientCore) -> Vec<MailboxItemView> {
    let requester = core.node_identity().did();
    core.store()
        .snapshot()
        .mailbox_items
        .iter()
        .filter(|row| row.requester_did == requester && row.status == "open")
        .map(MailboxItemView::from)
        .collect()
}

pub fn start_mailbox_request(
    core: &ClientCore,
    item_id: &str,
) -> Result<MailboxItemView, BridgeCommandError> {
    let requester = core.node_identity().did();
    let snapshot = core.store().snapshot();
    let row = snapshot
        .mailbox_items
        .iter()
        .find(|row| row.doc_id == item_id)
        .ok_or(BridgeCommandError::MailboxItemNotFound)?;
    if row.requester_did != requester {
        return Err(BridgeCommandError::MailboxRequesterMismatch);
    }
    if row.status != "open" {
        return Err(BridgeCommandError::MailboxItemClosed);
    }
    if !matches!(row.action.as_str(), "start_request" | "write_document") {
        return Err(BridgeCommandError::MailboxActionNotComposable);
    }
    Ok(MailboxItemView::from(row))
}

pub(crate) fn question_reply_content(
    core: &ClientCore,
    item_id: Option<&str>,
    session_id: Option<&str>,
    node_did: &str,
    answer: &gents_protocol::mailbox_question::MailboxQuestionAnswer,
) -> Result<String, BridgeCommandError> {
    let item_id = item_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or(BridgeCommandError::AnswerRequiresItem)?;
    let item = start_mailbox_request(core, item_id)?;
    question_reply_for_item(&item, session_id, node_did, answer)
}

fn question_reply_for_item(
    item: &MailboxItemView,
    session_id: Option<&str>,
    node_did: &str,
    answer: &gents_protocol::mailbox_question::MailboxQuestionAnswer,
) -> Result<String, BridgeCommandError> {
    if item.kind != "ask" || item.action != "start_request" {
        return Err(BridgeCommandError::MailboxItemNotQuestion);
    }
    if item.session_id.is_none() || item.session_id.as_deref() != session_id.map(str::trim) {
        return Err(BridgeCommandError::AnswerSessionMismatch);
    }
    if item.target_node_did != node_did {
        return Err(BridgeCommandError::AnswerAgentMismatch);
    }
    let question = gents_protocol::mailbox_question::MailboxQuestion::from_payload(
        item.payload.as_deref().unwrap_or_default(),
    )?;
    Ok(question.reply_content(&item.title, answer)?)
}

pub async fn dismiss_mailbox(core: &ClientCore, item_id: &str) -> Result<()> {
    core.dismiss_mailbox_item(item_id).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::mailbox_question::{
        MailboxQuestion, MailboxQuestionAnswer, MailboxQuestionOption, MAILBOX_QUESTION_VERSION,
    };

    fn question_item() -> MailboxItemView {
        let question = MailboxQuestion {
            version: MAILBOX_QUESTION_VERSION,
            prompt: "Ship it?".into(),
            options: ["yes", "no"]
                .map(|id| MailboxQuestionOption {
                    id: id.into(),
                    label: id.to_uppercase(),
                    description: None,
                })
                .to_vec(),
            multi_select: false,
            allow_free_text: false,
        };
        let row: gents_protocol::row::MailboxItemRow = serde_json::from_value(serde_json::json!({
            "_docID": "item-1", "item_key": "k", "requester_did": "did:person",
            "node_did": "did:agent", "status": "open", "kind": "ask",
            "action": "start_request", "title": "Release",
            "payload": serde_json::to_string(&question).unwrap(),
            "source_kind": "agent", "source_id": "s", "session_id": "session-1",
            "target_node_did": "did:agent", "target_agent_id": "engineer",
            "created_at": "2026-09-29T00:00:00Z"
        }))
        .unwrap();
        MailboxItemView::from(&row)
    }

    #[test]
    fn answer_renders_from_the_question_and_addresses_its_session() {
        let item = question_item();
        let answer = MailboxQuestionAnswer {
            option_ids: vec!["yes".into()],
            free_text: None,
        };
        assert_eq!(
            question_reply_for_item(&item, Some("session-1"), "did:agent", &answer).unwrap(),
            "Decision on Release: YES (yes)"
        );
        assert!(question_reply_for_item(&item, Some("other"), "did:agent", &answer).is_err());
        assert!(question_reply_for_item(&item, None, "did:agent", &answer).is_err());
        assert!(question_reply_for_item(&item, Some("session-1"), "did:other", &answer).is_err());
        let unknown = MailboxQuestionAnswer {
            option_ids: vec!["maybe".into()],
            free_text: None,
        };
        assert!(question_reply_for_item(&item, Some("session-1"), "did:agent", &unknown).is_err());
        let mut flag = item.clone();
        flag.kind = "flag".into();
        assert!(question_reply_for_item(&flag, Some("session-1"), "did:agent", &answer).is_err());
    }
}
