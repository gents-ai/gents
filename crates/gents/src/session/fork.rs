//! Header-only authorized session forks.
//!
//! Forking creates fresh child headers and child compaction metadata.  Payload
//! segments and tool lifecycle documents stay at their origin: a header's
//! immutable references are the dependency graph and never executable child
//! state.
use std::collections::BTreeSet;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::output::{MessageBlock, MessagePublication, MessageRole, TranscriptMessage};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::{json, Value};

use super::canonical_rows::{
    decode_transcript_message_row, transcript_message_create_variables, AGENT_MESSAGE_FIELDS,
    CREATE_AGENT_MESSAGE_MUTATION,
};
use super::compaction_entries::{
    compaction_key, validate_compaction_chain, CompactionGenerationRow,
};
use super::history::sequence_message_key;
use super::output::load_canonical_message_in_txn;
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
        let child = child.clone();
        let params = params.clone();
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
            let child = child.clone();
            let params = params.clone();
            Box::pin(async move { fork_in_txn(txn, &params, &child).await })
        })
        .await
        .map_err(fork_error)
}

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

fn ordinal(row: &Value, field: &str) -> Result<u32> {
    u32::try_from(
        row.get(field)
            .and_then(Value::as_u64)
            .with_context(|| format!("fork row has no nonnegative {field}"))?,
    )
    .with_context(|| format!("fork {field} exceeds supported sequence range"))
}

/// User headers that only deliver a tool result do not consume a fork turn.
pub fn is_user_turn(row: &Value) -> Result<bool> {
    let message = decode_transcript_message_row(row)?.message;
    Ok(message.role == MessageRole::User
        && message
            .blocks
            .iter()
            .any(|block| !matches!(block, MessageBlock::ToolResult { .. })))
}

fn fork_header(
    origin_doc_id: String,
    child: &str,
    key: String,
    origin: &TranscriptMessage,
) -> TranscriptMessage {
    TranscriptMessage {
        message_key: key,
        session_id: child.to_owned(),
        agent_did: origin.agent_did.clone(),
        requester_did: origin.requester_did.clone(),
        request_doc_id: None,
        publication: MessagePublication::Fork {
            origin_message_doc_id: origin_doc_id,
        },
        outcome: origin.outcome,
        sequence: origin.sequence,
        role: origin.role,
        native_id: origin.native_id.clone(),
        blocks: origin.blocks.clone(),
        created_at: origin.created_at.clone(),
    }
}

fn fork_compaction(origin: &Value, child: &str, params: &ForkParams<'_>) -> Result<Value> {
    anyhow::ensure!(
        text(origin, "session_id")? == params.source_session_id
            && text(origin, "agent_did")? == params.caller_agent_did
            && optional_text(origin, "requester_did")? == params.caller_requester_did,
        "fork compaction crossed exact source scope"
    );
    let sequence = ordinal(origin, "sequence")?;
    let mut row = origin
        .as_object()
        .context("fork compaction row is not an object")?
        .clone();
    row.remove("_docID");
    row.insert("session_id".into(), json!(child));
    row.insert("agent_did".into(), json!(params.caller_agent_did));
    row.insert("requester_did".into(), json!(params.caller_requester_did));
    row.insert("request_id".into(), Value::Null);
    row.insert("request_doc_id".into(), Value::Null);
    row.insert(
        "compaction_key".into(),
        json!(compaction_key(
            params.caller_agent_did,
            child,
            params.caller_requester_did,
            sequence
        )),
    );
    Ok(Value::Object(row))
}

async fn create_compaction(txn: &ConfigApplyTxn<'_>, row: Value) -> Result<()> {
    let response = txn
        .execute_with_variables(
            "mutation($input: CompactionEntryMutationInputArg!) { create_CompactionEntry(input: $input) { _docID } }",
            &json!({ "input": row }),
        )
        .await?;
    crate::graphql::created_doc_id(&response, "CompactionEntry")?;
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
            r#"{{
                AgentRequest(filter: {{ {scope} }}) {{ purpose lifecycle_state }}
                AgentMessage(filter: {{ {scope} }}, order: {{ sequence: ASC }}) {{ {AGENT_MESSAGE_FIELDS} }}
                CompactionEntry(filter: {{ {scope} }}, order: {{ sequence: ASC }}) {{ _docID compaction_key session_id agent_did requester_did request_id request_doc_id sequence summary files_read files_modified messages_compacted compacted_through_sequence original_tokens compacted_tokens created_at }}
            }}"#
        ))
        .await?;
    for request in rows(&snapshot, "AgentRequest")? {
        let purpose =
            gents_protocol::request_admission::RequestPurpose::try_from(text(&request, "purpose")?)
                .map_err(anyhow::Error::msg)?;
        if purpose == gents_protocol::request_admission::RequestPurpose::TitleAudit {
            continue;
        }
        if RequestLifecycleState::parse(text(&request, "lifecycle_state")?)?.is_active_runtime() {
            return Err(ForkError::ForkSourceBusy.into());
        }
    }
    let headers = rows(&snapshot, "AgentMessage")?
        .iter()
        .map(decode_transcript_message_row)
        .collect::<Result<Vec<_>>>()?;
    let mut sequences = BTreeSet::new();
    for header in &headers {
        anyhow::ensure!(
            header.message.session_id == params.source_session_id
                && header.message.agent_did == params.caller_agent_did
                && header.message.requester_did.as_deref() == params.caller_requester_did,
            "fork header crossed exact source scope"
        );
        anyhow::ensure!(
            sequences.insert(header.message.sequence),
            "fork source has duplicate sequence"
        );
    }
    let turns = headers
        .iter()
        .filter(|header| {
            header.message.role == MessageRole::User
                && header
                    .message
                    .blocks
                    .iter()
                    .any(|b| !matches!(b, MessageBlock::ToolResult { .. }))
        })
        .map(|header| header.message.sequence)
        .collect::<Vec<_>>();
    let total = u32::try_from(turns.len())?;
    if params.fork_at_user_turn > total {
        return Err(ForkError::ForkAtUserTurnOutOfRange(params.fork_at_user_turn, total).into());
    }
    let cut = turns
        .get(params.fork_at_user_turn as usize)
        .copied()
        .map(u64::from)
        .unwrap_or_else(|| {
            sequences
                .iter()
                .next_back()
                .map(|sequence| u64::from(*sequence) + 1)
                .unwrap_or(0)
        });
    let retained = headers
        .iter()
        .filter(|header| u64::from(header.message.sequence) < cut)
        .collect::<Vec<_>>();
    let retained_sequences = retained
        .iter()
        .map(|header| header.message.sequence)
        .collect::<BTreeSet<_>>();
    let compactions = rows(&snapshot, "CompactionEntry")?;
    let source_compactions = compactions
        .iter()
        .cloned()
        .map(serde_json::from_value::<CompactionGenerationRow>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    validate_compaction_chain(
        params.caller_agent_did,
        params.source_session_id,
        params.caller_requester_did,
        &source_compactions,
    )?;

    for origin in &retained {
        let (resolved, _) = load_canonical_message_in_txn(
            txn,
            &origin.doc_id,
            params.caller_agent_did,
            params.caller_requester_did,
        )
        .await?;
        anyhow::ensure!(
            resolved == origin.message,
            "fork origin header changed or conflicts during dependency validation"
        );
        let child_header = fork_header(
            origin.doc_id.clone(),
            child,
            sequence_message_key(
                params.caller_agent_did,
                child,
                params.caller_requester_did,
                origin.message.sequence,
            ),
            &origin.message,
        );
        let response = txn
            .execute_with_variables(
                CREATE_AGENT_MESSAGE_MUTATION,
                &transcript_message_create_variables(&child_header)?,
            )
            .await?;
        crate::graphql::created_doc_id(&response, "AgentMessage")?;
    }

    let mut copied_compactions = 0u32;
    let mut child_compactions = Vec::new();
    for origin in compactions {
        let through = ordinal(&origin, "compacted_through_sequence")?;
        if u64::from(through) < cut {
            anyhow::ensure!(
                retained_sequences.contains(&through),
                "fork compaction cursor does not name a retained child header"
            );
            child_compactions.push(fork_compaction(&origin, child, params)?);
        }
    }
    let child_chain = child_compactions
        .iter()
        .cloned()
        .map(serde_json::from_value::<CompactionGenerationRow>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    validate_compaction_chain(
        params.caller_agent_did,
        child,
        params.caller_requester_did,
        &child_chain,
    )?;
    for compaction in child_compactions {
        create_compaction(txn, compaction).await?;
        copied_compactions += 1;
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
                    at_user_turn: params.fork_at_user_turn,
                }),
                ..Default::default()
            }),
            &chrono::Utc::now().to_rfc3339(),
        )
        .await?,
        "fork child session already exists"
    );
    Ok(ForkOutcome {
        session_id: child.to_owned(),
        copied_messages: u32::try_from(retained.len())?,
        copied_tool_calls: 0,
        copied_tool_results: 0,
        copied_compaction_entries: copied_compactions,
    })
}
