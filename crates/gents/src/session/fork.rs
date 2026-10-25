use std::collections::BTreeSet;
use std::num::TryFromIntError;

use defra_node::EmbeddedNode;
use gents_protocol::output::{MessageBlock, MessagePublication, MessageRole, TranscriptMessage};
use gents_protocol::request_lifecycle::{InvalidRequestLifecycleState, RequestLifecycleState};
use serde_json::{json, Value};

use super::canonical_rows::{
    decode_transcript_message_row, transcript_message_create_variables, CanonicalRowsError,
    AGENT_MESSAGE_FIELDS, CREATE_AGENT_MESSAGE_MUTATION,
};
use super::compaction_entries::{
    compaction_key, validate_compaction_chain, CompactionGenerationRow,
};
use super::history::sequence_message_key;
use super::output::{load_canonical_message_in_txn, SessionOutputError};
use super::query::session_scope_filter;
use super::sessions::{ensure_session_in_txn, load_agent_session_row_in_txn};
use super::SessionError;
use crate::config_client::{
    read_desired_state_document_in_txn, ConfigAccess, ConfigApplyTxn, ConfigClientError,
    DesiredStateError, TransactionFailure,
};
use crate::graphql::GraphqlResponseError;

#[derive(Debug, Clone)]
pub enum ForkBoundary<'a> {
    UserTurn(u32),
    BeforeMessageKey(&'a str),
}

#[derive(Debug, Clone)]
pub struct ForkParams<'a> {
    pub source_session_id: &'a str,
    pub boundary: ForkBoundary<'a>,
    pub caller_node_did: &'a str,
    pub caller_requester_did: Option<&'a str>,
    pub target_agent_id: Option<&'a str>,
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
    #[error("fork source's node or requester does not match caller")]
    ForkNotSameAgent,
    #[error("fork source has an active runtime AgentRequest and is busy")]
    ForkSourceBusy,
    #[error("fork_at_user_turn={0} is out of range (parent has only {1} user messages)")]
    ForkAtUserTurnOutOfRange(u32, u32),
    #[error("the selected user message is missing, ambiguous, or not editable: {0}")]
    ForkMessageNotEditable(String),
    #[error("target agent not found: {0}")]
    ForkAgentNotFound(String),
    #[error("fork copy step failed: {0}")]
    ForkCopyFailed(#[source] Box<SessionForkError>),
}

/// Wrapping variants are transparent so [`ForkError::ForkCopyFailed`] renders
/// the callee's own chain.
#[derive(Debug, thiserror::Error)]
pub enum SessionForkError {
    #[error(transparent)]
    Fork(#[from] ForkError),
    #[error(transparent)]
    Config(#[from] ConfigClientError),
    #[error(transparent)]
    Graphql(#[from] GraphqlResponseError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    DesiredState(#[from] DesiredStateError),
    #[error(transparent)]
    Output(#[from] SessionOutputError),
    #[error(transparent)]
    CanonicalRows(#[from] CanonicalRowsError),
    #[error(transparent)]
    LifecycleState(#[from] InvalidRequestLifecycleState),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    IntegerRange(#[from] TryFromIntError),
    #[error("{message}")]
    InvalidRequestPurpose { message: &'static str },
    #[error("fork snapshot omitted {collection} rows")]
    SnapshotRowsOmitted { collection: &'static str },
    #[error("fork row has no string {field}")]
    StringFieldMissing { field: &'static str },
    #[error("fork row has malformed optional string {field}")]
    MalformedOptionalField { field: &'static str },
    #[error("fork row has no nonnegative {field}")]
    NonnegativeFieldMissing { field: &'static str },
    #[error("fork {field} exceeds supported sequence range")]
    SequenceOutOfRange {
        field: &'static str,
        #[source]
        source: TryFromIntError,
    },
    #[error("fork compaction crossed exact source scope")]
    CompactionScope,
    #[error("fork compaction row is not an object")]
    CompactionNotObject,
    #[error("fork compaction cursor does not name a retained child header")]
    CompactionCursorUnretained,
    #[error("the edit anchor is not a source user turn")]
    EditAnchorNotUserTurn,
    #[error("fork header crossed exact source scope")]
    HeaderScope,
    #[error("fork source has duplicate sequence")]
    DuplicateSequence,
    #[error("fork origin header changed or conflicts during dependency validation")]
    OriginChanged,
    #[error("fork child session already exists")]
    ChildExists,
    #[error("fork target agent is disabled")]
    AgentDisabled,
}

impl TransactionFailure for SessionForkError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Config(error) => Some(error),
            Self::Graphql(error) => TransactionFailure::config_client_error(error),
            Self::Session(error) => TransactionFailure::config_client_error(error),
            Self::DesiredState(error) => TransactionFailure::config_client_error(error),
            Self::Output(error) => TransactionFailure::config_client_error(error),
            _ => None,
        }
    }
}

type Result<T, E = SessionForkError> = std::result::Result<T, E>;

fn fork_error(error: SessionForkError) -> ForkError {
    match error {
        SessionForkError::Fork(error) => error,
        error => ForkError::ForkCopyFailed(Box::new(error)),
    }
}

pub async fn fork(
    node: &std::sync::Arc<EmbeddedNode>,
    params: ForkParams<'_>,
) -> std::result::Result<ForkOutcome, ForkError> {
    fork_on(&ConfigAccess::local(node.clone()), params).await
}

pub async fn fork_via_http(
    endpoint: &crate::config_client::GraphqlEndpoint,
    params: ForkParams<'_>,
) -> std::result::Result<ForkOutcome, ForkError> {
    fork_on(&ConfigAccess::Graphql(endpoint.clone()), params).await
}

pub async fn fork_on(
    access: &ConfigAccess,
    params: ForkParams<'_>,
) -> std::result::Result<ForkOutcome, ForkError> {
    let child = uuid::Uuid::new_v4().to_string();
    access
        .transact_write("session.fork", |txn| {
            let child = child.clone();
            let params = params.clone();
            Box::pin(async move { fork_in_txn(txn, &params, &child).await })
        })
        .await
        .map_err(fork_error)
}

fn rows(response: &Value, collection: &'static str) -> Result<Vec<Value>> {
    response
        .get("data")
        .and_then(|data| data.get(collection))
        .and_then(Value::as_array)
        .cloned()
        .ok_or(SessionForkError::SnapshotRowsOmitted { collection })
}

fn text<'a>(row: &'a Value, field: &'static str) -> Result<&'a str> {
    row.get(field)
        .and_then(Value::as_str)
        .ok_or(SessionForkError::StringFieldMissing { field })
}

fn optional_text<'a>(row: &'a Value, field: &'static str) -> Result<Option<&'a str>> {
    match row.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(SessionForkError::MalformedOptionalField { field }),
    }
}

fn ordinal(row: &Value, field: &'static str) -> Result<u32> {
    let value = row
        .get(field)
        .and_then(Value::as_u64)
        .ok_or(SessionForkError::NonnegativeFieldMissing { field })?;
    u32::try_from(value).map_err(|source| SessionForkError::SequenceOutOfRange { field, source })
}

pub fn is_user_turn(row: &Value) -> Result<bool, CanonicalRowsError> {
    let message = decode_transcript_message_row(row)?.message;
    Ok(is_user_message(&message))
}

fn is_user_message(message: &TranscriptMessage) -> bool {
    message.role == MessageRole::User
        && message
            .blocks
            .iter()
            .any(|block| !matches!(block, MessageBlock::ToolResult { .. }))
}

fn edit_cut<'a>(
    messages: impl Iterator<Item = &'a TranscriptMessage>,
    message_key: &str,
) -> Option<u32> {
    let mut matching = messages.filter(|message| message.message_key == message_key);
    let message = matching.next()?;
    (matching.next().is_none() && is_user_message(message)).then_some(message.sequence)
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
        node_did: origin.node_did.clone(),
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
    if !(text(origin, "session_id")? == params.source_session_id
        && text(origin, "node_did")? == params.caller_node_did
        && optional_text(origin, "requester_did")? == params.caller_requester_did)
    {
        return Err(SessionForkError::CompactionScope);
    }
    let sequence = ordinal(origin, "sequence")?;
    let mut row = origin
        .as_object()
        .ok_or(SessionForkError::CompactionNotObject)?
        .clone();
    row.remove("_docID");
    row.insert("session_id".into(), json!(child));
    row.insert("node_did".into(), json!(params.caller_node_did));
    row.insert("requester_did".into(), json!(params.caller_requester_did));
    row.insert("request_id".into(), Value::Null);
    row.insert("request_doc_id".into(), Value::Null);
    row.insert(
        "compaction_key".into(),
        json!(compaction_key(
            params.caller_node_did,
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
        params.caller_node_did,
        params.source_session_id,
        params.caller_requester_did,
    )
    .await?
    .ok_or_else(|| ForkError::ForkSourceNotFound(params.source_session_id.to_owned()))?
    .session;
    if !(parent.node_did == params.caller_node_did
        && parent.requester_did.as_deref() == params.caller_requester_did)
    {
        return Err(ForkError::ForkNotSameAgent.into());
    }
    let agent = params.target_agent_id.unwrap_or(&parent.agent_id);
    let agent_document = read_desired_state_document_in_txn(
        txn,
        crate::collection::Collection::Agent,
        params.caller_node_did,
        agent,
    )
    .await?
    .ok_or_else(|| ForkError::ForkAgentNotFound(agent.to_owned()))?;
    if agent_document["enabled"] != true {
        return Err(SessionForkError::AgentDisabled);
    }

    let scope = session_scope_filter(
        params.caller_node_did,
        params.source_session_id,
        params.caller_requester_did,
    );
    let snapshot = txn
        .execute(&format!(
            r#"{{
                AgentRequest(filter: {{ {scope} }}) {{ purpose lifecycle_state }}
                AgentMessage(filter: {{ {scope} }}, order: {{ sequence: ASC }}) {{ {AGENT_MESSAGE_FIELDS} }}
                CompactionEntry(filter: {{ {scope} }}, order: {{ sequence: ASC }}) {{ _docID compaction_key session_id node_did requester_did request_id request_doc_id sequence summary files_read files_modified messages_compacted compacted_through_sequence original_tokens compacted_tokens created_at }}
            }}"#
        ))
        .await?;
    for request in rows(&snapshot, "AgentRequest")? {
        let purpose =
            gents_protocol::request_admission::RequestPurpose::try_from(text(&request, "purpose")?)
                .map_err(|message| SessionForkError::InvalidRequestPurpose { message })?;
        if purpose.is_auxiliary() {
            continue;
        }
        if RequestLifecycleState::parse(text(&request, "lifecycle_state")?)?.is_active_runtime() {
            return Err(ForkError::ForkSourceBusy.into());
        }
    }
    let headers = rows(&snapshot, "AgentMessage")?
        .iter()
        .map(decode_transcript_message_row)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut sequences = BTreeSet::new();
    for header in &headers {
        if !(header.message.session_id == params.source_session_id
            && header.message.node_did == params.caller_node_did
            && header.message.requester_did.as_deref() == params.caller_requester_did)
        {
            return Err(SessionForkError::HeaderScope);
        }
        if !sequences.insert(header.message.sequence) {
            return Err(SessionForkError::DuplicateSequence);
        }
    }
    let turns = headers
        .iter()
        .filter(|header| is_user_message(&header.message))
        .map(|header| header.message.sequence)
        .collect::<Vec<_>>();
    let total = u32::try_from(turns.len())?;
    let at_user_turn = match params.boundary {
        ForkBoundary::UserTurn(turn) => turn,
        ForkBoundary::BeforeMessageKey(key) => {
            let cut = edit_cut(headers.iter().map(|header| &header.message), key)
                .ok_or_else(|| ForkError::ForkMessageNotEditable(key.to_owned()))?;
            let position = turns
                .iter()
                .position(|sequence| *sequence == cut)
                .ok_or(SessionForkError::EditAnchorNotUserTurn)?;
            u32::try_from(position)?
        }
    };
    if at_user_turn > total {
        return Err(ForkError::ForkAtUserTurnOutOfRange(at_user_turn, total).into());
    }
    let cut = turns
        .get(at_user_turn as usize)
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
        params.caller_node_did,
        params.source_session_id,
        params.caller_requester_did,
        &source_compactions,
    )?;

    for origin in &retained {
        let (resolved, _) = load_canonical_message_in_txn(
            txn,
            &origin.doc_id,
            params.caller_node_did,
            params.caller_requester_did,
        )
        .await?;
        if resolved != origin.message {
            return Err(SessionForkError::OriginChanged);
        }
        let child_header = fork_header(
            origin.doc_id.clone(),
            child,
            sequence_message_key(
                params.caller_node_did,
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
            if !retained_sequences.contains(&through) {
                return Err(SessionForkError::CompactionCursorUnretained);
            }
            child_compactions.push(fork_compaction(&origin, child, params)?);
        }
    }
    let child_chain = child_compactions
        .iter()
        .cloned()
        .map(serde_json::from_value::<CompactionGenerationRow>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    validate_compaction_chain(
        params.caller_node_did,
        child,
        params.caller_requester_did,
        &child_chain,
    )?;
    for compaction in child_compactions {
        create_compaction(txn, compaction).await?;
        copied_compactions += 1;
    }
    if !(ensure_session_in_txn(
        txn,
        child,
        params.caller_node_did,
        agent,
        params.caller_requester_did,
        None,
        Some(gents_protocol::session::SessionProvenance {
            fork: Some(gents_protocol::session::SessionFork {
                source_session_id: params.source_session_id.to_owned(),
                at_user_turn,
            }),
            ..Default::default()
        }),
        &chrono::Utc::now().to_rfc3339(),
    )
    .await?)
    {
        return Err(SessionForkError::ChildExists);
    }
    Ok(ForkOutcome {
        session_id: child.to_owned(),
        copied_messages: u32::try_from(retained.len())?,
        copied_tool_calls: 0,
        copied_tool_results: 0,
        copied_compaction_entries: copied_compactions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::output::{
        OutputOutcome, PayloadPresentation, PayloadRef, PresentedPayload,
    };

    fn modeled_message(row: &Value) -> TranscriptMessage {
        TranscriptMessage {
            message_key: row["key"].as_str().unwrap().to_owned(),
            sequence: u32::try_from(row["sequence"].as_u64().unwrap()).unwrap(),
            role: serde_json::from_value(row["role"].clone()).unwrap(),
            session_id: "source".into(),
            node_did: "node".into(),
            requester_did: Some("requester".into()),
            request_doc_id: Some("request".into()),
            publication: MessagePublication::RequestExecution {
                execution_generation: "generation".into(),
            },
            outcome: OutputOutcome::Complete,
            native_id: None,
            created_at: "2026-10-18T00:00:00Z".into(),
            blocks: row["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|block| match block.as_str().unwrap() {
                    "text" => MessageBlock::Text {
                        text: PresentedPayload {
                            output: PayloadRef {
                                close_doc_id: "close".into(),
                                stream: 0,
                            },
                            presentation: PayloadPresentation::Full,
                        },
                    },
                    "toolResult" => MessageBlock::ToolResult {
                        tool_call_doc_id: "tool".into(),
                        id: "call".into(),
                        call_id: None,
                        parts: Vec::new(),
                    },
                    other => panic!("untranslated modeled block {other}"),
                })
                .collect(),
        }
    }

    #[test]
    fn generated_edit_anchors_select_exact_canonical_user_messages() {
        let cases = &crate::lean_vocab_test::lean_contract_snapshot()
            .session_document_cases
            .edit_anchors;
        assert!(!cases.is_empty());
        for case in cases {
            let messages = case["messages"]
                .as_array()
                .unwrap()
                .iter()
                .map(modeled_message)
                .collect::<Vec<_>>();
            let expected = case["cut"].as_u64().map(|cut| u32::try_from(cut).unwrap());
            assert_eq!(
                edit_cut(messages.iter(), case["key"].as_str().unwrap()),
                expected,
                "{case}"
            );
        }
    }
}
