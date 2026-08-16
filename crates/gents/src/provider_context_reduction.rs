//! Durable request-local provider-context reductions (#1127).
//!
//! `CompactionEntry` remains the session-prefix fact consumed by request
//! loading. This module owns the different per-turn entity: an immutable exact
//! provider projection, persisted before the owned loop may activate it.

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::graphql::{escape_graphql_string, graphql_mutation_response_with_transaction_retry};
use crate::llm::message::Message;

const REDUCTION_KEY_PREFIX: &str = "provider-context-reduction:v1";
const SOURCE_BOUNDARY_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptFactRef {
    pub doc_id: String,
    pub sequence: i64,
    pub commit_cid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceBoundary {
    pub boundary_version: u32,
    pub request_doc_id: String,
    pub request_commit_cid: String,
    /// The newest canonical transcript fact visible when the exact provider
    /// source projection was reduced. The projection itself is stored in the
    /// prefix/suffix payloads; this bounded high-water mark ties it to the
    /// append-only transcript without copying the session or its history.
    pub canonical_through: Option<TranscriptFactRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducerCallRef {
    pub call_id: String,
    pub call_seq: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderContextReduction {
    #[serde(default, rename = "_docID")]
    pub doc_id: String,
    pub reduction_key: String,
    pub agent_did: String,
    #[serde(default)]
    pub requester_did: Option<String>,
    pub session_id: String,
    pub request_id: String,
    pub request_doc_id: String,
    pub request_commit_cid: String,
    pub reduction_index: i64,
    pub turn_index: i64,
    #[serde(default)]
    pub parent_reduction_key: Option<String>,
    #[serde(default)]
    pub producer_call_id: Option<String>,
    #[serde(default)]
    pub producer_call_seq: Option<i64>,
    pub source_boundary_json: String,
    pub compacted_prefix_json: String,
    pub retained_suffix_json: String,
    pub pair_closed: bool,
    pub checkpoint_messages_json: String,
    pub summary: String,
    pub messages_compacted: i64,
    pub original_tokens: i64,
    pub compacted_tokens: i64,
    pub created_at: String,
}

impl ProviderContextReduction {
    pub fn checkpoint_messages(&self) -> Result<Vec<Message>> {
        serde_json::from_str(&self.checkpoint_messages_json)
            .context("decoding ProviderContextReduction checkpoint_messages_json")
    }

    pub fn source_boundary(&self) -> Result<SourceBoundary> {
        serde_json::from_str(&self.source_boundary_json)
            .context("decoding ProviderContextReduction source_boundary_json")
    }

    /// Only this newest checkpoint directly shapes the restored provider view;
    /// `load_unconsumed_for_request` returns the full lineage separately.
    pub fn active_reduction_keys(&self) -> Vec<String> {
        vec![self.reduction_key.clone()]
    }
}

#[derive(Debug)]
pub struct NewProviderContextReduction<'a> {
    pub agent_did: &'a str,
    pub requester_did: Option<&'a str>,
    pub session_id: &'a str,
    pub request_id: &'a str,
    pub request_doc_id: &'a str,
    pub request_commit_cid: &'a str,
    pub reduction_index: usize,
    pub turn_index: usize,
    pub parent_reduction_key: Option<&'a str>,
    pub producer_call: Option<&'a ProducerCallRef>,
    pub source_boundary: &'a SourceBoundary,
    pub compacted_prefix: &'a [Message],
    pub retained_suffix: &'a [Message],
    pub checkpoint_messages: &'a [Message],
    pub summary: &'a str,
    pub original_tokens: usize,
    pub compacted_tokens: usize,
}

pub fn reduction_key(
    agent_did: &str,
    session_id: &str,
    request_doc_id: &str,
    turn_index: usize,
    reduction_index: usize,
) -> Result<String> {
    let digest = crate::rendered_request::sha256_canonical_json(&json!([
        agent_did,
        session_id,
        request_doc_id,
        turn_index,
        reduction_index
    ]))?;
    Ok(format!("{REDUCTION_KEY_PREFIX}:{digest}"))
}

pub async fn capture_source_boundary(
    node: &EmbeddedNode,
    session_id: &str,
    request_doc_id: &str,
    request_commit_cid: &str,
) -> Result<SourceBoundary> {
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                order: {{ sequence: DESC }},
                limit: 1
            ) {{
                _docID
                sequence
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "capturing provider-context source boundary for session {session_id}: {:?}",
            response.errors
        );
    }
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let canonical_through = match rows.as_slice() {
        [] => None,
        [row] => {
            let doc_id = required_str(row, "_docID")?;
            let sequence = row
                .get("sequence")
                .and_then(Value::as_i64)
                .with_context(|| format!("AgentMessage {doc_id} has no sequence"))?;
            let commit = crate::graphql::newest_document_composite_commit(
                node,
                doc_id,
                &format!("AgentMessage {doc_id} source boundary"),
            )
            .await?
            .with_context(|| format!("AgentMessage {doc_id} has no composite commit CID"))?;
            Some(TranscriptFactRef {
                doc_id: doc_id.to_string(),
                sequence,
                commit_cid: commit.cid,
            })
        }
        rows => anyhow::bail!(
            "capturing provider-context source boundary returned {} high-water rows",
            rows.len()
        ),
    };
    Ok(SourceBoundary {
        boundary_version: SOURCE_BOUNDARY_VERSION,
        request_doc_id: request_doc_id.to_string(),
        request_commit_cid: request_commit_cid.to_string(),
        canonical_through,
    })
}

pub async fn persist(
    node: &EmbeddedNode,
    input: NewProviderContextReduction<'_>,
) -> Result<ProviderContextReduction> {
    if input.checkpoint_messages.is_empty() {
        anyhow::bail!("provider-context reduction checkpoint cannot be empty");
    }
    if input.reduction_index == 0 {
        anyhow::bail!("provider-context reduction index starts at one");
    }
    validate_source_boundary(
        input.source_boundary,
        input.request_doc_id,
        input.request_commit_cid,
    )?;
    let pair_closed = crate::compaction::pair_safe_boundary(
        &[
            input.compacted_prefix.to_vec(),
            input.retained_suffix.to_vec(),
        ]
        .concat(),
        input.compacted_prefix.len(),
    ) == input.compacted_prefix.len();
    if !pair_closed {
        anyhow::bail!("provider-context reduction split crosses an open tool-call/result pair");
    }
    let expected_checkpoint = checkpoint_from_suffix(input.retained_suffix, input.summary);
    if input.checkpoint_messages != expected_checkpoint {
        anyhow::bail!(
            "provider-context reduction checkpoint disagrees with its summary and retained suffix"
        );
    }
    let reduction_key = reduction_key(
        input.agent_did,
        input.session_id,
        input.request_doc_id,
        input.turn_index,
        input.reduction_index,
    )?;
    let intended = IntendedReduction::from_input(&input, reduction_key.clone())?;

    match load_by_key(node, &reduction_key).await?.as_slice() {
        [] => {}
        [existing] => {
            intended.ensure_matches(existing)?;
            return Ok(existing.clone());
        }
        twins => anyhow::bail!(
            "provider-context reduction key {reduction_key} has {} visible logical twins",
            twins.len()
        ),
    }

    let created_at = chrono::Utc::now().to_rfc3339();
    let producer_call_id = graphql_optional_string(
        input
            .producer_call
            .map(|producer| producer.call_id.as_str()),
    );
    let producer_call_seq = input
        .producer_call
        .map(|producer| producer.call_seq.to_string())
        .unwrap_or_else(|| "null".to_string());
    let parent_reduction_key = graphql_optional_string(input.parent_reduction_key);
    let requester_did = graphql_optional_string(input.requester_did);
    let mutation = format!(
        r#"mutation {{
            create_ProviderContextReduction(input: {{
                reduction_key: "{reduction_key}"
                agent_did: "{agent_did}"
                requester_did: {requester_did}
                session_id: "{session_id}"
                request_id: "{request_id}"
                request_doc_id: "{request_doc_id}"
                request_commit_cid: "{request_commit_cid}"
                reduction_index: {reduction_index}
                turn_index: {turn_index}
                parent_reduction_key: {parent_reduction_key}
                producer_call_id: {producer_call_id}
                producer_call_seq: {producer_call_seq}
                source_boundary_json: "{source_boundary_json}"
                compacted_prefix_json: "{compacted_prefix_json}"
                retained_suffix_json: "{retained_suffix_json}"
                pair_closed: true
                checkpoint_messages_json: "{checkpoint_messages_json}"
                summary: "{summary}"
                messages_compacted: {messages_compacted}
                original_tokens: {original_tokens}
                compacted_tokens: {compacted_tokens}
                created_at: "{created_at}"
            }}) {{ _docID }}
        }}"#,
        reduction_key = escape_graphql_string(&reduction_key),
        agent_did = escape_graphql_string(input.agent_did),
        session_id = escape_graphql_string(input.session_id),
        request_id = escape_graphql_string(input.request_id),
        request_doc_id = escape_graphql_string(input.request_doc_id),
        request_commit_cid = escape_graphql_string(input.request_commit_cid),
        reduction_index = input.reduction_index,
        turn_index = input.turn_index,
        source_boundary_json = escape_graphql_string(&intended.source_boundary_json),
        compacted_prefix_json = escape_graphql_string(&intended.compacted_prefix_json),
        retained_suffix_json = escape_graphql_string(&intended.retained_suffix_json),
        checkpoint_messages_json = escape_graphql_string(&intended.checkpoint_messages_json),
        summary = escape_graphql_string(input.summary.trim()),
        messages_compacted = input.compacted_prefix.len(),
        original_tokens = input.original_tokens,
        compacted_tokens = input.compacted_tokens,
    );
    let response = graphql_mutation_response_with_transaction_retry(
        node,
        &mutation,
        "creating ProviderContextReduction",
    )
    .await;
    if response.has_errors() {
        anyhow::bail!(
            "creating ProviderContextReduction {reduction_key}: {:?}",
            response.errors
        );
    }

    let rows = load_by_key(node, &reduction_key).await?;
    if rows.len() != 1 {
        anyhow::bail!(
            "provider-context reduction key {reduction_key} has {} visible logical twins after create",
            rows.len()
        );
    }
    intended.ensure_matches(&rows[0])?;
    Ok(rows.into_iter().next().expect("length checked"))
}

pub async fn load_for_request(
    node: &EmbeddedNode,
    request_doc_id: &str,
) -> Result<Vec<ProviderContextReduction>> {
    let query = format!(
        r#"{{
            ProviderContextReduction(
                filter: {{ request_doc_id: {{ _eq: "{}" }} }},
                order: [{{ reduction_index: ASC }}, {{ created_at: ASC }}]
            ) {{ {} }}
        }}"#,
        escape_graphql_string(request_doc_id),
        REDUCTION_FIELDS
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "loading ProviderContextReduction for request {request_doc_id}: {:?}",
            response.errors
        );
    }
    let rows: Vec<ProviderContextReduction> = serde_json::from_value(
        response
            .data
            .as_ref()
            .and_then(|data| data.get("ProviderContextReduction"))
            .cloned()
            .unwrap_or_else(|| json!([])),
    )?;
    validate_chain(&rows)?;
    validate_recoverable_rows(&rows)?;
    Ok(rows)
}

/// Restore only the crash cut this fact closes: a durable checkpoint that no
/// subsequent RenderedRequest consumed. Once captured for a provider call, a
/// restarted request deliberately derives from canonical history instead of
/// pretending the call outcome is known.
pub async fn load_unconsumed_for_request(
    node: &EmbeddedNode,
    request_doc_id: &str,
) -> Result<Option<(ProviderContextReduction, Vec<String>)>> {
    let reductions = load_for_request(node, request_doc_id).await?;
    let Some(latest) = reductions.last() else {
        return Ok(None);
    };
    let lineage_keys = reductions
        .iter()
        .map(|row| row.reduction_key.clone())
        .collect::<Vec<_>>();
    let rendered = format!(
        r#"{{ RenderedRequest(
            filter: {{
                request_doc_id: {{ _eq: "{}" }}
                capture_scope: {{ _like: "inference.%" }}
                turn_index: {{ _ge: {} }}
                provenance_json: {{ _like: "%{}%" }}
            }},
            order: [{{ turn_index: DESC }}, {{ attempt: DESC }}],
            limit: 64
        ) {{ capture_scope turn_index provenance_json }} }}"#,
        escape_graphql_string(request_doc_id),
        latest.turn_index,
        escape_graphql_string(&latest.reduction_key)
    );
    let response = node.execute(&rendered).await;
    if response.has_errors() {
        anyhow::bail!(
            "checking ProviderContextReduction consumption for request {request_doc_id}: {:?}",
            response.errors
        );
    }
    let captures = response
        .data
        .as_ref()
        .and_then(|data| data.get("RenderedRequest"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut consumed = false;
    for row in captures {
        let Some(scope) = row.get("capture_scope").and_then(Value::as_str) else {
            tracing::warn!(
                request_doc_id,
                "ignoring RenderedRequest without capture scope"
            );
            continue;
        };
        let Some(turn_index) = row.get("turn_index").and_then(Value::as_i64) else {
            tracing::warn!(
                request_doc_id,
                scope,
                "ignoring RenderedRequest without turn index"
            );
            continue;
        };
        let Some(provenance) = row.get("provenance_json").and_then(Value::as_str) else {
            tracing::warn!(
                request_doc_id,
                scope,
                "ignoring RenderedRequest without provenance"
            );
            continue;
        };
        consumed |= rendered_capture_cites_reduction(
            scope,
            turn_index,
            provenance,
            latest.turn_index,
            &latest.reduction_key,
        );
    }
    Ok((!consumed).then(|| (latest.clone(), lineage_keys)))
}

/// Consumption is an explicit, supported join from an inference capture to a
/// reduction, never an inference from clocks or unrelated/opaque rows.
pub fn rendered_capture_cites_reduction(
    capture_scope: &str,
    capture_turn_index: i64,
    provenance_json: &str,
    reduction_turn_index: i64,
    reduction_key: &str,
) -> bool {
    use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind, ParsedProvenance};

    let Ok(scope) = capture_scope.parse::<CaptureScope>() else {
        return false;
    };
    if scope.kind != CaptureScopeKind::Inference || capture_turn_index < reduction_turn_index {
        return false;
    }
    let Ok(ParsedProvenance::Manifest(manifest)) =
        gents_protocol::rendered_request::ProvenanceManifest::parse(provenance_json)
    else {
        return false;
    };
    manifest
        .assembly_trace
        .reduction_keys
        .iter()
        .any(|key| key == reduction_key)
}

const REDUCTION_FIELDS: &str = "_docID reduction_key agent_did requester_did session_id request_id request_doc_id request_commit_cid reduction_index turn_index parent_reduction_key producer_call_id producer_call_seq source_boundary_json compacted_prefix_json retained_suffix_json pair_closed checkpoint_messages_json summary messages_compacted original_tokens compacted_tokens created_at";

async fn load_by_key(
    node: &EmbeddedNode,
    reduction_key: &str,
) -> Result<Vec<ProviderContextReduction>> {
    let query = format!(
        r#"{{ ProviderContextReduction(filter: {{ reduction_key: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
        escape_graphql_string(reduction_key),
        REDUCTION_FIELDS
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "loading ProviderContextReduction key {reduction_key}: {:?}",
            response.errors
        );
    }
    serde_json::from_value(
        response
            .data
            .as_ref()
            .and_then(|data| data.get("ProviderContextReduction"))
            .cloned()
            .unwrap_or_else(|| json!([])),
    )
    .context("decoding ProviderContextReduction rows")
}

#[derive(Debug)]
struct IntendedReduction {
    reduction_key: String,
    agent_did: String,
    requester_did: Option<String>,
    session_id: String,
    request_id: String,
    request_doc_id: String,
    source_boundary_json: String,
    compacted_prefix_json: String,
    retained_suffix_json: String,
    checkpoint_messages_json: String,
    request_commit_cid: String,
    reduction_index: i64,
    turn_index: i64,
    parent_reduction_key: Option<String>,
    producer_call_id: Option<String>,
    producer_call_seq: Option<i64>,
    summary: String,
    messages_compacted: i64,
    original_tokens: i64,
    compacted_tokens: i64,
}

impl IntendedReduction {
    fn from_input(input: &NewProviderContextReduction<'_>, reduction_key: String) -> Result<Self> {
        Ok(Self {
            reduction_key,
            agent_did: input.agent_did.to_string(),
            requester_did: input.requester_did.map(ToOwned::to_owned),
            session_id: input.session_id.to_string(),
            request_id: input.request_id.to_string(),
            request_doc_id: input.request_doc_id.to_string(),
            source_boundary_json: crate::rendered_request::canonical_json_string(
                &serde_json::to_value(input.source_boundary)?,
            )?,
            compacted_prefix_json: crate::rendered_request::canonical_json_string(
                &serde_json::to_value(input.compacted_prefix)?,
            )?,
            retained_suffix_json: crate::rendered_request::canonical_json_string(
                &serde_json::to_value(input.retained_suffix)?,
            )?,
            checkpoint_messages_json: crate::rendered_request::canonical_json_string(
                &serde_json::to_value(input.checkpoint_messages)?,
            )?,
            request_commit_cid: input.request_commit_cid.to_string(),
            reduction_index: i64::try_from(input.reduction_index).unwrap_or(i64::MAX),
            turn_index: i64::try_from(input.turn_index).unwrap_or(i64::MAX),
            parent_reduction_key: input.parent_reduction_key.map(ToOwned::to_owned),
            producer_call_id: input.producer_call.map(|producer| producer.call_id.clone()),
            producer_call_seq: input.producer_call.map(|producer| producer.call_seq),
            summary: input.summary.trim().to_string(),
            messages_compacted: i64::try_from(input.compacted_prefix.len()).unwrap_or(i64::MAX),
            original_tokens: i64::try_from(input.original_tokens).unwrap_or(i64::MAX),
            compacted_tokens: i64::try_from(input.compacted_tokens).unwrap_or(i64::MAX),
        })
    }

    fn ensure_matches(&self, row: &ProviderContextReduction) -> Result<()> {
        let matches = row.reduction_key == self.reduction_key
            && row.agent_did == self.agent_did
            && row.requester_did == self.requester_did
            && row.session_id == self.session_id
            && row.request_id == self.request_id
            && row.request_doc_id == self.request_doc_id
            && row.request_commit_cid == self.request_commit_cid
            && row.reduction_index == self.reduction_index
            && row.turn_index == self.turn_index
            && row.parent_reduction_key == self.parent_reduction_key
            && row.producer_call_id == self.producer_call_id
            && row.producer_call_seq == self.producer_call_seq
            && row.source_boundary_json == self.source_boundary_json
            && row.compacted_prefix_json == self.compacted_prefix_json
            && row.retained_suffix_json == self.retained_suffix_json
            && row.pair_closed
            && row.checkpoint_messages_json == self.checkpoint_messages_json
            && row.summary == self.summary
            && row.messages_compacted == self.messages_compacted
            && row.original_tokens == self.original_tokens
            && row.compacted_tokens == self.compacted_tokens;
        if !matches {
            anyhow::bail!(
                "provider-context reduction key {} is already bound to conflicting immutable facts",
                self.reduction_key
            );
        }
        Ok(())
    }
}

fn validate_chain(rows: &[ProviderContextReduction]) -> Result<()> {
    for (index, row) in rows.iter().enumerate() {
        let expected_index = i64::try_from(index + 1).unwrap_or(i64::MAX);
        if row.reduction_index != expected_index {
            anyhow::bail!(
                "ProviderContextReduction {} has index {}, expected {}",
                row.reduction_key,
                row.reduction_index,
                expected_index
            );
        }
        let expected_parent = index
            .checked_sub(1)
            .map(|parent| rows[parent].reduction_key.as_str());
        if row.parent_reduction_key.as_deref() != expected_parent {
            anyhow::bail!(
                "ProviderContextReduction {} has parent {:?}, expected {:?}",
                row.reduction_key,
                row.parent_reduction_key,
                expected_parent
            );
        }
    }
    Ok(())
}

fn validate_recoverable_rows(rows: &[ProviderContextReduction]) -> Result<()> {
    let Some(first) = rows.first() else {
        return Ok(());
    };
    for row in rows {
        if row.agent_did != first.agent_did
            || row.requester_did != first.requester_did
            || row.session_id != first.session_id
            || row.request_id != first.request_id
            || row.request_doc_id != first.request_doc_id
        {
            anyhow::bail!(
                "ProviderContextReduction {} rebinds its request-local chain",
                row.reduction_key
            );
        }
        let reduction_index = usize::try_from(row.reduction_index)
            .context("ProviderContextReduction has a negative reduction index")?;
        let turn_index = usize::try_from(row.turn_index)
            .context("ProviderContextReduction has a negative turn index")?;
        let expected_key = reduction_key(
            &row.agent_did,
            &row.session_id,
            &row.request_doc_id,
            turn_index,
            reduction_index,
        )?;
        if row.reduction_key != expected_key {
            anyhow::bail!(
                "ProviderContextReduction {} does not match its immutable identity tuple",
                row.reduction_key
            );
        }
        if !row.pair_closed {
            anyhow::bail!(
                "ProviderContextReduction {} has an open tool-call/result split",
                row.reduction_key
            );
        }
        if row.producer_call_id.is_some() != row.producer_call_seq.is_some() {
            anyhow::bail!(
                "ProviderContextReduction {} has incomplete producer-call provenance",
                row.reduction_key
            );
        }
        let boundary = row.source_boundary()?;
        validate_source_boundary(&boundary, &row.request_doc_id, &row.request_commit_cid)?;
        let prefix: Vec<Message> = serde_json::from_str(&row.compacted_prefix_json)
            .context("decoding ProviderContextReduction compacted_prefix_json")?;
        let suffix: Vec<Message> = serde_json::from_str(&row.retained_suffix_json)
            .context("decoding ProviderContextReduction retained_suffix_json")?;
        let split = prefix.len();
        let source = [prefix.as_slice(), suffix.as_slice()].concat();
        if crate::compaction::pair_safe_boundary(&source, split) != split {
            anyhow::bail!(
                "ProviderContextReduction {} stored a split through a tool-call/result pair",
                row.reduction_key
            );
        }
        if row.messages_compacted != i64::try_from(split).unwrap_or(i64::MAX) {
            anyhow::bail!(
                "ProviderContextReduction {} messages_compacted disagrees with its exact prefix",
                row.reduction_key
            );
        }
        if row.original_tokens < 0 || row.compacted_tokens < 0 {
            anyhow::bail!(
                "ProviderContextReduction {} has a negative token estimate",
                row.reduction_key
            );
        }
        let checkpoint = row.checkpoint_messages()?;
        if !checkpoint_matches_stored_projection(&checkpoint, &suffix, &row.summary) {
            anyhow::bail!(
                "ProviderContextReduction {} checkpoint has invalid stored projection structure",
                row.reduction_key
            );
        }
    }
    Ok(())
}

fn checkpoint_from_suffix(suffix: &[Message], summary: &str) -> Vec<Message> {
    let mut checkpoint = suffix.to_vec();
    let summary = summary.trim();
    if !summary.is_empty() {
        checkpoint.insert(
            0,
            crate::prompt::LayeredPromptBuilder::system_reminder(
                &crate::prompt::continuation_checkpoint_reminder(summary),
            ),
        );
    }
    checkpoint
}

fn checkpoint_matches_stored_projection(
    checkpoint: &[Message],
    suffix: &[Message],
    summary: &str,
) -> bool {
    if checkpoint.is_empty() {
        return false;
    }
    if summary.trim().is_empty() {
        return checkpoint == suffix;
    }
    checkpoint.len() == suffix.len() + 1 && checkpoint.get(1..) == Some(suffix)
}

fn validate_source_boundary(
    boundary: &SourceBoundary,
    request_doc_id: &str,
    request_commit_cid: &str,
) -> Result<()> {
    if boundary.boundary_version != SOURCE_BOUNDARY_VERSION
        || boundary.request_doc_id != request_doc_id
        || boundary.request_commit_cid != request_commit_cid
    {
        anyhow::bail!(
            "provider-context reduction source boundary is bound to another request version"
        );
    }
    if let Some(row) = &boundary.canonical_through {
        if row.doc_id.is_empty() || row.commit_cid.is_empty() {
            anyhow::bail!("provider-context reduction source boundary has an empty fact identity");
        }
    }
    Ok(())
}

fn graphql_optional_string(value: Option<&str>) -> String {
    value
        .map(|value| format!("\"{}\"", escape_graphql_string(value)))
        .unwrap_or_else(|| "null".to_string())
}

fn required_str<'a>(row: &'a Value, field: &str) -> Result<&'a str> {
    row.get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("row has no string {field}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::message::{
        AssistantContent, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent, UserContent,
    };

    #[test]
    fn reduction_key_is_componentwise_and_delimiter_safe() {
        let base = reduction_key("did:a", "s:x", "doc:y", 1, 2).unwrap();
        assert_ne!(base, reduction_key("did:a", "s", "x:doc:y", 1, 2).unwrap());
        assert_ne!(base, reduction_key("did:a", "s:x", "doc:y", 2, 2).unwrap());
        assert_ne!(base, reduction_key("did:a", "s:x", "doc:y", 1, 3).unwrap());
    }

    #[test]
    fn exact_chain_rejects_gaps_and_parent_rebinding() {
        let mut rows = Vec::new();
        let row = |index: i64, key: &str, parent: Option<&str>| ProviderContextReduction {
            doc_id: format!("doc-{index}"),
            reduction_key: key.to_string(),
            agent_did: "did:a".to_string(),
            requester_did: None,
            session_id: "s".to_string(),
            request_id: "r".to_string(),
            request_doc_id: "rd".to_string(),
            request_commit_cid: "cid".to_string(),
            reduction_index: index,
            turn_index: index,
            parent_reduction_key: parent.map(ToOwned::to_owned),
            producer_call_id: None,
            producer_call_seq: None,
            source_boundary_json: "{}".to_string(),
            compacted_prefix_json: "[]".to_string(),
            retained_suffix_json: "[]".to_string(),
            pair_closed: true,
            checkpoint_messages_json: "[]".to_string(),
            summary: String::new(),
            messages_compacted: 0,
            original_tokens: 0,
            compacted_tokens: 0,
            created_at: String::new(),
        };
        rows.push(row(1, "k1", None));
        rows.push(row(2, "k2", Some("k1")));
        assert!(validate_chain(&rows).is_ok());
        rows[1].parent_reduction_key = Some("other".to_string());
        assert!(validate_chain(&rows).is_err());
    }

    fn boundary(request_doc_id: &str) -> SourceBoundary {
        SourceBoundary {
            boundary_version: SOURCE_BOUNDARY_VERSION,
            request_doc_id: request_doc_id.to_string(),
            request_commit_cid: "request-cid".to_string(),
            canonical_through: Some(TranscriptFactRef {
                doc_id: "message-doc".to_string(),
                sequence: 1,
                commit_cid: "message-cid".to_string(),
            }),
        }
    }

    #[tokio::test]
    async fn durable_chain_is_idempotent_conflict_visible_and_restartable() {
        let node = EmbeddedNode::builder().build().await.unwrap();
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let prefix = vec![Message::user("old")];
        let suffix = vec![Message::user("current")];
        let checkpoint = checkpoint_from_suffix(&suffix, "summary");
        let source = boundary("request-doc");
        let boundary_compaction = node
            .execute(
                r#"mutation { create_CompactionEntry(input: {
                    compaction_key: "session:1"
                    session_id: "session"
                    request_id: "earlier-request"
                    sequence: 1
                    summary: "session prefix"
                    messages_compacted: 1
                    original_tokens: 200
                    compacted_tokens: 50
                    created_at: "2026-08-13T00:00:00Z"
                }) { _docID } }"#,
            )
            .await;
        assert!(
            !boundary_compaction.has_errors(),
            "request-boundary compaction seed failed: {:?}",
            boundary_compaction.errors
        );

        let first = persist(
            &node,
            NewProviderContextReduction {
                agent_did: "did:key:agent",
                requester_did: Some("did:key:user"),
                session_id: "session",
                request_id: "request",
                request_doc_id: "request-doc",
                request_commit_cid: "request-cid",
                reduction_index: 1,
                turn_index: 3,
                parent_reduction_key: None,
                producer_call: None,
                source_boundary: &source,
                compacted_prefix: &prefix,
                retained_suffix: &suffix,
                checkpoint_messages: &checkpoint,
                summary: "summary",
                original_tokens: 100,
                compacted_tokens: 20,
            },
        )
        .await
        .unwrap();
        let redelivery = persist(
            &node,
            NewProviderContextReduction {
                agent_did: "did:key:agent",
                requester_did: Some("did:key:user"),
                session_id: "session",
                request_id: "request",
                request_doc_id: "request-doc",
                request_commit_cid: "request-cid",
                reduction_index: 1,
                turn_index: 3,
                parent_reduction_key: None,
                producer_call: None,
                source_boundary: &source,
                compacted_prefix: &prefix,
                retained_suffix: &suffix,
                checkpoint_messages: &checkpoint,
                summary: "summary",
                original_tokens: 100,
                compacted_tokens: 20,
            },
        )
        .await
        .unwrap();
        assert_eq!(first.doc_id, redelivery.doc_id);

        let conflicting_checkpoint = checkpoint_from_suffix(&suffix, "different");
        let conflict = persist(
            &node,
            NewProviderContextReduction {
                agent_did: "did:key:agent",
                requester_did: Some("did:key:user"),
                session_id: "session",
                request_id: "request",
                request_doc_id: "request-doc",
                request_commit_cid: "request-cid",
                reduction_index: 1,
                turn_index: 3,
                parent_reduction_key: None,
                producer_call: None,
                source_boundary: &source,
                compacted_prefix: &prefix,
                retained_suffix: &suffix,
                checkpoint_messages: &conflicting_checkpoint,
                summary: "different",
                original_tokens: 100,
                compacted_tokens: 20,
            },
        )
        .await
        .unwrap_err();
        assert!(conflict.to_string().contains("conflicting immutable facts"));

        let restored = load_unconsumed_for_request(&node, "request-doc")
            .await
            .unwrap()
            .expect("persisted checkpoint is unconsumed");
        assert_eq!(restored.0.checkpoint_messages().unwrap(), checkpoint);
        assert_eq!(restored.1, vec![first.reduction_key.clone()]);

        let second_checkpoint = checkpoint_from_suffix(&suffix, "summary 2");
        let mut second_source = source.clone();
        second_source.request_commit_cid = "request-cid-after-reclaim".to_string();
        let second = persist(
            &node,
            NewProviderContextReduction {
                agent_did: "did:key:agent",
                requester_did: Some("did:key:user"),
                session_id: "session",
                request_id: "request",
                request_doc_id: "request-doc",
                request_commit_cid: "request-cid-after-reclaim",
                reduction_index: 2,
                turn_index: 4,
                parent_reduction_key: Some(&first.reduction_key),
                producer_call: None,
                source_boundary: &second_source,
                compacted_prefix: &prefix,
                retained_suffix: &suffix,
                checkpoint_messages: &second_checkpoint,
                summary: "summary 2",
                original_tokens: 110,
                compacted_tokens: 18,
            },
        )
        .await
        .unwrap();
        let chain = load_for_request(&node, "request-doc").await.unwrap();
        assert_eq!(chain.len(), 2);
        assert_eq!(
            chain[1].parent_reduction_key.as_deref(),
            Some(first.reduction_key.as_str())
        );
        let restored = load_unconsumed_for_request(&node, "request-doc")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(restored.0.reduction_key, second.reduction_key);
        assert_eq!(
            restored.0.active_reduction_keys(),
            vec![second.reduction_key.clone()]
        );
        assert_eq!(
            restored.1,
            vec![first.reduction_key.clone(), second.reduction_key]
        );

        let title_manifest = gents_protocol::rendered_request::ProvenanceManifest::captured_only(
            "title.1".to_string(),
            None,
            None,
            gents_protocol::rendered_request::AssemblyTrace::from_effective_messages(
                gents_protocol::rendered_request::AssemblyBuildPath::Budgeted,
                vec![Message::user("title")],
            ),
        );
        let title_provenance =
            escape_graphql_string(&serde_json::to_string(&title_manifest).unwrap());
        let title_capture = format!(
            r#"mutation {{ create_RenderedRequest(input: {{
                capture_key: "title-after-reduction"
                request_doc_id: "request-doc"
                request_commit_cid: "request-cid-after-reclaim"
                request_id: "request"
                session_id: "session"
                agent_did: "did:key:agent"
                requester_did: "did:key:user"
                behavior_id: "behavior"
                capture_scope: "title.1"
                turn_index: 99
                attempt: 0
                capture_version: 1
                model_name: "model"
                source: "openai_responses"
                request_json: "{{}}"
                provenance_json: "{title_provenance}"
                created_at: "2200-08-14T00:00:00Z"
            }}) {{ _docID }} }}"#
        );
        let response = node.execute(&title_capture).await;
        assert!(
            !response.has_errors(),
            "title seed failed: {:?}",
            response.errors
        );
        assert!(load_unconsumed_for_request(&node, "request-doc")
            .await
            .unwrap()
            .is_some());

        let unsupported_provenance = escape_graphql_string(
            &json!({
                "manifest_version": 999,
                "reduction_key": restored.0.reduction_key,
            })
            .to_string(),
        );
        let unsupported_capture = format!(
            r#"mutation {{ create_RenderedRequest(input: {{
                capture_key: "unsupported-inference-after-reduction"
                request_doc_id: "request-doc"
                request_commit_cid: "request-cid-after-reclaim"
                request_id: "request"
                session_id: "session"
                agent_did: "did:key:agent"
                requester_did: "did:key:user"
                behavior_id: "behavior"
                capture_scope: "inference.1"
                turn_index: 4
                attempt: 0
                capture_version: 1
                model_name: "model"
                source: "openai_responses"
                request_json: "{{}}"
                provenance_json: "{unsupported_provenance}"
                created_at: "2026-08-14T00:00:00Z"
            }}) {{ _docID }} }}"#
        );
        let response = node.execute(&unsupported_capture).await;
        assert!(
            !response.has_errors(),
            "unsupported inference seed failed: {:?}",
            response.errors
        );
        let missing_provenance_capture = r#"mutation { create_RenderedRequest(input: {
            capture_key: "missing-provenance-after-reduction"
            request_doc_id: "request-doc"
            request_commit_cid: "request-cid-after-reclaim"
            request_id: "request"
            session_id: "session"
            agent_did: "did:key:agent"
            requester_did: "did:key:user"
            behavior_id: "behavior"
            capture_scope: "inference.1"
            turn_index: 4
            attempt: 1
            capture_version: 1
            model_name: "model"
            source: "openai_responses"
            request_json: "{}"
            created_at: "2026-08-14T00:00:01Z"
        }) { _docID } }"#;
        let response = node.execute(missing_provenance_capture).await;
        assert!(
            !response.has_errors(),
            "missing-provenance inference seed failed: {:?}",
            response.errors
        );
        assert!(
            load_unconsumed_for_request(&node, "request-doc")
                .await
                .unwrap()
                .is_some(),
            "unsupported and incomplete captures are not consumption evidence"
        );

        let coexistence = node
            .execute(
                r#"{ CompactionEntry(filter: { session_id: { _eq: "session" } }) { compaction_key }
                    ProviderContextReduction(filter: { session_id: { _eq: "session" } }) { reduction_key } }"#,
            )
            .await;
        assert!(
            !coexistence.has_errors(),
            "coexistence query failed: {:?}",
            coexistence.errors
        );
        let coexistence = coexistence.data.unwrap();
        assert_eq!(coexistence["CompactionEntry"].as_array().unwrap().len(), 1);
        assert_eq!(
            coexistence["ProviderContextReduction"]
                .as_array()
                .unwrap()
                .len(),
            2
        );

        let manifest = gents_protocol::rendered_request::ProvenanceManifest::captured_only(
            "inference.1".to_string(),
            None,
            None,
            gents_protocol::rendered_request::AssemblyTrace::from_effective_messages(
                gents_protocol::rendered_request::AssemblyBuildPath::Budgeted,
                second_checkpoint,
            )
            .with_reduction_keys(vec![restored.0.reduction_key.clone()]),
        );
        let provenance = escape_graphql_string(&serde_json::to_string(&manifest).unwrap());
        let rendered = format!(
            r#"mutation {{ create_RenderedRequest(input: {{
                capture_key: "capture-after-reduction"
                request_doc_id: "request-doc"
                request_commit_cid: "request-cid"
                request_id: "request"
                session_id: "session"
                agent_did: "did:key:agent"
                requester_did: "did:key:user"
                behavior_id: "behavior"
                capture_scope: "inference.1"
                turn_index: 4
                attempt: 2
                capture_version: 1
                model_name: "model"
                source: "openai_responses"
                request_json: "{{}}"
                provenance_json: "{provenance}"
                created_at: "1900-08-14T00:00:00Z"
            }}) {{ _docID }} }}"#
        );
        let response = node.execute(&rendered).await;
        assert!(
            !response.has_errors(),
            "rendered seed failed: {:?}",
            response.errors
        );
        assert!(load_unconsumed_for_request(&node, "request-doc")
            .await
            .unwrap()
            .is_none());

        assert!(load_for_request(&node, "fork-request-doc")
            .await
            .unwrap()
            .is_empty());
        assert_ne!(
            reduction_key("did:key:agent", "session", "request-doc", 3, 1).unwrap(),
            reduction_key("did:key:agent", "session", "concurrent-request-doc", 3, 1).unwrap()
        );
        assert_ne!(
            reduction_key("did:key:agent", "session", "request-doc", 3, 1).unwrap(),
            reduction_key("did:key:agent", "fork-session", "fork-request-doc", 3, 1).unwrap()
        );
    }

    #[tokio::test]
    async fn persistence_rejects_a_split_through_a_tool_pair() {
        let node = EmbeddedNode::builder().build().await.unwrap();
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let call = Message::Assistant {
            id: None,
            content: vec![AssistantContent::ToolCall(ToolCall {
                id: "call-1".to_string(),
                call_id: Some("call-1".to_string()),
                function: ToolFunction {
                    name: "read".to_string(),
                    arguments: Value::Object(Default::default()),
                },
                signature: None,
                additional_params: None,
            })],
        };
        let result = Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                id: "call-1".to_string(),
                call_id: Some("call-1".to_string()),
                content: vec![ToolResultContent::Text(Text {
                    text: "done".to_string(),
                })],
            })],
        };
        let source = boundary("request-doc-pair");
        let error = persist(
            &node,
            NewProviderContextReduction {
                agent_did: "did:key:agent",
                requester_did: None,
                session_id: "session",
                request_id: "request-pair",
                request_doc_id: "request-doc-pair",
                request_commit_cid: "request-cid",
                reduction_index: 1,
                turn_index: 0,
                parent_reduction_key: None,
                producer_call: None,
                source_boundary: &source,
                compacted_prefix: &[call],
                retained_suffix: &[result.clone()],
                checkpoint_messages: &[result],
                summary: "summary",
                original_tokens: 10,
                compacted_tokens: 5,
            },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("open tool-call/result pair"));
    }

    #[test]
    fn recovery_treats_the_stored_checkpoint_text_as_authoritative() {
        let suffix = vec![Message::user("current")];
        let checkpoint = [
            vec![crate::prompt::LayeredPromptBuilder::system_reminder(
                "wording from an older runtime",
            )],
            suffix.clone(),
        ]
        .concat();
        assert!(checkpoint_matches_stored_projection(
            &checkpoint,
            &suffix,
            "durable summary"
        ));
    }

    #[tokio::test]
    async fn source_boundary_pins_bounded_canonical_high_water() {
        let node = EmbeddedNode::builder().build().await.unwrap();
        crate::ensure_runtime_schemas(&node).await.unwrap();
        for sequence in [2, 1] {
            let mutation = format!(
                r#"mutation {{ create_AgentMessage(input: {{
                    message_key: "session-boundary:{sequence}"
                    session_id: "session-boundary"
                    sequence: {sequence}
                    role: "user"
                    content: "message {sequence}"
                    timestamp: "2026-08-14T00:00:0{sequence}Z"
                }}) {{ _docID }} }}"#
            );
            let response = node.execute(&mutation).await;
            assert!(
                !response.has_errors(),
                "message seed failed: {:?}",
                response.errors
            );
        }
        let boundary =
            capture_source_boundary(&node, "session-boundary", "request-doc", "request-cid")
                .await
                .unwrap();
        let high_water = boundary.canonical_through.expect("canonical high-water");
        assert_eq!(high_water.sequence, 2);
        assert!(!high_water.doc_id.is_empty());
        assert!(!high_water.commit_cid.is_empty());
    }
}
