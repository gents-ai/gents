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
    gents::mailbox::load_mailbox_item(core.node(), &request.item_id)
        .await?
        .map(|item| mailbox_observation(&requester, &item))
        .transpose()
}

fn mailbox_observation(
    requester: &str,
    item: &gents::mailbox::MailboxItem,
) -> Result<MailboxItemObservation> {
    anyhow::ensure!(
        item.requester_did == requester,
        "only requester_did may read a MailboxItem"
    );
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

pub fn start_mailbox_request(core: &ClientCore, item_id: &str) -> Result<MailboxItemView> {
    let requester = core.node_identity().did();
    let snapshot = core.store().snapshot();
    let row = snapshot
        .mailbox_items
        .iter()
        .find(|row| row.doc_id == item_id)
        .context("MailboxItem not found")?;
    if row.requester_did != requester {
        bail!("only requester_did may act on a MailboxItem");
    }
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
    agent_id: Option<&str>,
    answer: &gents_protocol::mailbox_question::MailboxQuestionAnswer,
) -> Result<(
    String,
    Option<gents_protocol::mailbox_question::MailboxQuestionReply>,
)> {
    let item_id = item_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .context("an answer requires causedBySourceDocId")?;
    let item = start_mailbox_request(core, item_id)?;
    if gents_protocol::mailbox_question::question_requires_reply_receipt(
        item.payload.as_deref().unwrap_or_default(),
    ) {
        anyhow::ensure!(
            agent_id == Some(item.target_agent_id.as_str()),
            "an answer must name the agent that asked the question"
        );
    }
    let content = question_reply_for_item(&item, session_id, node_did, answer)?;
    let payload = item
        .payload
        .as_deref()
        .context("question payload is unavailable")?;
    let evidence = if gents_protocol::mailbox_question::question_requires_reply_receipt(payload) {
        Some(gents_protocol::mailbox_question::MailboxQuestionReply::new(
            &item.item_id,
            &item.title,
            item.summary.as_deref(),
            payload,
            item.request_id.as_deref(),
            answer.clone(),
        )?)
    } else {
        None
    };
    Ok((content, evidence))
}

fn question_reply_for_item(
    item: &MailboxItemView,
    session_id: Option<&str>,
    node_did: &str,
    answer: &gents_protocol::mailbox_question::MailboxQuestionAnswer,
) -> Result<String> {
    if item.kind != "ask" || item.action != "start_request" {
        bail!("MailboxItem is not a question");
    }
    if item.session_id.is_none() || item.session_id.as_deref() != session_id.map(str::trim) {
        bail!("an answer must be sent to the question's session");
    }
    if item.target_node_did != node_did {
        bail!("an answer must be sent to the agent that asked");
    }
    let question = gents_protocol::mailbox_question::MailboxQuestion::from_payload(
        item.payload.as_deref().unwrap_or_default(),
    )?;
    if question.version >= 2 {
        anyhow::ensure!(
            answer.question_hash.is_some() && answer.question_hash == item.question_hash,
            "the question changed; reload its current prompt before replying"
        );
    }
    Ok(question.reply_content(&item.title, answer)?)
}

pub async fn dismiss_mailbox(core: &ClientCore, item_id: &str) -> Result<()> {
    core.dismiss_mailbox_item(item_id).await.map(|_| ())
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
                    prompt: None,
                })
                .to_vec(),
            multi_select: false,
            allow_free_text: false,
            optional: None,
            recommended_order: None,
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
    fn exact_observation_preserves_closed_items_and_rejects_foreign_readers() {
        let mut item: gents::mailbox::MailboxItem = serde_json::from_value(serde_json::json!({
            "_docID": "question", "item_key": "key", "requester_did": "reader",
            "node_did": "node", "status": "open", "kind": "ask",
            "action": "start_request", "title": "Question", "source_kind": "session",
            "source_id": "session", "session_id": "session", "target_node_did": "node",
            "target_agent_id": "agent", "created_at": "2026-10-28T00:00:00Z"
        }))
        .unwrap();
        for status in gents::mailbox::MailboxStatus::ALL {
            item.status = status.as_str().to_owned();
            let observation = mailbox_observation("reader", &item).unwrap();
            assert_eq!(observation.item.item_id, "question");
            assert_eq!(observation.item.status, status.as_str());
            assert_eq!(observation.item.session_id.as_deref(), Some("session"));
            assert_eq!(observation.terminal, status.is_terminal());
            assert!(mailbox_observation("another-reader", &item).is_err());
        }
        item.status = "unrecognized".into();
        assert!(mailbox_observation("reader", &item).is_err());
        assert!(
            serde_json::from_value::<MailboxItemObservationRequest>(serde_json::json!({
                "clientDid": "reader", "itemId": "question", "requesterDid": "another-reader"
            }))
            .is_err()
        );
    }

    #[test]
    fn answer_renders_from_the_question_and_addresses_its_session() {
        let item = question_item();
        let answer = MailboxQuestionAnswer {
            option_ids: vec!["yes".into()],
            free_text: None,
            skip: None,
            question_hash: item.question_hash.clone(),
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
            skip: None,
            question_hash: item.question_hash.clone(),
        };
        assert!(question_reply_for_item(&item, Some("session-1"), "did:agent", &unknown).is_err());
        let mut flag = item.clone();
        flag.kind = "flag".into();
        assert!(question_reply_for_item(&flag, Some("session-1"), "did:agent", &answer).is_err());
    }

    #[test]
    fn skip_is_optional_and_keeps_the_original_question_scope() {
        let mut item = question_item();
        let mut skip = MailboxQuestionAnswer {
            question_hash: item.question_hash.clone(),
            skip: Some(true),
            ..Default::default()
        };
        assert!(question_reply_for_item(&item, Some("session-1"), "did:agent", &skip).is_err());
        let mut question = MailboxQuestion::from_payload(item.payload.as_deref().unwrap()).unwrap();
        question.optional = Some(true);
        item.payload = Some(serde_json::to_string(&question).unwrap());
        item.question_hash = Some(
            gents_protocol::mailbox_question::question_source_hash(
                &item.title,
                item.summary.as_deref(),
                item.payload.as_deref().unwrap(),
                item.request_id.as_deref(),
            )
            .unwrap(),
        );
        assert!(question_reply_for_item(&item, Some("session-1"), "did:agent", &skip).is_err());
        skip.question_hash = item.question_hash.clone();
        assert_eq!(
            question_reply_for_item(&item, Some("session-1"), "did:agent", &skip).unwrap(),
            "Skipped optional decision on Release."
        );
        assert!(question_reply_for_item(&item, Some("other"), "did:agent", &skip).is_err());
        assert!(question_reply_for_item(&item, Some("session-1"), "did:other", &skip).is_err());
    }
}
