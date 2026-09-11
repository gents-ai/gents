//! Fork one authorized session snapshot in the existing configuration transaction.
//! All copied links are resolved before the transaction can publish the child.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::message::{Message, ToolResultContent, UserContent};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::{json, Value};

use super::compaction_entries::{
    compaction_key, validate_compaction_chain, CompactionGenerationRow,
};
use super::history::sequence_message_key;
use super::query::session_scope_filter;
use super::sessions::{ensure_session_in_txn, load_agent_session_row_in_txn};
use crate::config_client::{ConfigAccess, ConfigApplyTxn};

#[derive(Debug, Clone)]
pub struct ForkParams<'a> {
    pub source_session_id: &'a str,
    pub fork_at_user_turn: u32,
    pub caller_agent_did: &'a str,
    /// Exact requester scope. None means absent, never every requester.
    pub caller_requester_did: Option<&'a str>,
    pub target_behavior_id: Option<&'a str>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ForkOutcome {
    pub session_id: String,
    pub copied_messages: u32,
    pub copied_tool_calls: u32,
    pub copied_tool_results: u32,
    pub copied_compaction_entries: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum ForkError {
    #[error("fork source not found in caller scope: session_id={0}")]
    ForkSourceNotFound(String),
    #[error("fork source's principal or requester does not match caller")]
    ForkNotSameAgent,
    #[error("fork source has an active runtime AgentRequest and is busy")]
    ForkSourceBusy,
    #[error("fork_at_user_turn={0} is out of range (parent has only {1} user messages)")]
    ForkAtUserTurnOutOfRange(u32, u32),
    #[error("target behavior not found: {0}")]
    ForkBehaviorNotFound(String),
    #[error("target behavior {0} is not owned by principal {1}")]
    ForkBehaviorNotOwnedByPrincipal(String, String),
    #[error("fork copy step failed: {0}")]
    ForkCopyFailed(#[from] anyhow::Error),
}

fn fork_error(error: anyhow::Error) -> ForkError {
    match error.downcast::<ForkError>() {
        Ok(error) => error,
        Err(error) => ForkError::ForkCopyFailed(error),
    }
}

pub async fn fork(node: &EmbeddedNode, params: ForkParams<'_>) -> Result<ForkOutcome, ForkError> {
    let child = uuid::Uuid::new_v4().to_string();
    ConfigAccess::transact_local(node, None, "session.fork", |txn| {
        let params = params.clone();
        let child = child.clone();
        Box::pin(async move { fork_in_txn(txn, &params, &child).await })
    })
    .await
    .map_err(fork_error)
}

pub async fn fork_via_http(
    endpoint: &str,
    params: ForkParams<'_>,
) -> Result<ForkOutcome, ForkError> {
    let child = uuid::Uuid::new_v4().to_string();
    ConfigAccess::Graphql(endpoint.to_owned())
        .transact("session.fork", |txn| {
            let params = params.clone();
            let child = child.clone();
            Box::pin(async move { fork_in_txn(txn, &params, &child).await })
        })
        .await
        .map_err(fork_error)
}

const MESSAGE_FIELDS: &str = "_docID message_key session_id agent_did requester_did request_id request_doc_id sequence role content reasoning timestamp";
const CALL_FIELDS: &str = "_docID tool_call_key session_id agent_did requester_did request_id request_doc_id message_sequence tool_name tool_call_id args result status lifecycle_state started_at completed_at selected_service_id selected_tool_name tool_failure_class denial_reason denied_argv denied_command denied_argument denied_subcommand denied_prefix policy_mode policy_network cancel_cause latency_ms";
const SPILL_FIELDS: &str = "_docID tool_call_doc_id session_id agent_did requester_did tool_name tool_input output_text truncated truncation_metadata created_at discarded_because_interrupted";
const COMPACTION_FIELDS: &str = "_docID compaction_key session_id agent_did requester_did request_id request_doc_id sequence summary files_read files_modified messages_compacted compacted_through_sequence original_tokens compacted_tokens created_at";

fn rows(response: &Value, collection: &str) -> Result<Vec<Value>> {
    response
        .get("data")
        .and_then(|data| data.get(collection))
        .and_then(Value::as_array)
        .cloned()
        .with_context(|| format!("fork snapshot omitted {collection} rows"))
}

fn text<'a>(row: &'a Value, field: &str) -> Result<&'a str> {
    row.get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("fork row has no string {field}"))
}

fn optional_text<'a>(row: &'a Value, field: &str) -> Result<Option<&'a str>> {
    match row.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => anyhow::bail!("fork row has malformed optional string {field}"),
    }
}

fn register_copy(
    source: &[Value],
    copied: &mut BTreeSet<String>,
    id: String,
    collection: &str,
) -> Result<()> {
    anyhow::ensure!(
        !id.is_empty() && !source.iter().any(|row| row["_docID"] == id) && copied.insert(id),
        "fork {collection} copy has duplicate or source physical identity"
    );
    Ok(())
}

fn ordinal(row: &Value, field: &str) -> Result<u32> {
    let value = row
        .get(field)
        .and_then(Value::as_u64)
        .with_context(|| format!("fork row has no nonnegative {field}"))?;
    u32::try_from(value).with_context(|| format!("fork {field} exceeds supported sequence range"))
}

fn scoped_rows(rows: &[Value], params: &ForkParams<'_>, collection: &str) -> Result<()> {
    let mut ids = BTreeSet::new();
    for row in rows {
        anyhow::ensure!(
            text(row, "agent_did")? == params.caller_agent_did
                && text(row, "session_id")? == params.source_session_id
                && optional_text(row, "requester_did")? == params.caller_requester_did,
            "fork {collection} row crossed session scope"
        );
        let id = text(row, "_docID")?;
        anyhow::ensure!(
            !id.is_empty() && ids.insert(id),
            "duplicate {collection} physical identity in fork source"
        );
    }
    Ok(())
}

/// Exact suffix written by the existing spill owner. User prose is not a link:
/// only actual ToolResult text and persisted call result fields use this decoder.
fn spill_reference(value: &str) -> Option<(&str, &str)> {
    const PREFIX: &str = "[Full output: DefraDB doc ";
    let body = value.strip_suffix(']')?;
    let at = body.rfind(PREFIX)?;
    let id = &body[at + PREFIX.len()..];
    (!id.is_empty() && !id.chars().any(char::is_whitespace)).then_some((&value[..at], id))
}

fn remap_output(value: &str, ids: &BTreeMap<String, String>) -> Result<String> {
    match spill_reference(value) {
        None => Ok(value.to_owned()),
        Some((prefix, source)) => {
            let target = ids
                .get(source)
                .context("fork output refers to an absent or excluded spill")?;
            Ok(format!("{prefix}[Full output: DefraDB doc {target}]"))
        }
    }
}

fn remap_message(row: &Value, ids: &BTreeMap<String, String>) -> Result<String> {
    let original = text(row, "content")?;
    let mut message =
        gents_protocol::transcript::decode_persisted_message(text(row, "role")?, original);
    let mut changed = false;
    if let Message::User { content } = &mut message {
        for item in content {
            if let UserContent::ToolResult(result) = item {
                for part in &mut result.content {
                    if let ToolResultContent::Text(text) = part {
                        let mapped = remap_output(&text.text, ids)?;
                        changed |= mapped != text.text;
                        text.text = mapped;
                    }
                }
            }
        }
    }
    if changed {
        Ok(serde_json::to_string(&message)?)
    } else {
        Ok(original.to_owned())
    }
}

pub fn is_user_turn(row: &Value) -> Result<bool> {
    if text(row, "role")? != "user" {
        return Ok(false);
    }
    Ok(
        match gents_protocol::transcript::decode_persisted_message(
            text(row, "role")?,
            text(row, "content")?,
        ) {
            Message::User { content } => content
                .iter()
                .any(|part| !matches!(part, UserContent::ToolResult(_))),
            _ => false,
        },
    )
}

fn detached(row: &Value, child: &str) -> Result<Value> {
    let mut value = row
        .as_object()
        .context("fork row is not an object")?
        .clone();
    value.remove("_docID");
    value.insert("session_id".into(), json!(child));
    for key in ["request_id", "request_doc_id"] {
        if value.contains_key(key) {
            value.insert(key.into(), Value::Null);
        }
    }
    // DefraDB's nullable list wire representation is null, never [].
    for item in value.values_mut() {
        if item.as_array().is_some_and(Vec::is_empty) {
            *item = Value::Null;
        }
    }
    Ok(Value::Object(value))
}

async fn create_row(txn: &ConfigApplyTxn<'_>, collection: &str, row: Value) -> Result<String> {
    let mutation = format!("mutation($input: {collection}MutationInputArg!) {{ create_{collection}(input: $input) {{ _docID }} }}");
    let response = txn
        .execute_with_variables(&mutation, &json!({"input": row}))
        .await?;
    gents_protocol::graphql::extract_mutation_doc_id(&response, collection)
}

fn validate_compactions(
    params: &ForkParams<'_>,
    session: &str,
    entries: &[Value],
    messages: &[Value],
) -> Result<()> {
    let typed = entries
        .iter()
        .cloned()
        .map(serde_json::from_value::<CompactionGenerationRow>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    validate_compaction_chain(
        params.caller_agent_did,
        session,
        params.caller_requester_did,
        &typed,
    )?;
    let sequences = messages
        .iter()
        .map(|row| ordinal(row, "sequence"))
        .collect::<Result<BTreeSet<_>>>()?;
    for entry in entries {
        anyhow::ensure!(
            sequences.contains(&ordinal(entry, "compacted_through_sequence")?),
            "fork compaction cursor does not name a retained message"
        );
    }
    Ok(())
}

async fn fork_in_txn(
    txn: &ConfigApplyTxn<'_>,
    params: &ForkParams<'_>,
    child: &str,
) -> Result<ForkOutcome> {
    let parent = load_agent_session_row_in_txn(
        txn,
        params.caller_agent_did,
        params.source_session_id,
        params.caller_requester_did,
    )
    .await?
    .ok_or_else(|| ForkError::ForkSourceNotFound(params.source_session_id.to_owned()))?
    .session;
    anyhow::ensure!(
        parent.agent_did == params.caller_agent_did
            && parent.requester_did.as_deref() == params.caller_requester_did,
        ForkError::ForkNotSameAgent
    );
    let behavior = params.target_behavior_id.unwrap_or(&parent.behavior_id);
    let behavior_document = crate::config_client::read_desired_state_document_in_txn(
        txn,
        crate::collection::Collection::AgentBehavior,
        params.caller_agent_did,
        behavior,
    )
    .await?
    .ok_or_else(|| ForkError::ForkBehaviorNotFound(behavior.to_owned()))?;
    anyhow::ensure!(
        behavior_document["enabled"] == true,
        "fork target behavior is disabled"
    );
    let scope = session_scope_filter(
        params.caller_agent_did,
        params.source_session_id,
        params.caller_requester_did,
    );
    let snapshot = txn
        .execute(&format!(
            "{{
        AgentRequest(filter: {{{scope}}}) {{ lifecycle_state }}
        AgentMessage(filter: {{{scope}}}, order: {{sequence: ASC}}) {{{MESSAGE_FIELDS}}}
        AgentToolCall(filter: {{{scope}}}) {{{CALL_FIELDS}}}
        AgentToolResult(filter: {{{scope}}}) {{{SPILL_FIELDS}}}
        CompactionEntry(filter: {{{scope}}}, order: {{sequence: ASC}}) {{{COMPACTION_FIELDS}}}
    }}"
        ))
        .await?;
    for request in rows(&snapshot, "AgentRequest")? {
        let state = RequestLifecycleState::parse(text(&request, "lifecycle_state")?)?;
        if state.is_active_runtime() {
            return Err(ForkError::ForkSourceBusy.into());
        }
    }
    let messages = rows(&snapshot, "AgentMessage")?;
    let calls = rows(&snapshot, "AgentToolCall")?;
    let spills = rows(&snapshot, "AgentToolResult")?;
    let compactions = rows(&snapshot, "CompactionEntry")?;
    for (collection, values) in [
        ("AgentMessage", &messages),
        ("AgentToolCall", &calls),
        ("AgentToolResult", &spills),
        ("CompactionEntry", &compactions),
    ] {
        scoped_rows(values, params, collection)?;
    }
    let sequences = messages
        .iter()
        .map(|row| ordinal(row, "sequence"))
        .collect::<Result<BTreeSet<_>>>()?;
    anyhow::ensure!(
        sequences.len() == messages.len(),
        "fork source has duplicate message sequence keys"
    );
    for call in &calls {
        anyhow::ensure!(
            sequences.contains(&ordinal(call, "message_sequence")?),
            "fork tool call lacks an exact retained message association"
        );
    }
    let call_ids = calls
        .iter()
        .map(|row| text(row, "_docID"))
        .collect::<Result<BTreeSet<_>>>()?;
    for spill in &spills {
        anyhow::ensure!(
            call_ids.contains(text(spill, "tool_call_doc_id")?),
            "fork spill lacks its exact source call"
        );
    }
    let source_spills = spills
        .iter()
        .map(|row| {
            Ok((
                text(row, "_docID")?.to_owned(),
                text(row, "_docID")?.to_owned(),
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    for row in &messages {
        remap_message(row, &source_spills)?;
    }
    for row in &calls {
        if let Some(result) = optional_text(row, "result")? {
            remap_output(result, &source_spills)?;
        }
    }
    validate_compactions(params, params.source_session_id, &compactions, &messages)?;
    let turns = messages
        .iter()
        .filter_map(|row| match is_user_turn(row) {
            Ok(true) => Some(ordinal(row, "sequence")),
            Ok(false) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>>>()?;
    let total = u32::try_from(turns.len())?;
    if params.fork_at_user_turn > total {
        return Err(ForkError::ForkAtUserTurnOutOfRange(params.fork_at_user_turn, total).into());
    }
    let cut = turns
        .get(params.fork_at_user_turn as usize)
        .map(|v| u64::from(*v))
        .unwrap_or_else(|| sequences.last().map_or(0, |v| u64::from(*v) + 1));
    let retained_messages = messages
        .iter()
        .filter(|row| u64::from(ordinal(row, "sequence").expect("validated sequence")) < cut)
        .collect::<Vec<_>>();
    let retained_calls = calls
        .iter()
        .filter(|row| {
            u64::from(ordinal(row, "message_sequence").expect("validated sequence")) < cut
        })
        .collect::<Vec<_>>();
    let retained_call_ids = retained_calls
        .iter()
        .map(|row| text(row, "_docID").expect("validated doc id"))
        .collect::<BTreeSet<_>>();
    let retained_spills = spills
        .iter()
        .filter(|row| {
            retained_call_ids.contains(text(row, "tool_call_doc_id").expect("validated call link"))
        })
        .collect::<Vec<_>>();
    let retained_spill_ids = retained_spills
        .iter()
        .map(|row| {
            (
                text(row, "_docID").unwrap().to_owned(),
                text(row, "_docID").unwrap().to_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for row in &retained_messages {
        remap_message(row, &retained_spill_ids)?;
    }
    for row in &retained_calls {
        if let Some(result) = optional_text(row, "result")? {
            remap_output(result, &retained_spill_ids)?;
        }
    }
    let mut copied_compactions = compactions
        .iter()
        .filter(|row| {
            u64::from(ordinal(row, "compacted_through_sequence").expect("validated cursor")) < cut
        })
        .map(|row| detached(row, child))
        .collect::<Result<Vec<_>>>()?;
    for row in &mut copied_compactions {
        row["compaction_key"] = json!(compaction_key(
            params.caller_agent_did,
            child,
            params.caller_requester_did,
            ordinal(row, "sequence")?
        ));
    }
    validate_compactions(
        params,
        child,
        &copied_compactions,
        &retained_messages
            .iter()
            .map(|row| (*row).clone())
            .collect::<Vec<_>>(),
    )?;
    let mut copied_calls = BTreeMap::new();
    let mut new_call_ids = BTreeSet::new();
    for source in &retained_calls {
        let mut row = detached(source, child)?;
        row["tool_call_key"] = json!(format!("{child}:{}", text(source, "tool_call_id")?));
        row["result"] = Value::Null;
        let id = create_row(txn, "AgentToolCall", row).await?;
        register_copy(&calls, &mut new_call_ids, id.clone(), "AgentToolCall")?;
        copied_calls.insert(text(source, "_docID")?.to_owned(), id);
    }
    let mut copied_spills = BTreeMap::new();
    let mut new_spill_ids = BTreeSet::new();
    for source in &retained_spills {
        let mut row = detached(source, child)?;
        row["tool_call_doc_id"] = json!(copied_calls
            .get(text(source, "tool_call_doc_id")?)
            .context("fork call mapping is missing")?);
        let id = create_row(txn, "AgentToolResult", row).await?;
        register_copy(&spills, &mut new_spill_ids, id.clone(), "AgentToolResult")?;
        copied_spills.insert(text(source, "_docID")?.to_owned(), id);
    }
    for source in &retained_calls {
        let result = optional_text(source, "result")?
            .map(|result| remap_output(result, &copied_spills))
            .transpose()?;
        let updated = txn.execute_with_variables(
            "mutation($id:String!,$input:AgentToolCallMutationInputArg!){update_AgentToolCall(filter:{_docID:{_eq:$id}},input:$input){_docID}}",
            &json!({"id": copied_calls[text(source,"_docID")?], "input":{"result":result}}),
        ).await?;
        let updated_rows = rows(&updated, "update_AgentToolCall")?;
        anyhow::ensure!(
            updated_rows.len() == 1
                && text(&updated_rows[0], "_docID")? == copied_calls[text(source, "_docID")?],
            "fork call result update did not affect its exact child call"
        );
    }
    let mut new_message_ids = BTreeSet::new();
    for source in &retained_messages {
        let mut row = detached(source, child)?;
        row["message_key"] = json!(sequence_message_key(
            params.caller_agent_did,
            child,
            params.caller_requester_did,
            ordinal(source, "sequence")?
        ));
        row["content"] = json!(remap_message(source, &copied_spills)?);
        let id = create_row(txn, "AgentMessage", row).await?;
        register_copy(&messages, &mut new_message_ids, id, "AgentMessage")?;
    }
    let mut new_compaction_ids = BTreeSet::new();
    for row in copied_compactions.iter().cloned() {
        let id = create_row(txn, "CompactionEntry", row).await?;
        register_copy(&compactions, &mut new_compaction_ids, id, "CompactionEntry")?;
    }
    anyhow::ensure!(
        ensure_session_in_txn(
            txn,
            child,
            params.caller_agent_did,
            behavior,
            params.caller_requester_did,
            None,
            Some(gents_protocol::session::SessionProvenance {
                fork: Some(gents_protocol::session::SessionFork {
                    source_session_id: params.source_session_id.to_owned(),
                    at_user_turn: params.fork_at_user_turn
                }),
                ..Default::default()
            }),
            &chrono::Utc::now().to_rfc3339()
        )
        .await?,
        "fork child session already exists"
    );
    Ok(ForkOutcome {
        session_id: child.to_owned(),
        copied_messages: u32::try_from(retained_messages.len())?,
        copied_tool_calls: u32::try_from(retained_calls.len())?,
        copied_tool_results: u32::try_from(retained_spills.len())?,
        copied_compaction_entries: u32::try_from(copied_compactions.len())?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaps_only_tool_output_links_and_keeps_user_prose() {
        // Construct through the actual native types so this test follows their wire format.
        let message = Message::User {
            content: vec![
                UserContent::Text(gents_protocol::message::Text {
                    text: "[Full output: DefraDB doc source]".into(),
                }),
                UserContent::ToolResult(gents_protocol::message::ToolResult {
                    id: "call".into(),
                    call_id: None,
                    content: vec![ToolResultContent::Text(gents_protocol::message::Text {
                        text: "summary\n[Full output: DefraDB doc source]".into(),
                    })],
                }),
            ],
        };
        let row = json!({"role":"user","content":serde_json::to_string(&message).unwrap()});
        let mapping = BTreeMap::from([("source".into(), "child".into())]);
        let output = remap_message(&row, &mapping).unwrap();
        assert!(output.contains("summary\\n[Full output: DefraDB doc child]"));
        assert!(output.contains("[Full output: DefraDB doc source]"));
        assert!(remap_message(&row, &BTreeMap::new()).is_err());
    }

    #[test]
    fn tool_result_rows_do_not_consume_user_turns() {
        let message = Message::User {
            content: vec![UserContent::ToolResult(
                gents_protocol::message::ToolResult {
                    id: "call".into(),
                    call_id: None,
                    content: vec![ToolResultContent::Text(gents_protocol::message::Text {
                        text: "output".into(),
                    })],
                },
            )],
        };
        assert!(!is_user_turn(
            &json!({"role":"user","content":serde_json::to_string(&message).unwrap()})
        )
        .unwrap());
        assert!(!is_user_turn(&json!({"role":"tool","content":"legacy output"})).unwrap());
        assert!(is_user_turn(&json!({"role":"user","content":"question"})).unwrap());
    }

    #[test]
    fn scope_is_exact_and_malformed_requester_is_rejected() {
        let params = ForkParams {
            source_session_id: "s",
            fork_at_user_turn: 0,
            caller_agent_did: "owner",
            caller_requester_did: None,
            target_behavior_id: None,
        };
        let row = json!({"_docID":"same-physical-id","session_id":"s","agent_did":"owner","requester_did":null});
        assert!(scoped_rows(&[row.clone()], &params, "AgentMessage").is_ok());
        assert!(scoped_rows(&[row.clone(), row.clone()], &params, "AgentMessage").is_err());
        let mut invalid = row;
        invalid["requester_did"] = json!(42);
        assert!(scoped_rows(&[invalid], &params, "AgentMessage").is_err());
    }
}
