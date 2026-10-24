use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use serde_json::Value;

use crate::agent::loop_stream::TaggedMessage;
use crate::config_client::ConfigAccess;
use crate::graphql::{escape_graphql_string, graphql_string_list_literal};

fn current_keys(claimed: &[String], acknowledged: &[String]) -> Vec<String> {
    let acknowledged = acknowledged.iter().collect::<BTreeSet<_>>();
    claimed
        .iter()
        .filter(|key| !acknowledged.contains(key))
        .cloned()
        .collect()
}

/// Lean `CurrentInput.wakeEntry`. The caller persists only its authored
/// prompt; these notification rows already have their canonical publications.
pub(super) fn project(
    history: Vec<TaggedMessage>,
    notifications: &[TaggedMessage],
) -> Vec<TaggedMessage> {
    let headers = notifications
        .iter()
        .filter_map(|row| row.physical_header.as_deref())
        .collect::<BTreeSet<_>>();
    history
        .into_iter()
        .filter(|row| {
            !row.physical_header
                .as_deref()
                .is_some_and(|header| headers.contains(header))
        })
        .chain(notifications.iter().cloned())
        .collect()
}

fn rows<'a>(response: &'a Value, collection: &str) -> Result<&'a [Value]> {
    response["data"][collection]
        .as_array()
        .map(Vec::as_slice)
        .with_context(|| format!("wake input query omitted {collection}"))
}

fn receipt_keys(row: &Value) -> Result<Vec<String>> {
    Ok(crate::background_completion::notification_receipt_keys(
        row["background_completion_notification_keys_json"]
            .as_str()
            .context("wake claim lacks its notification receipt")?,
    )?)
}

pub(super) async fn load(
    node: &Arc<EmbeddedNode>,
    request: &crate::AgentRequest,
    cutoff: Option<u32>,
) -> Result<Vec<TaggedMessage>> {
    let Some(cutoff) = cutoff else {
        return Ok(Vec::new());
    };
    let scope = crate::session::session_scope_filter(
        &request.node_did,
        &request.session_id,
        request.requester_did.as_deref(),
    );
    ConfigAccess::transact_local_readonly(node, None, "request.wake_input", |txn| {
        let scope = scope.clone();
        Box::pin(async move {
            let assignments = crate::goal::load_request_assignments_in_txn(txn, &request.node_did, &request.session_id).await?;
            let validate_wake = |doc: &str| -> Result<()> {
                let row = assignments.request(doc)?;
                anyhow::ensure!(row.input.as_ref().is_some_and(crate::lifecycle::is_background_completion_request), "notification receipt does not belong to a background wake");
                crate::request_admission::verify_runtime_local_control_receipt(row, &request.node_did,
                    row.caused_by_parent_request_id.as_deref().context("background wake receipt has no signed parent")?,
                    request.requester_did.as_deref().unwrap_or(&request.node_did))?;
                assignments.scope(doc)?;
                Ok(())
            };
            validate_wake(&request.doc_id)?;
            let doc = escape_graphql_string(&request.doc_id);
            let response = txn.execute(&format!(r#"{{
                current: AgentRequest(filter: {{{scope}, _docID: {{_eq: "{doc}"}}, background_completion_input_through_sequence: {{_eq: {cutoff}}}}}, limit: 1) {{background_completion_notification_keys_json}}
                acknowledged: AgentRequest(filter: {{{scope}, _docID: {{_ne: "{doc}"}}, lifecycle_state: {{_eq: "completed"}}, background_completion_notification_keys_json: {{_ne: null}}, background_completion_input_through_sequence: {{_le: {cutoff}}}}}) {{_docID background_completion_input_through_sequence background_completion_notification_keys_json}}
            }}"#)).await?;
            let current = rows(&response, "current")?.first().context("current wake claim receipt is unavailable")?;
            let claimed = receipt_keys(current)?;
            if claimed.is_empty() { return Ok(Vec::new()); }
            let mut acknowledged = std::collections::BTreeMap::<String, u64>::new();
            for receipt in rows(&response, "acknowledged")? {
                let keys = receipt_keys(receipt)?;
                if !keys.iter().any(|key| claimed.contains(key)) { continue; }
                validate_wake(receipt["_docID"].as_str().context("completed wake receipt has no physical request")?)?;
                let through = receipt["background_completion_input_through_sequence"].as_u64().context("completed wake receipt lacks its revision cutoff")?;
                for key in keys {
                    acknowledged.entry(key).and_modify(|prior| *prior = (*prior).max(through)).or_insert(through);
                }
            }
            let keys = &claimed;
            let mut headers = Vec::new();
            for batch in keys.chunks(128) {
                let response = txn.execute(&format!("{{AgentMessage(filter: {{{scope}, sequence: {{_le: {cutoff}}}, message_key: {{_in: {}}}}}) {{{}}}}}", graphql_string_list_literal(batch.iter().map(String::as_str)), crate::session::canonical_rows::AGENT_MESSAGE_FIELDS)).await?;
                headers.extend(rows(&response, "AgentMessage")?.iter().map(crate::session::canonical_rows::decode_transcript_message_row).collect::<Result<Vec<_>>>()?);
            }
            anyhow::ensure!(headers.len() == keys.len() && headers.iter().map(|row| &row.message.message_key).collect::<BTreeSet<_>>().len() == keys.len(), "current wake notifications are missing or ambiguous");
            headers.sort_by_key(|row| row.message.sequence);
            anyhow::ensure!(headers.windows(2).all(|pair| pair[0].message.sequence < pair[1].message.sequence), "current wake notifications have ambiguous sequence order");
            let settled = headers.iter().filter(|header| acknowledged.get(&header.message.message_key)
                .is_some_and(|cutoff| u64::from(header.message.sequence) <= *cutoff))
                .map(|header| header.message.message_key.clone()).collect::<Vec<_>>();
            let current = current_keys(&claimed, &settled).into_iter().collect::<BTreeSet<_>>();
            headers.retain(|header| current.contains(&header.message.message_key));
            let mut reader = crate::session::TxnCanonicalReader::new(txn, &request.node_did, request.requester_did.as_deref());
            reader.observe_headers(&headers);
            let mut notifications = Vec::with_capacity(headers.len());
            for header in headers {
                let (_, message) = reader.load_message(&header.doc_id).await?;
                anyhow::ensure!(matches!(message, crate::llm::message::Message::User { .. }), "wake notification is not user input");
                let mut row = TaggedMessage::unassociated(message);
                row.physical_header = Some(header.doc_id);
                notifications.push(row);
            }
            Ok(notifications)
        })
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_wake_input_matches_lean_owner() {
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().wake_input_cases;
        assert!(cases.len() >= 6);
        for case in cases {
            assert_eq!(
                current_keys(&case.claimed, &case.acknowledged),
                case.current_keys,
                "{}",
                case.name
            );
            let native = |row: &crate::lean_vocab_test::LeanWakeInputRow| {
                if let Some(key) = &row.notification_key {
                    assert!(case.claimed.contains(key) || !case.current_keys.contains(key));
                }
                let mut native = TaggedMessage::unassociated(crate::llm::message::Message::user(
                    "equal content",
                ));
                native.physical_header = Some(row.header.clone());
                native
            };
            let history = case.rows.iter().map(native).collect();
            let notifications = case.notifications.iter().map(native).collect::<Vec<_>>();
            let actual = project(history, &notifications)
                .into_iter()
                .map(|row| row.physical_header.unwrap())
                .collect::<Vec<_>>();
            assert_eq!(actual, case.expected_order, "{}", case.name);
        }
    }
}

#[cfg(test)]
#[path = "wake_input_scope_tests.rs"]
mod scope_tests;
