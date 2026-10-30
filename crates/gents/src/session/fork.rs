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
use super::output::reader::load_canonical_message_in_txn;
use super::query::session_scope_filter;
use super::sessions::{
    ensure_session_in_txn, load_agent_session_owner_in_txn, load_agent_session_row_in_txn,
};
use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::session_origin::SessionScope;

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditForkBinding {
    client_did: String,
    parent: SessionScope,
    child: SessionScope,
    message_key: String,
}

enum ExistingEditFork {
    Missing,
    Unbound,
    Bound(EditForkBinding),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditForkDecision {
    Create,
    Reuse,
    Refuse,
}

fn decide_edit_fork(requested: &EditForkBinding, retained: &ExistingEditFork) -> EditForkDecision {
    if requested.parent.node_did != requested.child.node_did
        || requested.parent.requester_did != requested.child.requester_did
        || requested.parent.session_id == requested.child.session_id
        || requested.message_key.is_empty()
    {
        return EditForkDecision::Refuse;
    }
    match retained {
        ExistingEditFork::Missing => EditForkDecision::Create,
        ExistingEditFork::Bound(committed) if committed == requested => EditForkDecision::Reuse,
        ExistingEditFork::Unbound | ExistingEditFork::Bound(_) => EditForkDecision::Refuse,
    }
}

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
    ForkCopyFailed(#[from] anyhow::Error),
}

fn fork_error(error: anyhow::Error) -> ForkError {
    match error.downcast::<ForkError>() {
        Ok(error) => error,
        Err(error) => ForkError::ForkCopyFailed(error),
    }
}

pub async fn fork(
    node: &std::sync::Arc<EmbeddedNode>,
    params: ForkParams<'_>,
) -> Result<ForkOutcome, ForkError> {
    fork_on(&ConfigAccess::local(node.clone()), params).await
}

pub async fn fork_via_http(
    endpoint: &crate::config_client::GraphqlEndpoint,
    params: ForkParams<'_>,
) -> Result<ForkOutcome, ForkError> {
    fork_on(&ConfigAccess::Graphql(endpoint.clone()), params).await
}

pub async fn fork_on(
    access: &ConfigAccess,
    params: ForkParams<'_>,
) -> Result<ForkOutcome, ForkError> {
    let child = uuid::Uuid::new_v4().to_string();
    access
        .transact_write("session.fork", |txn| {
            let child = child.clone();
            let params = params.clone();
            Box::pin(async move { fork_in_txn(txn, &params, &child, None).await })
        })
        .await
        .map_err(fork_error)
}

/// The caller retains `child` before this call. Prefix publication and its exact
/// source binding commit together; a lost reply can therefore be recovered
/// without copying the prefix again or requiring the source to remain idle.
pub async fn fork_edit_on(
    access: &ConfigAccess,
    params: ForkParams<'_>,
    child: &str,
    client_did: &str,
) -> Result<String, ForkError> {
    let ForkBoundary::BeforeMessageKey(message_key) = params.boundary else {
        return Err(fork_error(anyhow::anyhow!(
            "an edit fork requires a canonical message key"
        )));
    };
    if child.trim().is_empty() || client_did.trim().is_empty() || params.target_agent_id.is_some() {
        return Err(fork_error(anyhow::anyhow!(
            "an edit fork requires a retained destination and the source agent"
        )));
    }
    let child = child.to_owned();
    let parent = SessionScope {
        node_did: params.caller_node_did.to_owned(),
        requester_did: params.caller_requester_did.map(str::to_owned),
        session_id: params.source_session_id.to_owned(),
    };
    let requested = EditForkBinding {
        client_did: client_did.to_owned(),
        child: SessionScope {
            session_id: child.clone(),
            ..parent.clone()
        },
        parent,
        message_key: message_key.to_owned(),
    };
    access
        .transact_write("session.edit_fork", |txn| {
            let requested = requested.clone();
            let params = params.clone();
            let child = child.clone();
            Box::pin(async move {
                let retained =
                    match load_agent_session_owner_in_txn(txn, params.caller_node_did, &child)
                        .await?
                    {
                        None => ExistingEditFork::Missing,
                        Some(row) => {
                            let session = row.session;
                            match session
                                .provenance
                                .and_then(|provenance| provenance.fork)
                                .and_then(|fork| {
                                    fork.before_message_key
                                        .zip(fork.edit_client_did)
                                        .map(|(key, client)| (fork.source_session_id, key, client))
                                }) {
                                None => ExistingEditFork::Unbound,
                                Some((source, key, client)) => {
                                    let scope = SessionScope {
                                        node_did: session.node_did,
                                        session_id: session.session_id,
                                        requester_did: session.requester_did,
                                    };
                                    ExistingEditFork::Bound(EditForkBinding {
                                        client_did: client,
                                        parent: SessionScope {
                                            session_id: source,
                                            ..scope.clone()
                                        },
                                        child: scope,
                                        message_key: key,
                                    })
                                }
                            }
                        }
                    };
                match decide_edit_fork(&requested, &retained) {
                    EditForkDecision::Create => {
                        Ok(
                            fork_in_txn(txn, &params, &child, Some(&requested.client_did))
                                .await?
                                .session_id,
                        )
                    }
                    EditForkDecision::Reuse => Ok(child),
                    EditForkDecision::Refuse => {
                        anyhow::bail!("the retained fork destination has another source binding")
                    }
                }
            })
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

pub fn is_user_turn(row: &Value) -> Result<bool> {
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
    anyhow::ensure!(
        text(origin, "session_id")? == params.source_session_id
            && text(origin, "node_did")? == params.caller_node_did
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
    edit_client_did: Option<&str>,
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
    anyhow::ensure!(
        parent.node_did == params.caller_node_did
            && parent.requester_did.as_deref() == params.caller_requester_did,
        ForkError::ForkNotSameAgent
    );
    let agent = params.target_agent_id.unwrap_or(&parent.agent_id);
    let agent_document = crate::config_client::read_desired_state_document_in_txn(
        txn,
        crate::collection::Collection::Agent,
        params.caller_node_did,
        agent,
    )
    .await?
    .ok_or_else(|| ForkError::ForkAgentNotFound(agent.to_owned()))?;
    anyhow::ensure!(
        agent_document["enabled"] == true,
        "fork target agent is disabled"
    );

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
                .map_err(anyhow::Error::msg)?;
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
        .collect::<Result<Vec<_>>>()?;
    let mut sequences = BTreeSet::new();
    for header in &headers {
        anyhow::ensure!(
            header.message.session_id == params.source_session_id
                && header.message.node_did == params.caller_node_did
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
        .filter(|header| is_user_message(&header.message))
        .map(|header| header.message.sequence)
        .collect::<Vec<_>>();
    let total = u32::try_from(turns.len())?;
    let at_user_turn = match params.boundary {
        ForkBoundary::UserTurn(turn) => turn,
        ForkBoundary::BeforeMessageKey(key) => {
            let cut = edit_cut(headers.iter().map(|header| &header.message), key)
                .ok_or_else(|| ForkError::ForkMessageNotEditable(key.to_owned()))?;
            u32::try_from(
                turns
                    .iter()
                    .position(|sequence| *sequence == cut)
                    .context("the edit anchor is not a source user turn")?,
            )?
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
        anyhow::ensure!(
            resolved == origin.message,
            "fork origin header changed or conflicts during dependency validation"
        );
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
        params.caller_node_did,
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
            params.caller_node_did,
            agent,
            params.caller_requester_did,
            None,
            Some(gents_protocol::session::SessionProvenance {
                fork: Some(gents_protocol::session::SessionFork {
                    source_session_id: params.source_session_id.to_owned(),
                    at_user_turn,
                    before_message_key: match params.boundary {
                        ForkBoundary::BeforeMessageKey(key) => Some(key.to_owned()),
                        ForkBoundary::UserTurn(_) => None,
                    },
                    edit_client_did: edit_client_did.map(str::to_owned),
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

    #[test]
    fn generated_edit_fork_bindings_control_destination_reuse() {
        fn scope(value: &Value) -> SessionScope {
            SessionScope {
                node_did: value["node"].as_u64().unwrap().to_string(),
                session_id: value["session"].as_u64().unwrap().to_string(),
                requester_did: value["requester"].as_u64().map(|id| id.to_string()),
            }
        }
        fn binding(value: &Value) -> EditForkBinding {
            EditForkBinding {
                client_did: value["client"].as_u64().unwrap().to_string(),
                parent: scope(&value["parent"]),
                child: scope(&value["child"]),
                message_key: value["message_key"].as_str().unwrap().to_owned(),
            }
        }
        let cases = &crate::lean_vocab_test::lean_contract_snapshot()
            .session_document_cases
            .edit_fork_bindings;
        assert!(!cases.is_empty());
        for case in cases {
            let retained = match case["retained_kind"].as_str().unwrap() {
                "missing" => ExistingEditFork::Missing,
                "unbound" => ExistingEditFork::Unbound,
                "bound" => ExistingEditFork::Bound(binding(&case["retained"])),
                other => panic!("untranslated retained fork {other}"),
            };
            let actual = match decide_edit_fork(&binding(&case["requested"]), &retained) {
                EditForkDecision::Create => "create",
                EditForkDecision::Reuse => "reuse",
                EditForkDecision::Refuse => "refuse",
            };
            assert_eq!(actual, case["decision"].as_str().unwrap(), "{case}");
        }
    }
}
