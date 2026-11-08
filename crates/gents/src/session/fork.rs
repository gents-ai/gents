use std::collections::BTreeSet;

use anyhow::{Context, Result};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, SourceClose, TranscriptMessage,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::{json, Value};

use super::compaction_entries::{
    compaction_key, validate_compaction_chain, CompactionGenerationRow,
};
use super::history::sequence_message_key;
use super::output::reader::load_canonical_message_segments_in_txn;
use super::sessions::ensure_session_in_txn;
use crate::store::config_client::agent_session::{
    load_agent_session_owner_in_txn, load_agent_session_row_in_txn,
};
use crate::store::config_client::{ConfigAccess, ConfigApplyTxn};
use gents_protocol::output::rows::{
    decode_transcript_message_row, OutputSegmentRow, AGENT_MESSAGE_FIELDS,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents_protocol::session::session_scope_filter;
use gents_protocol::session::SessionScope;

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
    pub target_agent_did: Option<&'a str>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ForkOutcome {
    pub session_id: String,
    pub copied_messages: u32,
    pub copied_tool_calls: u32,
    pub copied_tool_results: u32,
    pub copied_compaction_entries: u32,
}

#[derive(Debug)]
pub struct ForkRelationshipIntent {
    session_id: String,
    creator_did: String,
    session_doc_id: String,
    agent_doc_id: String,
    agent_did: String,
    session_pending: bool,
    segment_doc_ids: Vec<String>,
    message_doc_ids: Vec<String>,
}

pub(super) struct ForkPublication {
    pub(super) outcome: ForkOutcome,
    pub(super) intent: ForkRelationshipIntent,
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
    #[error(
        "forking to a different agent is unavailable until source fragments can be copied safely"
    )]
    CrossAgentForkUnavailable,
    #[error("fork {session_id} committed but relationship publication is incomplete: {source}")]
    ForkRelationshipIncomplete {
        session_id: String,
        intent: Box<ForkRelationshipIntent>,
        #[source]
        source: anyhow::Error,
    },
    #[error("fork {0} has incomplete relationships; retry its retained creation intent")]
    ForkRecoveryIntentRequired(String),
    #[error("fork copy step failed: {0}")]
    ForkCopyFailed(#[from] anyhow::Error),
}

fn fork_error(error: anyhow::Error) -> ForkError {
    match error.downcast::<ForkError>() {
        Ok(error) => error,
        Err(error) => ForkError::ForkCopyFailed(error),
    }
}

fn may_reuse_source_fragments(source_agent: &str, target_agent: &str) -> bool {
    source_agent == target_agent
}

pub async fn fork_on(
    access: &ConfigAccess,
    params: ForkParams<'_>,
) -> Result<ForkOutcome, ForkError> {
    let publication = commit_fork(access, params).await?;
    publish_fork_relationships(access, publication.intent).await?;
    Ok(publication.outcome)
}

/// The fork and all of its destination-owned content commit in one
/// transaction; relationship publication follows from the returned intent.
pub(super) async fn commit_fork(
    access: &ConfigAccess,
    params: ForkParams<'_>,
) -> Result<ForkPublication, ForkError> {
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

/// Retry only an intent returned by the committed fork creation or an earlier
/// failed retry. Its exact physical IDs and creator are retained in memory.
pub async fn publish_fork_relationships(
    access: &ConfigAccess,
    mut intent: ForkRelationshipIntent,
) -> Result<(), ForkError> {
    let result: Result<()> = async {
        anyhow::ensure!(
            access.actor_did().as_deref() == Some(intent.creator_did.as_str()),
            "fork relationship retry requires its creating actor"
        );
        if intent.session_pending {
            super::sessions::grant_created_session_relationships(
                access,
                &intent.session_doc_id,
                &intent.agent_doc_id,
                &intent.agent_did,
            )
            .await?;
            intent.session_pending = false;
        }
        for (collection, schema, doc_ids) in [
            (
                "AgentOutputSegment",
                gents_protocol::schemas::AGENT_OUTPUT_SEGMENT,
                &mut intent.segment_doc_ids,
            ),
            (
                "AgentMessage",
                gents_protocol::schemas::AGENT_MESSAGE,
                &mut intent.message_doc_ids,
            ),
        ] {
            link_fork_children(
                access,
                collection,
                schema,
                &intent.creator_did,
                &intent.session_doc_id,
                &intent.agent_did,
                doc_ids,
            )
            .await?;
        }
        Ok(())
    }
    .await;
    result.map_err(|source| ForkError::ForkRelationshipIncomplete {
        session_id: intent.session_id.clone(),
        intent: Box::new(intent),
        source,
    })
}

/// Links each created child to the fork session and marks it, last first, so a
/// retry resumes after the last child whose marker committed.
async fn link_fork_children(
    access: &ConfigAccess,
    collection: &str,
    schema: &str,
    creator_did: &str,
    session_doc_id: &str,
    agent_did: &str,
    doc_ids: &mut Vec<String>,
) -> Result<()> {
    let creator = crate::store::graphql::escape_graphql_string(creator_did);
    while let Some(doc_id) = doc_ids.last() {
        let doc = crate::store::graphql::escape_graphql_string(doc_id);
        let response = access.execute(&format!(r#"{{ {collection}(filter: {{ _docID: {{ _eq: "{doc}" }}, creator_did: {{ _eq: "{creator}" }} }}, limit: 2) {{ _docID parent_linked }} }}"#)).await?;
        let rows = response["data"][collection]
            .as_array()
            .context("fork relationship status omitted rows")?;
        anyhow::ensure!(
            rows.len() == 1 && rows[0]["_docID"].as_str() == Some(doc_id.as_str()),
            "fork relationship status lost its creation binding"
        );
        if rows[0]["parent_linked"] == true {
            doc_ids.pop();
            continue;
        }
        anyhow::ensure!(
            rows[0]["parent_linked"] == false,
            "fork relationship marker is missing"
        );
        access
            .add_resource_relationship(collection, doc_id, "parent", "sessions", session_doc_id)
            .await?;
        access
            .add_actor_relationship(
                collection,
                doc_id,
                "working_agent",
                &identity::Did::new(agent_did)?,
            )
            .await?;
        let update = format!("update_{collection}");
        let response = access.transact_write("session.mark_fork_parent_linked", |txn| {
            let mutation = format!(r#"mutation($input: {collection}MutationInputArg!) {{ {update}(filter: {{ _docID: {{ _eq: "{doc}" }}, creator_did: {{ _eq: "{creator}" }}, parent_linked: {{ _eq: false }} }}, input: $input) {{ _docID }} }}"#);
            Box::pin(async move { txn.execute_client_signed_update(
                &mutation, &json!({"input": {"parent_linked": true}}), schema,
            ).await })
        }).await?;
        let rows = crate::store::graphql::mutation_documents(&response, &update)?
            .context("fork relationship marker omitted mutation result")?;
        anyhow::ensure!(
            rows.len() == 1 && rows[0]["_docID"].as_str() == Some(doc_id.as_str()),
            "fork relationship marker lost its creation binding"
        );
        doc_ids.pop();
    }
    Ok(())
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
    if child.trim().is_empty() || client_did.trim().is_empty() || params.target_agent_did.is_some()
    {
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
    let publication = access
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
                        Ok(Some(fork_in_txn(txn, &params, &child, Some(&requested.client_did)).await?))
                    }
                    EditForkDecision::Reuse => {
                        let escaped = crate::store::graphql::escape_graphql_string(&child);
                        let owner = crate::store::graphql::escape_graphql_string(params.caller_node_did);
                        let rows = txn.execute(&format!(r#"{{ AgentMessage(filter: {{ node_did: {{ _eq: "{owner}" }}, session_id: {{ _eq: "{escaped}" }}, parent_linked: {{ _eq: false }} }}, limit: 1) {{ _docID }} AgentOutputSegment(filter: {{ node_did: {{ _eq: "{owner}" }}, session_id: {{ _eq: "{escaped}" }}, parent_linked: {{ _eq: false }} }}, limit: 1) {{ _docID }} AgentSession(filter: {{ node_did: {{ _eq: "{owner}" }}, session_id: {{ _eq: "{escaped}" }}, relationships_pending: {{ _eq: true }} }}, limit: 1) {{ _docID }} }}"#)).await?;
                        anyhow::ensure!(rows["data"]["AgentMessage"].as_array().is_some_and(Vec::is_empty), ForkError::ForkRecoveryIntentRequired(child));
                        anyhow::ensure!(rows["data"]["AgentOutputSegment"].as_array().is_some_and(Vec::is_empty), ForkError::ForkRecoveryIntentRequired(child));
                        anyhow::ensure!(rows["data"]["AgentSession"].as_array().is_some_and(Vec::is_empty), ForkError::ForkRecoveryIntentRequired(child));
                        Ok(None)
                    },
                    EditForkDecision::Refuse => {
                        anyhow::bail!("the retained fork destination has another source binding")
                    }
                }
            })
        })
        .await
        .map_err(fork_error)?;
    if let Some(publication) = publication {
        let child = publication.outcome.session_id;
        publish_fork_relationships(access, publication.intent).await?;
        Ok(child)
    } else {
        Ok(child)
    }
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
        native_request_id: origin.native_request_id.clone(),
        blocks: origin.blocks.clone(),
        created_at: origin.created_at.clone(),
    }
}

/// The closing record a logical reference names and the flushes within its
/// exact extent, as the source reconstruction observed them.
fn fork_closure_records<'a>(
    segments: &'a [OutputSegmentRow],
    logical: &str,
) -> Result<Vec<&'a OutputSegmentRow>> {
    let closings = segments
        .iter()
        .filter(|row| row.doc_id == logical && row.segment.close.is_some())
        .collect::<Vec<_>>();
    let [closing] = closings.as_slice() else {
        anyhow::bail!("fork reference {logical} has no unique closing record");
    };
    let Some(SourceClose::Closed {
        segments: extent, ..
    }) = &closing.segment.close
    else {
        anyhow::bail!("fork reference {logical} names a retraction");
    };
    let mut seen = BTreeSet::new();
    Ok(segments
        .iter()
        .filter(|row| {
            row.segment.request_doc_id == closing.segment.request_doc_id
                && row.segment.source == closing.segment.source
                && row.segment.session_id == closing.segment.session_id
                && (row.doc_id == closing.doc_id
                    || row.segment.ordinal.is_some_and(|ordinal| ordinal < *extent))
                && seen.insert(row.doc_id.as_str())
        })
        .collect())
}

/// The physical tools a header names, as hydration selects them, refining
/// `FileInventory.messageTools`.
pub(crate) fn message_tool_references(
    message: &TranscriptMessage,
) -> impl Iterator<Item = String> + '_ {
    let delivery = match &message.publication {
        MessagePublication::ToolDelivery { tool_call_doc_id } => Some(tool_call_doc_id.clone()),
        _ => None,
    };
    delivery
        .into_iter()
        .chain(message.blocks.iter().filter_map(|block| match block {
            MessageBlock::ToolCall {
                tool_call_doc_id, ..
            }
            | MessageBlock::ToolResult {
                tool_call_doc_id, ..
            } => Some(tool_call_doc_id.clone()),
            _ => None,
        }))
}

struct ForkInventorySource<'a> {
    params: &'a ForkParams<'a>,
    session_doc_id: &'a str,
    agent_did: &'a str,
    tools: &'a BTreeSet<String>,
}

const FORK_INVENTORY_FIELDS: &str = "_docID tool_call_doc_id observation_doc_id request_doc_id node_did session_id requester_did changes verified_unchanged";

/// A fork is self-contained: the copied transcript's inventory projections are
/// copied into the fork session in its creating transaction, owned by that
/// creator and readable by the fork's Agent, and never resolved through the
/// source afterwards. Only projections the forking actor can read are copied;
/// a hidden or missing one leaves that tool's coverage unknown in the fork.
/// A copied source projection must be visible to the forking actor. Where the
/// backend observes its owner, that owner must be the source session's Agent
/// (native capture) or the source session's owner (an earlier fork's copy).
/// DefraDB's HTTP owner observation only answers whether the caller itself
/// owns a document, so an Agent-owned projection's owner is unobservable to a
/// client fork; the destination owner is the trust root for its own copies.
async fn copy_fork_inventory(
    txn: &ConfigApplyTxn<'_>,
    source: ForkInventorySource<'_>,
    child: &str,
    agent_did: &str,
) -> Result<()> {
    let params = source.params;
    let Some(requester) = params.caller_requester_did else {
        return Ok(());
    };
    if source.tools.is_empty() {
        return Ok(());
    }
    let escape = crate::store::graphql::escape_graphql_string;
    let response = txn
        .execute(&format!(
            r#"{{ ToolFileInventory(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }}, requester_did: {{ _eq: "{}" }} }}) {{ {FORK_INVENTORY_FIELDS} }} }}"#,
            escape(params.caller_node_did),
            escape(params.source_session_id),
            escape(requester),
        ))
        .await?;
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    for row in rows(&response, "ToolFileInventory")? {
        let tool = text(&row, "tool_call_doc_id")?;
        if !source.tools.contains(tool) {
            continue;
        }
        anyhow::ensure!(
            seen.insert(tool.to_owned()),
            "fork source has ambiguous file inventory projections"
        );
        anyhow::ensure!(
            text(&row, "node_did")? == params.caller_node_did
                && text(&row, "session_id")? == params.source_session_id
                && text(&row, "requester_did")? == requester,
            "fork inventory projection crossed exact source scope"
        );
        selected.push(row);
    }
    if selected.is_empty() {
        return Ok(());
    }
    let session_owner = txn
        .document_owners("AgentSession", &[source.session_doc_id.to_owned()])
        .await?
        .into_iter()
        .find(|row| row.doc_id == source.session_doc_id)
        .and_then(|row| row.owner)
        .map(|owner| owner.as_str().to_owned());
    let ids = selected
        .iter()
        .map(|row| text(row, "_docID").map(str::to_owned))
        .collect::<Result<Vec<_>>>()?;
    for chunk in ids.chunks(::query::owners::MAX_OWNER_OBSERVATIONS) {
        let owners = txn.document_owners("ToolFileInventory", chunk).await?;
        anyhow::ensure!(
            owners.len() == chunk.len()
                && owners.iter().zip(chunk).all(|(row, id)| {
                    row.doc_id == *id
                        && row.visible
                        && row.owner.as_ref().is_none_or(|owner| {
                            owner.as_str() == source.agent_did
                                || session_owner.as_deref() == Some(owner.as_str())
                        })
                }),
            "fork inventory projection is not owned by its source session"
        );
    }
    let reader = identity::Did::new(agent_did)?;
    for mut row in selected {
        let object = row
            .as_object_mut()
            .context("fork inventory projection is not an object")?;
        object.remove("_docID");
        anyhow::ensure!(
            object
                .get("changes")
                .is_none_or(|changes| changes.as_array().is_none_or(|rows| !rows.is_empty())),
            "fork inventory projection has an empty capture"
        );
        object.insert("session_id".into(), json!(child));
        let response = txn
            .execute_client_signed_create(
                "mutation($input: ToolFileInventoryMutationInputArg!) { create_ToolFileInventory(input: $input) { _docID } }",
                &json!({ "input": row }),
                gents_protocol::schemas::TOOL_FILE_INVENTORY,
            )
            .await?;
        let doc_id = crate::store::graphql::created_doc_id(&response, "ToolFileInventory")?;
        txn.add_actor_relationship("ToolFileInventory", &doc_id, "reader", &reader)
            .await?;
    }
    Ok(())
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

async fn create_compaction(
    txn: &ConfigApplyTxn<'_>,
    row: Value,
    session_doc_id: &str,
) -> Result<()> {
    let response = txn
        .execute_client_signed_create(
            "mutation($input: CompactionEntryMutationInputArg!) { create_CompactionEntry(input: $input) { _docID } }",
            &json!({ "input": row }),
            gents_protocol::schemas::COMPACTION_ENTRY,
        )
        .await?;
    let doc_id = crate::store::graphql::created_doc_id(&response, "CompactionEntry")?;
    txn.add_resource_relationship(
        "CompactionEntry",
        &doc_id,
        "parent",
        "sessions",
        session_doc_id,
    )
    .await?;
    Ok(())
}

async fn fork_in_txn(
    txn: &ConfigApplyTxn<'_>,
    params: &ForkParams<'_>,
    child: &str,
    edit_client_did: Option<&str>,
) -> Result<ForkPublication> {
    let parent_row = load_agent_session_row_in_txn(
        txn,
        params.caller_node_did,
        params.source_session_id,
        params.caller_requester_did,
    )
    .await?
    .ok_or_else(|| ForkError::ForkSourceNotFound(params.source_session_id.to_owned()))?;
    let parent_session_doc_id = parent_row.doc_id;
    let parent = parent_row.session;
    anyhow::ensure!(
        parent.node_did == params.caller_node_did
            && parent.requester_did.as_deref() == params.caller_requester_did,
        ForkError::ForkNotSameAgent
    );
    let agent = params.target_agent_did.unwrap_or(&parent.agent_did);
    let (agent_doc_id, agent_document) =
        crate::store::config_client::read_desired_state_record_in_txn(
            txn,
            crate::store::collection::Collection::Agent,
            params.caller_node_did,
            agent,
        )
        .await?
        .ok_or_else(|| ForkError::ForkAgentNotFound(agent.to_owned()))?;
    anyhow::ensure!(
        agent_document["enabled"] == true,
        "fork target agent is disabled"
    );
    anyhow::ensure!(
        may_reuse_source_fragments(&parent.agent_did, agent),
        ForkError::CrossAgentForkUnavailable
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
        .map(|row| decode_transcript_message_row(row).map_err(anyhow::Error::from))
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

    let session_doc_id = load_agent_session_owner_in_txn(txn, params.caller_node_did, child)
        .await?
        .context("created fork session is unavailable")?
        .doc_id;
    let creator_did = txn
        .actor_did()
        .context("fork requires an explicit creating actor")?
        .to_owned();
    let mut message_doc_ids = Vec::with_capacity(retained.len());
    let mut segment_doc_ids = Vec::new();
    let mut copied_closures = BTreeSet::new();
    let mut copied_tools = BTreeSet::new();

    for origin in &retained {
        copied_tools.extend(message_tool_references(&origin.message));
        let (resolved, segments) = load_canonical_message_segments_in_txn(
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
            .execute_client_signed_create(
                CREATE_AGENT_MESSAGE_MUTATION,
                &crate::store::config_client::session_children::fork_message_bound_variables(
                    txn,
                    &child_header,
                )
                .await?,
                gents_protocol::schemas::AGENT_MESSAGE,
            )
            .await?;
        message_doc_ids.push(crate::store::graphql::created_doc_id(
            &response,
            "AgentMessage",
        )?);
        for reference in origin.message.payload_references() {
            let logical = &reference.close_doc_id;
            if !copied_closures.insert(logical.clone()) {
                continue;
            }
            for record in fork_closure_records(&segments, logical)? {
                if let gents_protocol::output::OutputSource::ToolCall { tool_call_doc_id } =
                    &record.segment.source
                {
                    copied_tools.insert(tool_call_doc_id.clone());
                }
                let mut copy = record.segment.clone();
                copy.session_id = child.to_owned();
                copy.request_doc_id.clone_from(logical);
                let response = txn
                    .execute_client_signed_create(
                        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                        &crate::store::config_client::session_children::fork_output_segment_bound_variables(
                            txn, &copy,
                        )
                        .await?,
                        gents_protocol::schemas::AGENT_OUTPUT_SEGMENT,
                    )
                    .await?;
                segment_doc_ids.push(crate::store::graphql::created_doc_id(
                    &response,
                    "AgentOutputSegment",
                )?);
            }
        }
    }

    copy_fork_inventory(
        txn,
        ForkInventorySource {
            params,
            session_doc_id: &parent_session_doc_id,
            agent_did: &parent.agent_did,
            tools: &copied_tools,
        },
        child,
        agent,
    )
    .await?;

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
        create_compaction(txn, compaction, &session_doc_id).await?;
        copied_compactions += 1;
    }
    let outcome = ForkOutcome {
        session_id: child.to_owned(),
        copied_messages: u32::try_from(retained.len())?,
        copied_tool_calls: 0,
        copied_tool_results: 0,
        copied_compaction_entries: copied_compactions,
    };
    Ok(ForkPublication {
        outcome,
        intent: ForkRelationshipIntent {
            session_id: child.to_owned(),
            creator_did,
            session_doc_id,
            agent_doc_id,
            agent_did: agent.to_owned(),
            session_pending: true,
            segment_doc_ids,
            message_doc_ids,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::output::{
        OutputOutcome, PayloadPresentation, PayloadRef, PresentedPayload,
    };

    #[test]
    fn generated_fork_reference_authority_matches_native_decision() {
        let cases = crate::lean_vocab_test::lean_contract_snapshot()
            .session_document_cases
            .projection
            .iter()
            .filter(|case| case["operation"] == "fork_reference_authority")
            .collect::<Vec<_>>();
        assert!(!cases.is_empty());
        for case in cases {
            assert_eq!(
                may_reuse_source_fragments(
                    case["source_agent"].as_str().unwrap(),
                    case["target_agent"].as_str().unwrap(),
                ),
                case["allowed"].as_bool().unwrap(),
                "{}",
                case["name"]
            );
        }
    }

    fn modeled_message(row: &Value) -> TranscriptMessage {
        TranscriptMessage {
            native_request_id: None,
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
