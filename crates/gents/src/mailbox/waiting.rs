use chrono::{DateTime, Utc};
use gents_protocol::mailbox_question::MailboxQuestion;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::{AgentRequestRow, MailboxItemRow};

/// The caller supplies the canonical parent selected by the request owner.
/// This projection does not authenticate or select a competing request head.
pub fn awaiting_question_ids(
    items: &[MailboxItemRow],
    parent: &AgentRequestRow,
    now: DateTime<Utc>,
) -> Vec<String> {
    let mut ids = Vec::new();
    if parent.lifecycle_state != Some(RequestLifecycleState::Completed) {
        return ids;
    }
    for item in items {
        if item.kind != "ask" || item.action != "start_request" || item.status != "open" {
            continue;
        }
        let same_parent = item.request_id.as_deref() == Some(parent.request_id.as_str())
            && parent.node_did.as_deref() == Some(item.node_did.as_str())
            && parent.node_did.as_deref() == Some(item.target_node_did.as_str())
            && parent.requester_did.as_deref() == Some(item.requester_did.as_str())
            && item.session_id.is_some()
            && item.session_id == parent.session_id
            && parent.agent_id.as_deref() == Some(item.target_agent_id.as_str());
        if !same_parent {
            continue;
        }
        let Some(question) = item
            .payload
            .as_deref()
            .and_then(|payload| MailboxQuestion::from_payload(payload).ok())
        else {
            continue;
        };
        let is_open = item.status == "open"
            && item.deadline_at.as_deref().is_none_or(|deadline| {
                DateTime::parse_from_rfc3339(deadline).is_ok_and(|deadline| deadline > now)
            });
        if question.awaiting_answer(
            is_open,
            same_parent,
            parent.lifecycle_state == Some(RequestLifecycleState::Completed),
        ) {
            ids.push(item.doc_id.clone());
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

pub(crate) async fn required_question_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    parent: &AgentRequestRow,
) -> anyhow::Result<bool> {
    if parent.lifecycle_state != Some(RequestLifecycleState::Completed) {
        return Ok(false);
    }
    let query = format!(
        "{{ MailboxItem(filter: {{ node_did: {{_eq: \"{}\"}}, request_id: {{_eq: \"{}\"}}, status: {{_eq: \"open\"}} }}) {{ {} }} }}",
        crate::graphql::escape_graphql_string(parent.node_did.as_deref().unwrap_or_default()),
        crate::graphql::escape_graphql_string(&parent.request_id),
        super::MAILBOX_FIELDS,
    );
    let response = txn.execute_local_response(&query).await?;
    let items = crate::graphql::rows::<MailboxItemRow>(&response, super::MAILBOX_COLLECTION)?;
    Ok(!awaiting_question_ids(&items, parent, Utc::now()).is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn waiting_questions_follow_the_model_and_keep_the_exact_parent_scope() {
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().mailbox_question_cases;
        let now = "2030-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        for case in cases {
            let payload = json!({
                "version": case["version"], "prompt": "Choose a plan", "optional": case["optional"],
                "allow_free_text": case["free_text"], "options": (0..case["option_count"].as_u64().unwrap())
                    .map(|index| json!({"id":index.to_string(), "label":index.to_string()})).collect::<Vec<_>>(),
            }).to_string();
            for wait in case["waits"].as_array().unwrap() {
                let parent = AgentRequestRow {
                    doc_id: Some("physical-parent".into()),
                    request_id: "parent".into(),
                    node_did: Some("node".into()),
                    requester_did: Some("person".into()),
                    agent_id: Some("worker".into()),
                    session_id: Some("chat".into()),
                    lifecycle_state: Some(if wait["completed"] == true {
                        RequestLifecycleState::Completed
                    } else {
                        RequestLifecycleState::Interrupted
                    }),
                    ..Default::default()
                };
                let base = json!({
                    "_docID":"question", "item_key":"question", "requester_did":"person", "node_did":"node",
                    "status":if wait["open"] == true {"open"} else {"acted"}, "kind":"ask", "action":"start_request",
                    "title":"Plan", "payload":payload, "source_kind":"agent", "source_id":"source",
                    "session_id":"chat", "request_id":"parent", "target_node_did":"node", "target_agent_id":"worker",
                    "created_at":"2030-01-01T00:00:00Z"
                });
                for field in [
                    "node_did",
                    "requester_did",
                    "session_id",
                    "request_id",
                    "target_node_did",
                    "target_agent_id",
                ] {
                    let mut value = base.clone();
                    if wait["same_parent"] != true {
                        value[field] = Value::String("other".into());
                    }
                    let item: MailboxItemRow = serde_json::from_value(value).unwrap();
                    assert_eq!(
                        !awaiting_question_ids(&[item], &parent, now).is_empty(),
                        wait["waiting"] == true,
                        "{case}: {wait}: {field}"
                    );
                }
                let mut expired = base;
                expired["deadline_at"] = json!("2020-01-01T00:00:00Z");
                let item = serde_json::from_value(expired).unwrap();
                assert!(awaiting_question_ids(&[item], &parent, now).is_empty());
            }
        }
    }
}
