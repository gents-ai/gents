use rapidhash::{HashMapExt, RapidHashMap};
use std::collections::BTreeMap;

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

#[derive(Debug, thiserror::Error)]
pub enum QueryRowsError {
    #[error("caused terminal {collection} query omitted rows")]
    Omitted { collection: String },
    #[error("decoding caused terminal {collection}")]
    Decode {
        collection: String,
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Execute(#[from] crate::config_client::ConfigClientError),
    #[error(transparent)]
    Render(#[from] crate::background_tools::BackgroundToolsError),
    #[error("caused request missing _docID")]
    RequestMissingDocId,
    #[error("caused request missing lifecycle_state")]
    RequestMissingLifecycle,
    #[error("caused request missing node_did")]
    RequestMissingNodeDid,
    #[error("caused request missing session_id")]
    RequestMissingSession,
    #[error("caused terminal header omitted _docID")]
    HeaderMissingDocId,
    #[error("ambiguous physical caused terminal header")]
    AmbiguousHeader,
    #[error(transparent)]
    Message(crate::session::SessionOutputError),
    #[error("selected caused terminal header is not an owned assistant publication")]
    HeaderNotOwnedAssistant,
}

impl crate::config_client::TransactionFailure for QueryRowsError {
    fn config_client_error(&self) -> Option<&crate::config_client::ConfigClientError> {
        match self {
            Self::Execute(error) => Some(error),
            Self::Render(error) => error.config_client_error(),
            Self::Message(error) => error.config_client_error(),
            _ => None,
        }
    }
}

impl QueryRowsError {
    pub(crate) fn reconstruction(&self) -> Option<&ReconstructionError> {
        match self {
            Self::Message(error) => error.reconstruction(),
            Self::Render(error) => error.reconstruction(),
            _ => None,
        }
    }
}

type Result<T, E = QueryRowsError> = std::result::Result<T, E>;

fn query_rows<T>(response: &serde_json::Value, collection: &str) -> Result<Vec<T>, QueryRowsError>
where
    T: serde::de::DeserializeOwned,
{
    let rows = response
        .pointer(&format!("/data/{collection}"))
        .cloned()
        .ok_or_else(|| QueryRowsError::Omitted {
            collection: collection.to_owned(),
        })?;
    serde_json::from_value(rows).map_err(|source| QueryRowsError::Decode {
        collection: collection.to_owned(),
        source,
    })
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
            .ok_or(QueryRowsError::RequestMissingDocId)?;
        let state = row
            .lifecycle_state
            .ok_or(QueryRowsError::RequestMissingLifecycle)?;
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
            .ok_or(QueryRowsError::HeaderMissingDocId)?;
        *headers.entry(id.to_owned()).or_default() += 1;
    }
    let mut readers: BTreeMap<(&str, Option<&str>), TxnCanonicalReader<'_, '_>> = BTreeMap::new();
    for (doc_id, row, message_doc_id) in pending {
        match headers.get(message_doc_id) {
            None => continue,
            Some(1) => {}
            Some(_) => return Err(QueryRowsError::AmbiguousHeader),
        }
        let node_did = row
            .node_did
            .as_deref()
            .ok_or(QueryRowsError::RequestMissingNodeDid)?;
        let session_id = row
            .session_id
            .as_deref()
            .ok_or(QueryRowsError::RequestMissingSession)?;
        let requester_did = row.requester_did.as_deref();
        let reader = readers
            .entry((node_did, requester_did))
            .or_insert_with(|| TxnCanonicalReader::new(txn, node_did, requester_did));
        let (header, message) = match reader.load_message(message_doc_id).await {
            Ok(value) => value,
            Err(error)
                if error
                    .reconstruction()
                    .is_some_and(ReconstructionError::is_incomplete) =>
            {
                continue
            }
            Err(error) => return Err(QueryRowsError::Message(error)),
        };
        if !(header.request_doc_id.as_deref() == Some(doc_id)
            && header.session_id == session_id
            && header.role == MessageRole::Assistant
            && matches!(
                header.publication,
                MessagePublication::RequestExecution { .. }
                    | MessagePublication::RequestRecovery { .. }
            ))
        {
            return Err(QueryRowsError::HeaderNotOwnedAssistant);
        }
        terminals.insert(
            doc_id.to_owned(),
            CausedRequestTerminal::Completed {
                output: super::render_assistant_message_text(&message)?,
            },
        );
    }
    Ok(terminals)
}
