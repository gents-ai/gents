use rapidhash::{HashMapExt, RapidHashMap};
use std::collections::BTreeMap;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::output::{
    MessagePublication, MessageRole, ReconstructionError, TerminalOutput,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::graphql::graphql_string_list_literal;
use crate::session::TxnCanonicalReader;
use crate::tool_call_lifecycle::CausedRequestTerminal;

pub async fn load_caused_request_terminal(
    node: &std::sync::Arc<EmbeddedNode>,
    caused_request_doc_id: &str,
) -> Result<Option<CausedRequestTerminal>> {
    ConfigAccess::transact_local_readonly(node, None, "background.caused_request_terminal", |txn| {
        Box::pin(async move {
            let ids = [caused_request_doc_id.to_owned()];
            Ok(caused_request_terminals_in_txn(txn, &ids)
                .await?
                .remove(caused_request_doc_id))
        })
    })
    .await
}

pub(crate) fn select_folded_outcome(
    folded: bool,
    caused: Option<CausedRequestTerminal>,
    head: Option<CausedRequestTerminal>,
) -> Option<CausedRequestTerminal> {
    if folded && matches!(caused.as_ref(), Some(CausedRequestTerminal::Superseded)) {
        head
    } else {
        caused
    }
}

pub(crate) async fn load_session_message_terminal(
    node: &std::sync::Arc<EmbeddedNode>,
    caused_doc_id: &str,
) -> Result<Option<CausedRequestTerminal>> {
    ConfigAccess::transact_local_readonly(node, None, "background.session_message_terminal", |txn| {
        Box::pin(async move {
            let response = txn.execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{
                    _docID request_id node_did requester_did session_id agent_id lifecycle_state failure_reason superseded_by_request superseded_by_request_doc_id
                }} }}"#,
                crate::graphql::escape_graphql_string(caused_doc_id),
            )).await?;
            let rows: Vec<AgentRequestRow> = query_rows(&response, "AgentRequest")?;
            let Some(caused) = rows.first() else { return Ok(None); };
            anyhow::ensure!(rows.len() == 1, "caused request physical identity is ambiguous");
            anyhow::ensure!(
                caused.node_did.as_deref().is_some_and(|value| !value.is_empty())
                    && caused.session_id.as_deref().is_some_and(|value| !value.is_empty())
                    && caused.agent_id.as_deref().is_some_and(|value| !value.is_empty()),
                "caused request lacks its session scope"
            );
            let folded = caused.lifecycle_state == Some(RequestLifecycleState::Superseded)
                && caused.failure_reason.as_deref() == Some(crate::request::lifecycle::FOLDED_REASON);
            let mut ids = vec![caused_doc_id.to_owned()];
            if folded {
                let head = caused.superseded_by_request_doc_id.as_deref()
                    .context("folded request lacks its physical head")?;
                anyhow::ensure!(head != caused_doc_id, "folded request references itself");
                let response = txn.execute(&format!(
                    r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID request_id node_did requester_did session_id agent_id }} }}"#,
                    crate::graphql::escape_graphql_string(head),
                )).await?;
                let heads: Vec<AgentRequestRow> = query_rows(&response, "AgentRequest")?;
                if heads.is_empty() { return Ok(None); }
                anyhow::ensure!(heads.len() == 1 && heads[0].node_did == caused.node_did
                    && heads[0].session_id == caused.session_id
                    && heads[0].requester_did == caused.requester_did
                    && heads[0].agent_id == caused.agent_id
                    && caused.superseded_by_request.as_deref() == Some(heads[0].request_id.as_str()),
                    "folded request head is outside its session scope");
                ids.push(head.to_owned());
            }
            let mut outcomes = caused_request_terminals_in_txn(txn, &ids).await?;
            let caused_outcome = outcomes.remove(caused_doc_id);
            let head_outcome = ids.get(1).and_then(|id| outcomes.remove(id));
            Ok(select_folded_outcome(folded, caused_outcome, head_outcome))
        })
    }).await
}

/// The durable terminals of many caused requests: one request query and one
/// header query, whatever the count. A request that is live or whose terminal
/// selection has not replicated is absent from the result.
pub(crate) async fn load_caused_request_terminals(
    access: &ConfigAccess,
    caused_request_doc_ids: &[String],
) -> Result<RapidHashMap<String, CausedRequestTerminal>> {
    if caused_request_doc_ids.is_empty() {
        return Ok(RapidHashMap::new());
    }
    access
        .transact_readonly("background.caused_request_terminals", |txn| {
            Box::pin(caused_request_terminals_in_txn(txn, caused_request_doc_ids))
        })
        .await
}

fn query_rows<T>(response: &serde_json::Value, collection: &str) -> Result<Vec<T>>
where
    T: serde::de::DeserializeOwned,
{
    let rows = response
        .pointer(&format!("/data/{collection}"))
        .cloned()
        .with_context(|| format!("caused terminal {collection} query omitted rows"))?;
    serde_json::from_value(rows).with_context(|| format!("decoding caused terminal {collection}"))
}

pub(crate) async fn caused_request_terminals_in_txn(
    txn: &ConfigApplyTxn<'_>,
    caused_request_doc_ids: &[String],
) -> Result<RapidHashMap<String, CausedRequestTerminal>> {
    let list = graphql_string_list_literal(caused_request_doc_ids.iter().map(String::as_str));
    let response = txn
        .execute(&format!(
            r#"{{
        AgentRequest(filter: {{ _docID: {{ _in: {list} }} }}) {{
            _docID request_id node_did requester_did session_id lifecycle_state
            failure_reason terminal_output
        }}
    }}"#
        ))
        .await?;
    let rows: Vec<AgentRequestRow> = query_rows(&response, "AgentRequest")?;
    let mut terminals = RapidHashMap::new();
    let mut pending = Vec::new();
    for row in &rows {
        let doc_id = row
            .doc_id
            .as_deref()
            .context("caused request missing _docID")?;
        let state = row
            .lifecycle_state
            .context("caused request missing lifecycle_state")?;
        let terminal = match state {
            RequestLifecycleState::Failed => CausedRequestTerminal::Failed {
                reason: row
                    .failure_reason
                    .as_deref()
                    .map(str::trim)
                    .filter(|reason| !reason.is_empty())
                    .unwrap_or("the started request failed")
                    .to_owned(),
            },
            RequestLifecycleState::Dead => CausedRequestTerminal::Dead,
            RequestLifecycleState::Interrupted => CausedRequestTerminal::Interrupted,
            RequestLifecycleState::Superseded => CausedRequestTerminal::Superseded,
            RequestLifecycleState::Completed => match &row.terminal_output {
                // Terminal state may arrive before its selection.
                None => continue,
                Some(TerminalOutput::Message { message_doc_id }) => {
                    pending.push((doc_id, row, message_doc_id.as_str()));
                    continue;
                }
                Some(_) => CausedRequestTerminal::Completed {
                    output: String::new(),
                },
            },
            _ => continue,
        };
        terminals.insert(doc_id.to_owned(), terminal);
    }
    if pending.is_empty() {
        return Ok(terminals);
    }
    let header_list = graphql_string_list_literal(pending.iter().map(|(_, _, header)| *header));
    let available = txn
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ _docID: {{ _in: {header_list} }} }}) {{ _docID }} }}"#
        ))
        .await?;
    let mut headers: RapidHashMap<String, usize> = RapidHashMap::new();
    for fact in query_rows::<serde_json::Value>(&available, "AgentMessage")? {
        let id = fact
            .get("_docID")
            .and_then(serde_json::Value::as_str)
            .context("caused terminal header omitted _docID")?;
        *headers.entry(id.to_owned()).or_default() += 1;
    }
    let mut readers: BTreeMap<(&str, Option<&str>), TxnCanonicalReader<'_, '_>> = BTreeMap::new();
    for (doc_id, row, message_doc_id) in pending {
        match headers.get(message_doc_id) {
            None => continue,
            Some(1) => {}
            Some(_) => anyhow::bail!("ambiguous physical caused terminal header"),
        }
        let node_did = row
            .node_did
            .as_deref()
            .context("caused request missing node_did")?;
        let session_id = row
            .session_id
            .as_deref()
            .context("caused request missing session_id")?;
        let requester_did = row.requester_did.as_deref();
        let reader = readers
            .entry((node_did, requester_did))
            .or_insert_with(|| TxnCanonicalReader::new(txn, node_did, requester_did));
        let (header, message) = match reader.load_message(message_doc_id).await {
            Ok(value) => value,
            Err(error)
                if error
                    .downcast_ref::<ReconstructionError>()
                    .is_some_and(ReconstructionError::is_incomplete) =>
            {
                continue
            }
            Err(error) => return Err(error),
        };
        anyhow::ensure!(
            header.request_doc_id.as_deref() == Some(doc_id)
                && header.session_id == session_id
                && header.role == MessageRole::Assistant
                && matches!(
                    header.publication,
                    MessagePublication::RequestExecution { .. }
                        | MessagePublication::RequestRecovery { .. }
                ),
            "selected caused terminal header is not an owned assistant publication"
        );
        terminals.insert(
            doc_id.to_owned(),
            CausedRequestTerminal::Completed {
                output: super::render_assistant_message_text(&message)?,
            },
        );
    }
    Ok(terminals)
}
