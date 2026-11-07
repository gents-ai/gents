use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::inference::llm::message::Message;
use crate::store::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use gents_loop::claude_messages_body::{prepare_replay_checkpoint, ReplayTag, TaggedAssistantRow};
use gents_loop::loop_stream::TaggedMessage;

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
    pub canonical_through: Option<TranscriptFactRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducerCallRef {
    pub call_id: String,
    pub call_seq: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplayRowAssociation {
    pub source: Option<ReplayTag>,
    pub physical_header: Option<String>,
    pub block_indices: Vec<usize>,
}

impl ReplayRowAssociation {
    fn from_tagged(row: &TaggedMessage) -> Self {
        Self {
            source: row.source.clone(),
            physical_header: row.physical_header.clone(),
            block_indices: row.block_indices.clone(),
        }
    }

    fn attach(self, message: Message) -> TaggedMessage {
        TaggedMessage {
            message,
            source: self.source,
            physical_header: self.physical_header,
            block_indices: self.block_indices,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplayAssociations {
    pub required: Vec<ReplayTag>,
    pub prefix_rows: Vec<ReplayRowAssociation>,
    pub retained_rows: Vec<ReplayRowAssociation>,
}

impl ReplayAssociations {
    pub(crate) fn from_tagged_split(
        required: Vec<ReplayTag>,
        prefix: &[TaggedMessage],
        suffix: &[TaggedMessage],
    ) -> Self {
        Self {
            required,
            prefix_rows: prefix
                .iter()
                .map(ReplayRowAssociation::from_tagged)
                .collect(),
            retained_rows: suffix
                .iter()
                .map(ReplayRowAssociation::from_tagged)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderContextReduction {
    #[serde(default, rename = "_docID")]
    pub doc_id: String,
    pub reduction_key: String,
    pub node_did: String,
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
    pub replay_associations_json: String,
    pub summary: String,
    pub messages_compacted: i64,
    pub original_tokens: i64,
    pub compacted_tokens: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProviderReductionDisplay {
    reduction_key: String,
    node_did: String,
    requester_did: String,
    session_id: String,
    request_doc_id: String,
    canonical_through_sequence: Option<i64>,
    summary: String,
    messages_compacted: i64,
    original_tokens: i64,
    compacted_tokens: i64,
    created_at: String,
}

impl ProviderReductionDisplay {
    fn create_mutation(&self) -> String {
        format!(
            r#"mutation {{ create_ProviderReductionDisplay(input: {{ reduction_key: "{}", node_did: "{}", requester_did: "{}", session_id: "{}", request_doc_id: "{}", canonical_through_sequence: {}, summary: "{}", messages_compacted: {}, original_tokens: {}, compacted_tokens: {}, created_at: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(&self.reduction_key),
            escape_graphql_string(&self.node_did),
            escape_graphql_string(&self.requester_did),
            escape_graphql_string(&self.session_id),
            escape_graphql_string(&self.request_doc_id),
            self.canonical_through_sequence
                .map(|n| n.to_string())
                .unwrap_or_else(|| "null".to_string()),
            escape_graphql_string(&self.summary),
            self.messages_compacted,
            self.original_tokens,
            self.compacted_tokens,
            escape_graphql_string(&self.created_at),
        )
    }

    pub(crate) fn from_reduction(row: &ProviderContextReduction) -> Result<Option<Self>> {
        if !row.pair_closed {
            return Ok(None);
        }
        let Some(requester) = row
            .requester_did
            .as_deref()
            .filter(|did| !did.trim().is_empty())
        else {
            return Ok(None);
        };
        if row.node_did.trim().is_empty()
            || row.session_id.trim().is_empty()
            || row.reduction_key.trim().is_empty()
        {
            anyhow::bail!("reduction display scope is incomplete");
        }
        validate_display_key(
            &row.reduction_key,
            &row.node_did,
            &row.session_id,
            &row.request_doc_id,
            row.turn_index,
            row.reduction_index,
        )?;
        let boundary = row.source_boundary()?;
        validate_source_boundary(&boundary, &row.request_doc_id, &row.request_commit_cid)?;
        Ok(Some(Self {
            reduction_key: row.reduction_key.clone(),
            node_did: row.node_did.clone(),
            requester_did: requester.to_owned(),
            session_id: row.session_id.clone(),
            request_doc_id: row.request_doc_id.clone(),
            canonical_through_sequence: boundary.canonical_through.map(|fact| fact.sequence),
            summary: row.summary.clone(),
            messages_compacted: row.messages_compacted,
            original_tokens: row.original_tokens,
            compacted_tokens: row.compacted_tokens,
            created_at: row.created_at.clone(),
        }))
    }

    pub(crate) async fn ensure_in_txn(
        &self,
        txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    ) -> Result<()> {
        let query = format!(
            r#"{{ ProviderReductionDisplay(filter: {{ reduction_key: {{ _eq: "{}" }} }}) {{ reduction_key node_did requester_did session_id request_doc_id canonical_through_sequence summary messages_compacted original_tokens compacted_tokens created_at }} }}"#,
            escape_graphql_string(&self.reduction_key)
        );
        let response = txn.execute(&query).await?;
        let rows = response
            .pointer("/data/ProviderReductionDisplay")
            .and_then(Value::as_array)
            .context("ProviderReductionDisplay response omitted its result array")?;
        match rows.as_slice() {
            [] => {
                txn.execute(&self.create_mutation()).await?;
            }
            [existing] => {
                let existing: Self = serde_json::from_value(existing.clone())?;
                anyhow::ensure!(
                    &existing == self,
                    "conflicting reduction display observation"
                );
            }
            _ => anyhow::bail!("multiple reduction display observations for one key"),
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct DisplayBackfillSource {
    #[serde(rename = "_docID")]
    doc_id: String,
    reduction_key: String,
    node_did: String,
    requester_did: Option<String>,
    session_id: String,
    request_doc_id: String,
    request_commit_cid: String,
    turn_index: i64,
    reduction_index: i64,
    source_boundary_json: Option<String>,
    pair_closed: Option<bool>,
    summary: String,
    messages_compacted: i64,
    original_tokens: i64,
    compacted_tokens: i64,
    created_at: String,
}

/// Process one bounded page using the database cursor. A caller may restart at
/// the beginning after interruption.
pub(crate) async fn backfill_display_batch(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    after: Option<&str>,
) -> Result<Option<String>> {
    let filter = format!(
        r#"node_did: {{ _eq: "{}" }}"#,
        escape_graphql_string(node_did)
    );
    let query = crate::store::graphql::cursor_query(
        "ProviderContextReduction",
        "_docID reduction_key node_did requester_did session_id request_doc_id request_commit_cid turn_index reduction_index source_boundary_json pair_closed summary messages_compacted original_tokens compacted_tokens created_at",
        &filter,
        after,
    );
    crate::store::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "provider_context.backfill_display",
        |txn| {
            let query = &query;
            Box::pin(async move {
                let response = txn.execute(query).await?;
                let (rows, next) = crate::store::graphql::cursor_page_data::<Value>(
                    response.get("data"), "ProviderContextReduction",
                )?;
                anyhow::ensure!(rows.len() <= crate::store::graphql::CURSOR_PAGE_SIZE, "reduction display backfill page overflow");
                let mut ids = std::collections::BTreeSet::new();
                for row in &rows {
                    let id = row.get("_docID").and_then(Value::as_str).filter(|id| !id.is_empty())
                        .context("reduction display backfill row has no document ID")?;
                    anyhow::ensure!(ids.insert(id.to_owned()), "reduction display backfill page has repeated document IDs");
                }
                anyhow::ensure!(next.as_ref().is_none_or(|value| !value.is_empty()), "reduction display backfill cursor is empty");
                let keys: std::collections::BTreeSet<_> = rows.iter().filter_map(|row| {
                    row.get("reduction_key").and_then(Value::as_str).filter(|key| !key.is_empty())
                }).collect();
                let mut key_counts = std::collections::BTreeMap::<String, usize>::new();
                if !keys.is_empty() {
                    let literals = keys.iter().map(|key| format!("\"{}\"", escape_graphql_string(key))).collect::<Vec<_>>().join(", ");
                    let filter = format!("reduction_key: {{ _in: [{literals}] }}");
                    let mut cursor = txn.cursor_scan("ProviderContextReduction", "_docID reduction_key", &filter);
                    while let Some(page) = cursor.next_page().await? {
                        for row in page {
                            let key = row.get("reduction_key").and_then(Value::as_str).context("reduction key lookup omitted its key")?;
                            *key_counts.entry(key.to_owned()).or_default() += 1;
                        }
                    }
                }
                for value in rows {
                    let row: DisplayBackfillSource = match serde_json::from_value(value.clone()) {
                        Ok(row) => row,
                        Err(error) => {
                            tracing::warn!(doc_id = ?value.get("_docID"), %error, "skipping malformed reduction display source");
                            continue;
                        }
                    };
                    if row.pair_closed != Some(true) {
                        tracing::warn!(doc_id = %row.doc_id, "skipping reduction without a closed tool-call boundary");
                        continue;
                    }
                    if key_counts.get(&row.reduction_key) != Some(&1) {
                        tracing::warn!(doc_id = %row.doc_id, "skipping reduction with ambiguous logical key");
                        continue;
                    }
                    let Some(requester) = row.requester_did.filter(|did| !did.trim().is_empty())
                    else {
                        continue;
                    };
                    if row.node_did.trim().is_empty()
                        || row.session_id.trim().is_empty()
                        || row.reduction_key.trim().is_empty()
                    {
                        tracing::warn!(doc_id = %row.doc_id, "skipping reduction with incomplete display scope");
                        continue;
                    }
                    if let Err(error) = validate_display_key(&row.reduction_key, &row.node_did, &row.session_id, &row.request_doc_id, row.turn_index, row.reduction_index) {
                        tracing::warn!(doc_id = %row.doc_id, %error, "skipping reduction with invalid identity");
                        continue;
                    }
                    let Some(boundary_json) = row.source_boundary_json.as_deref() else {
                        tracing::warn!(doc_id = %row.doc_id, "skipping reduction with missing display boundary");
                        continue;
                    };
                    let boundary: SourceBoundary = match serde_json::from_str(boundary_json) {
                        Ok(boundary) => boundary,
                        Err(error) => {
                            tracing::warn!(doc_id = %row.doc_id, %error, "skipping reduction with malformed display boundary");
                            continue;
                        }
                    };
                    if let Err(error) = validate_source_boundary(&boundary, &row.request_doc_id, &row.request_commit_cid) {
                        tracing::warn!(doc_id = %row.doc_id, %error, "skipping reduction with invalid display boundary");
                        continue;
                    }
                    ProviderReductionDisplay {
                        reduction_key: row.reduction_key,
                        node_did: row.node_did,
                        requester_did: requester,
                        session_id: row.session_id,
                        request_doc_id: row.request_doc_id,
                        canonical_through_sequence: boundary
                            .canonical_through
                            .map(|fact| fact.sequence),
                        summary: row.summary,
                        messages_compacted: row.messages_compacted,
                        original_tokens: row.original_tokens,
                        compacted_tokens: row.compacted_tokens,
                        created_at: row.created_at,
                    }
                    .ensure_in_txn(txn)
                    .await?;
                }
                Ok::<_, anyhow::Error>(next)
            })
        },
    )
    .await
}

fn validate_display_key(
    key: &str,
    node: &str,
    session: &str,
    request: &str,
    turn: i64,
    ordinal: i64,
) -> Result<()> {
    let turn = usize::try_from(turn).context("negative reduction turn index")?;
    let ordinal = usize::try_from(ordinal).context("negative reduction index")?;
    anyhow::ensure!(
        ordinal > 0 && reduction_key(node, session, request, turn, ordinal)? == key,
        "reduction key does not match its identity"
    );
    Ok(())
}

pub(crate) async fn run_display_backfill(
    node: std::sync::Arc<EmbeddedNode>,
    node_did: String,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<()> {
    let mut after = None;
    let mut seen_cursors = std::collections::BTreeSet::new();
    loop {
        if cancel.is_cancelled() {
            return Ok(());
        }
        match backfill_display_batch(&node, &node_did, after.as_deref()).await {
            Ok(Some(next)) => {
                if !seen_cursors.insert(next.clone()) {
                    anyhow::bail!("reduction display backfill cursor did not advance");
                }
                after = Some(next);
                tokio::task::yield_now().await;
            }
            Ok(None) => {
                cancel.cancelled().await;
                return Ok(());
            }
            Err(error) => {
                tracing::warn!(%error, "reduction display backfill failed; retrying");
                tokio::select! {
                    _ = cancel.cancelled() => return Ok(()),
                    _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {}
                }
            }
        }
    }
}

impl ProviderContextReduction {
    pub fn checkpoint_messages(&self) -> Result<Vec<Message>> {
        let suffix: Vec<Message> = serde_json::from_str(&self.retained_suffix_json)
            .context("decoding ProviderContextReduction retained_suffix_json")?;
        decode_checkpoint(&self.checkpoint_messages_json, &suffix, &self.summary)
    }

    pub(crate) fn replay_associations(&self) -> Result<ReplayAssociations> {
        serde_json::from_str(&self.replay_associations_json)
            .context("decoding ProviderContextReduction replay_associations_json")
    }

    pub(crate) fn checkpoint_tagged_messages(&self) -> Result<Vec<TaggedMessage>> {
        tagged_checkpoint(
            &self.compacted_prefix_json,
            &self.retained_suffix_json,
            &self.replay_associations_json,
            &self.checkpoint_messages_json,
            &self.summary,
            &self.request_doc_id,
        )
    }

    pub fn source_boundary(&self) -> Result<SourceBoundary> {
        serde_json::from_str(&self.source_boundary_json)
            .context("decoding ProviderContextReduction source_boundary_json")
    }

    pub fn active_reduction_keys(&self) -> Vec<String> {
        vec![self.reduction_key.clone()]
    }
}

#[derive(Debug)]
pub(crate) struct NewProviderContextReduction<'a> {
    pub node_did: &'a str,
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
    pub replay_associations: &'a ReplayAssociations,
    pub summary: &'a str,
    pub original_tokens: usize,
    pub compacted_tokens: usize,
}

pub(crate) struct NewExactProviderContextReduction<'a> {
    pub(crate) node_did: &'a str,
    pub(crate) requester_did: Option<&'a str>,
    pub(crate) session_id: &'a str,
    pub(crate) request_id: &'a str,
    pub(crate) request_doc_id: &'a str,
    pub(crate) request_commit_cid: &'a str,
    pub(crate) reduction_index: usize,
    pub(crate) turn_index: usize,
    pub(crate) parent_reduction_key: Option<&'a str>,
    pub(crate) producer_call: Option<&'a ProducerCallRef>,
    pub(crate) source_boundary: &'a SourceBoundary,
    pub(crate) replay_associations: &'a ReplayAssociations,
    pub(crate) original_tokens: usize,
    pub(crate) compacted_tokens: usize,
}

fn tagged_checkpoint(
    compacted_prefix_json: &str,
    retained_suffix_json: &str,
    replay_associations_json: &str,
    checkpoint_messages_json: &str,
    summary: &str,
    request_doc_id: &str,
) -> Result<Vec<TaggedMessage>> {
    let prefix: Vec<Message> = serde_json::from_str(compacted_prefix_json)
        .context("decoding ProviderContextReduction compacted_prefix_json")?;
    let suffix: Vec<Message> = serde_json::from_str(retained_suffix_json)
        .context("decoding ProviderContextReduction retained_suffix_json")?;
    let associations: ReplayAssociations = serde_json::from_str(replay_associations_json)
        .context("decoding ProviderContextReduction replay_associations_json")?;
    validate_replay_associations(&associations, &prefix, &suffix, request_doc_id)?;
    let checkpoint = decode_checkpoint(checkpoint_messages_json, &suffix, summary)?;
    let mut tagged = Vec::with_capacity(checkpoint.len());
    if !summary.trim().is_empty() {
        tagged.push(TaggedMessage::unassociated(checkpoint[0].clone()));
    }
    tagged.extend(
        suffix
            .into_iter()
            .zip(associations.retained_rows)
            .map(|(message, association)| association.attach(message)),
    );
    Ok(tagged)
}

/// A validated reduction that has not been written. Its checkpoint is the
/// provider view the loop measures; `commit` writes the row, and dropping it
/// discards the reduction, so a rebuilt request that cannot fit leaves no row
/// (`Compaction.ReductionEngine.commitRequestLocalAfterRebuild`).
#[derive(Debug)]
pub(crate) struct StagedProviderContextReduction {
    intended: IntendedReduction,
}

impl StagedProviderContextReduction {
    pub(crate) fn reduction_key(&self) -> &str {
        &self.intended.reduction_key
    }

    pub(crate) fn checkpoint_tagged_messages(&self) -> Result<Vec<TaggedMessage>> {
        let intended = &self.intended;
        tagged_checkpoint(
            &intended.compacted_prefix_json,
            &intended.retained_suffix_json,
            &intended.replay_associations_json,
            &intended.checkpoint_messages_json,
            &intended.summary,
            &intended.request_doc_id,
        )
    }

    /// The loop's turn outcome for this staged reduction: the checkpoint is
    /// measured in memory and the row is written only by the loop's commit.
    pub(crate) fn into_turn_outcome(
        self,
        node: std::sync::Arc<EmbeddedNode>,
    ) -> Result<gents_loop::loop_stream::TurnCompactionOutcome> {
        let messages = self.checkpoint_tagged_messages()?;
        let reduction_key = self.reduction_key().to_string();
        Ok(gents_loop::loop_stream::TurnCompactionOutcome::Reduced {
            messages,
            reduction_key,
            commit: gents_loop::loop_stream::TurnReductionCommit::new(move || async move {
                self.commit(&node).await.map(drop).map_err(|error| {
                    gents_loop::loop_stream::TurnCompactionError::Compactor(error.into())
                })
            }),
        })
    }

    pub(crate) async fn commit(
        self,
        node: &std::sync::Arc<EmbeddedNode>,
    ) -> Result<ProviderContextReduction> {
        let intended = self.intended;
        let reduction_key = intended.reduction_key.clone();
        match load_by_key(node, &reduction_key).await?.as_slice() {
            [] => {}
            [existing] => {
                intended.ensure_matches(existing)?;
                if let Some(display) = ProviderReductionDisplay::from_reduction(existing)? {
                    crate::store::config_client::ConfigAccess::transact_local_write(
                        node,
                        None,
                        "provider_context.recover_display",
                        |txn| Box::pin(async { display.ensure_in_txn(txn).await }),
                    )
                    .await?;
                }
                return Ok(existing.clone());
            }
            twins => anyhow::bail!(
                "provider-context reduction key {reduction_key} has {} visible logical twins",
                twins.len()
            ),
        }

        let created_at = chrono::Utc::now().to_rfc3339();
        let producer_call_id = graphql_optional_string(intended.producer_call_id.as_deref());
        let producer_call_seq = intended
            .producer_call_seq
            .map(|seq| seq.to_string())
            .unwrap_or_else(|| "null".to_string());
        let parent_reduction_key =
            graphql_optional_string(intended.parent_reduction_key.as_deref());
        let requester_did = graphql_optional_string(intended.requester_did.as_deref());
        let display = match intended.requester_did.as_deref() {
            Some(requester) if !requester.trim().is_empty() => {
                let boundary: SourceBoundary = serde_json::from_str(&intended.source_boundary_json)
                    .context("decoding reduction display boundary")?;
                validate_source_boundary(
                    &boundary,
                    &intended.request_doc_id,
                    &intended.request_commit_cid,
                )?;
                Some(ProviderReductionDisplay {
                    reduction_key: intended.reduction_key.clone(),
                    node_did: intended.node_did.clone(),
                    requester_did: requester.to_owned(),
                    session_id: intended.session_id.clone(),
                    request_doc_id: intended.request_doc_id.clone(),
                    canonical_through_sequence: boundary
                        .canonical_through
                        .map(|fact| fact.sequence),
                    summary: intended.summary.clone(),
                    messages_compacted: intended.messages_compacted,
                    original_tokens: intended.original_tokens,
                    compacted_tokens: intended.compacted_tokens,
                    created_at: created_at.clone(),
                })
            }
            _ => None,
        };
        crate::store::config_client::ConfigAccess::transact_local_write(
            node,
            None,
            "provider_context.create_reduction",
            |txn| {
                let intended = &intended;
                let created_at = &created_at;
                let producer_call_id = &producer_call_id;
                let producer_call_seq = &producer_call_seq;
                let parent_reduction_key = &parent_reduction_key;
                let requester_did = &requester_did;
                let display = &display;
                Box::pin(async move {
                    let compacted_prefix_json =
                        crate::inference::rendered_request::sink::store_shared_json_in_txn(
                            txn,
                            &intended.compacted_prefix_json,
                        )
                        .await?;
                    let retained_suffix_json =
                        crate::inference::rendered_request::sink::store_shared_json_in_txn(
                            txn,
                            &intended.retained_suffix_json,
                        )
                        .await?;
                    let mutation = format!(
                        r#"mutation {{
            create_ProviderContextReduction(input: {{
                reduction_key: "{reduction_key}"
                node_did: "{node_did}"
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
                replay_associations_json: "{replay_associations_json}"
                summary: "{summary}"
                messages_compacted: {messages_compacted}
                original_tokens: {original_tokens}
                compacted_tokens: {compacted_tokens}
                created_at: "{created_at}"
            }}) {{ _docID }}
        }}"#,
                        reduction_key = escape_graphql_string(&intended.reduction_key),
                        node_did = escape_graphql_string(&intended.node_did),
                        session_id = escape_graphql_string(&intended.session_id),
                        request_id = escape_graphql_string(&intended.request_id),
                        request_doc_id = escape_graphql_string(&intended.request_doc_id),
                        request_commit_cid = escape_graphql_string(&intended.request_commit_cid),
                        reduction_index = intended.reduction_index,
                        turn_index = intended.turn_index,
                        source_boundary_json =
                            escape_graphql_string(&intended.source_boundary_json),
                        compacted_prefix_json = escape_graphql_string(&compacted_prefix_json),
                        retained_suffix_json = escape_graphql_string(&retained_suffix_json),
                        checkpoint_messages_json =
                            escape_graphql_string(&intended.checkpoint_messages_json),
                        replay_associations_json =
                            escape_graphql_string(&intended.replay_associations_json),
                        summary = escape_graphql_string(&intended.summary),
                        messages_compacted = intended.messages_compacted,
                        original_tokens = intended.original_tokens,
                        compacted_tokens = intended.compacted_tokens,
                    );
                    txn.execute(&mutation).await?;
                    if let Some(display) = display {
                        display.ensure_in_txn(txn).await?;
                    }
                    Ok::<_, anyhow::Error>(())
                })
            },
        )
        .await
        .with_context(|| format!("creating ProviderContextReduction {reduction_key}"))?;

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
}

pub fn reduction_key(
    node_did: &str,
    session_id: &str,
    request_doc_id: &str,
    turn_index: usize,
    reduction_index: usize,
) -> Result<String> {
    let digest = crate::inference::rendered_request::sha256_canonical_json(&json!([
        node_did,
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
    node_did: &str,
    requester_did: Option<&str>,
    request_doc_id: &str,
    request_commit_cid: &str,
) -> Result<SourceBoundary> {
    let session_scope = crate::session::session_scope_filter(node_did, session_id, requester_did);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ {session_scope} }},
                order: {{ sequence: DESC }},
                limit: 1
            ) {{
                _docID
                sequence
            }}
        }}"#,
    );
    let response =
        graphql_with_transaction_retry(node, &query, "capturing provider-context source boundary")
            .await
            .with_context(|| {
                format!("capturing provider-context source boundary for session {session_id}")
            })?;
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
            let commit = crate::store::graphql::newest_document_composite_commit(
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

#[cfg(test)]
pub(crate) async fn persist(
    node: &std::sync::Arc<EmbeddedNode>,
    input: NewProviderContextReduction<'_>,
) -> Result<ProviderContextReduction> {
    stage(input)?.commit(node).await
}

pub(crate) fn stage(
    input: NewProviderContextReduction<'_>,
) -> Result<StagedProviderContextReduction> {
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
    let pair_closed = crate::inference::compaction::pair_safe_boundary(
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
    validate_replay_associations(
        input.replay_associations,
        input.compacted_prefix,
        input.retained_suffix,
        input.request_doc_id,
    )?;
    let reduction_key = reduction_key(
        input.node_did,
        input.session_id,
        input.request_doc_id,
        input.turn_index,
        input.reduction_index,
    )?;
    Ok(StagedProviderContextReduction {
        intended: IntendedReduction::from_input(&input, reduction_key)?,
    })
}

#[cfg(test)]
#[path = "../../tests/conformance/durable_reduction.rs"]
mod durable_reduction_conformance;

pub(crate) fn stage_exact(
    input: NewExactProviderContextReduction<'_>,
    reduction: crate::inference::compaction::ExactReduction<'_>,
) -> Result<StagedProviderContextReduction> {
    let checkpoint_messages = reduction.checkpoint_messages();
    stage(NewProviderContextReduction {
        node_did: input.node_did,
        requester_did: input.requester_did,
        session_id: input.session_id,
        request_id: input.request_id,
        request_doc_id: input.request_doc_id,
        request_commit_cid: input.request_commit_cid,
        reduction_index: input.reduction_index,
        turn_index: input.turn_index,
        parent_reduction_key: input.parent_reduction_key,
        producer_call: input.producer_call,
        source_boundary: input.source_boundary,
        compacted_prefix: reduction.compacted_prefix,
        retained_suffix: reduction.retained_suffix,
        checkpoint_messages: &checkpoint_messages,
        replay_associations: input.replay_associations,
        summary: reduction.checkpoint,
        original_tokens: input.original_tokens,
        compacted_tokens: input.compacted_tokens,
    })
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
    let response =
        graphql_with_transaction_retry(node, &query, "loading ProviderContextReduction by request")
            .await
            .with_context(|| {
                format!("loading ProviderContextReduction for request {request_doc_id}")
            })?;
    let mut rows: Vec<ProviderContextReduction> = serde_json::from_value(
        response
            .data
            .as_ref()
            .and_then(|data| data.get("ProviderContextReduction"))
            .cloned()
            .unwrap_or_else(|| json!([])),
    )?;
    hydrate_rows(node, &mut rows).await?;
    validate_chain(&rows)?;
    validate_recoverable_rows(&rows)?;
    Ok(rows)
}

/// Restore only the crash cut this fact closes; once captured for a provider
/// call, a restarted request deliberately derives from canonical history
/// instead of pretending the call outcome is known.
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
    let response = graphql_with_transaction_retry(
        node,
        &rendered,
        "checking ProviderContextReduction consumption",
    )
    .await
    .with_context(|| {
        format!("checking ProviderContextReduction consumption for request {request_doc_id}")
    })?;
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

const REDUCTION_FIELDS: &str = "_docID reduction_key node_did requester_did session_id request_id request_doc_id request_commit_cid reduction_index turn_index parent_reduction_key producer_call_id producer_call_seq source_boundary_json compacted_prefix_json retained_suffix_json pair_closed checkpoint_messages_json replay_associations_json summary messages_compacted original_tokens compacted_tokens created_at";

async fn load_by_key(
    node: &EmbeddedNode,
    reduction_key: &str,
) -> Result<Vec<ProviderContextReduction>> {
    let query = format!(
        r#"{{ ProviderContextReduction(filter: {{ reduction_key: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
        escape_graphql_string(reduction_key),
        REDUCTION_FIELDS
    );
    let response =
        graphql_with_transaction_retry(node, &query, "loading ProviderContextReduction by key")
            .await
            .with_context(|| format!("loading ProviderContextReduction key {reduction_key}"))?;
    let mut rows = serde_json::from_value(
        response
            .data
            .as_ref()
            .and_then(|data| data.get("ProviderContextReduction"))
            .cloned()
            .unwrap_or_else(|| json!([])),
    )
    .context("decoding ProviderContextReduction rows")?;
    hydrate_rows(node, &mut rows).await?;
    Ok(rows)
}

async fn hydrate_rows(node: &EmbeddedNode, rows: &mut Vec<ProviderContextReduction>) -> Result<()> {
    let mut cache = crate::inference::rendered_request::CaptureReadCache::default();
    for row in rows {
        row.compacted_prefix_json = crate::inference::rendered_request::load_shared_json(
            node,
            &row.compacted_prefix_json,
            &mut cache,
        )
        .await?;
        row.retained_suffix_json = crate::inference::rendered_request::load_shared_json(
            node,
            &row.retained_suffix_json,
            &mut cache,
        )
        .await?;
    }
    Ok(())
}

#[derive(Debug)]
struct IntendedReduction {
    reduction_key: String,
    node_did: String,
    requester_did: Option<String>,
    session_id: String,
    request_id: String,
    request_doc_id: String,
    source_boundary_json: String,
    compacted_prefix_json: String,
    retained_suffix_json: String,
    checkpoint_messages_json: String,
    replay_associations_json: String,
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
            node_did: input.node_did.to_string(),
            requester_did: input.requester_did.map(ToOwned::to_owned),
            session_id: input.session_id.to_string(),
            request_id: input.request_id.to_string(),
            request_doc_id: input.request_doc_id.to_string(),
            source_boundary_json: crate::inference::rendered_request::canonical_json_string(
                &serde_json::to_value(input.source_boundary)?,
            )?,
            compacted_prefix_json: crate::inference::rendered_request::canonical_json_string(
                &serde_json::to_value(input.compacted_prefix)?,
            )?,
            retained_suffix_json: crate::inference::rendered_request::canonical_json_string(
                &serde_json::to_value(input.retained_suffix)?,
            )?,
            checkpoint_messages_json: encode_checkpoint(input.checkpoint_messages, input.summary)?,
            replay_associations_json: crate::inference::rendered_request::canonical_json_string(
                &serde_json::to_value(input.replay_associations)?,
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
            && row.node_did == self.node_did
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
            && row.checkpoint_messages()?
                == decode_checkpoint(
                    &self.checkpoint_messages_json,
                    &serde_json::from_str::<Vec<Message>>(&self.retained_suffix_json)?,
                    &self.summary,
                )?
            && row.replay_associations_json == self.replay_associations_json
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
        if row.node_did != first.node_did
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
            &row.node_did,
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
        if crate::inference::compaction::pair_safe_boundary(&source, split) != split {
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
        decode_checkpoint(&row.checkpoint_messages_json, &suffix, &row.summary).with_context(
            || format!("validating ProviderContextReduction {}", row.reduction_key),
        )?;
        validate_replay_associations(
            &row.replay_associations()?,
            &prefix,
            &suffix,
            &row.request_doc_id,
        )?;
    }
    Ok(())
}

pub(crate) fn validate_replay_associations(
    associations: &ReplayAssociations,
    prefix: &[Message],
    suffix: &[Message],
    request_doc_id: &str,
) -> Result<()> {
    anyhow::ensure!(
        associations.prefix_rows.len() == prefix.len()
            && associations.retained_rows.len() == suffix.len(),
        "ProviderContextReduction replay association lengths disagree with the exact native split"
    );
    anyhow::ensure!(
        associations
            .required
            .iter()
            .all(|tag| tag.request_doc_id == request_doc_id),
        "ProviderContextReduction replay association crosses its physical request"
    );
    let mut assistant_rows = Vec::new();
    let mut assistant_split = 0;
    for (messages, sources, in_prefix) in [
        (prefix, associations.prefix_rows.as_slice(), true),
        (suffix, associations.retained_rows.as_slice(), false),
    ] {
        for (message, association) in messages.iter().zip(sources) {
            match message {
                Message::Assistant { id, content, .. } => {
                    assistant_rows.push(TaggedAssistantRow {
                        source: association.source.clone(),
                        physical_header: association.physical_header.clone(),
                        block_indices: association.block_indices.clone(),
                        id: id.clone(),
                        content: content.clone(),
                    });
                    if in_prefix {
                        assistant_split += 1;
                    }
                }
                _ => anyhow::ensure!(
                    association.source.is_none(),
                    "ProviderContextReduction associates a non-assistant native row"
                ),
            }
        }
    }
    prepare_replay_checkpoint(
        associations.required.clone(),
        assistant_rows,
        assistant_split,
    )
    .context("validating ProviderContextReduction canonical replay associations")?;
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointPrefix {
    version: u32,
    prefix: Vec<Message>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredCheckpoint {
    Prefix(CheckpointPrefix),
    Full(Vec<Message>),
}

fn encode_checkpoint(checkpoint: &[Message], summary: &str) -> Result<String> {
    let prefix_len = usize::from(!summary.trim().is_empty());
    Ok(crate::inference::rendered_request::canonical_json_string(
        &serde_json::to_value(CheckpointPrefix {
            version: 1,
            prefix: checkpoint
                .get(..prefix_len)
                .context("checkpoint is missing its rendered summary")?
                .to_vec(),
        })?,
    )?)
}

fn decode_checkpoint(encoded: &str, suffix: &[Message], summary: &str) -> Result<Vec<Message>> {
    let stored: StoredCheckpoint = serde_json::from_str(encoded)
        .context("decoding ProviderContextReduction checkpoint_messages_json")?;
    let checkpoint = match stored {
        StoredCheckpoint::Full(messages) => messages,
        StoredCheckpoint::Prefix(mut stored) => {
            anyhow::ensure!(stored.version == 1, "unsupported checkpoint prefix version");
            anyhow::ensure!(
                stored.prefix.len() == usize::from(!summary.trim().is_empty()),
                "checkpoint prefix disagrees with its summary"
            );
            stored.prefix.extend_from_slice(suffix);
            stored.prefix
        }
    };
    anyhow::ensure!(
        checkpoint_matches_stored_projection(&checkpoint, suffix, summary),
        "ProviderContextReduction checkpoint has invalid stored projection structure"
    );
    Ok(checkpoint)
}

fn checkpoint_from_suffix(suffix: &[Message], summary: &str) -> Vec<Message> {
    let mut checkpoint = suffix.to_vec();
    let summary = summary.trim();
    if !summary.is_empty() {
        checkpoint.insert(
            0,
            crate::turn::prompt::LayeredPromptBuilder::system_reminder(
                &crate::turn::prompt::continuation_checkpoint_reminder(summary),
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

pub(crate) fn validate_source_boundary(
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
    use std::sync::Arc;

    use super::*;
    use crate::inference::llm::message::{
        AssistantContent, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent, UserContent,
    };

    #[test]
    fn retained_display_rejects_wrong_boundaries_and_unscoped_records() {
        let mut row: ProviderContextReduction = serde_json::from_value(json!({
            "reduction_key": "key", "node_did": "node", "requester_did": "requester",
            "session_id": "session", "request_id": "request", "request_doc_id": "doc",
            "request_commit_cid": "request-cid", "reduction_index": 1, "turn_index": 1,
            "source_boundary_json": serde_json::to_string(&boundary("doc")).unwrap(),
            "compacted_prefix_json": "private prefix", "retained_suffix_json": "private suffix",
            "checkpoint_messages_json": "private checkpoint", "replay_associations_json": "private associations",
            "pair_closed": true, "summary": "summary", "messages_compacted": 2,
            "original_tokens": 20, "compacted_tokens": 4, "created_at": "2026-01-01T00:00:00Z"
        })).unwrap();
        row.reduction_key = reduction_key("node", "session", "doc", 1, 1).unwrap();
        let display = ProviderReductionDisplay::from_reduction(&row)
            .unwrap()
            .unwrap();
        assert_eq!(display.canonical_through_sequence, Some(1));
        assert_eq!(display.requester_did, "requester");
        assert!(!display.create_mutation().contains("private prefix"));
        row.pair_closed = false;
        assert!(ProviderReductionDisplay::from_reduction(&row)
            .unwrap()
            .is_none());
        row.pair_closed = true;
        row.request_commit_cid = "different-cid".into();
        assert!(ProviderReductionDisplay::from_reduction(&row).is_err());
        row.requester_did = None;
        assert!(ProviderReductionDisplay::from_reduction(&row)
            .unwrap()
            .is_none());
    }

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
            node_did: "did:a".to_string(),
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
            replay_associations_json: serde_json::to_string(&ReplayAssociations {
                required: Vec::new(),
                prefix_rows: Vec::new(),
                retained_rows: Vec::new(),
            })
            .unwrap(),
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

    fn unassociated_row() -> ReplayRowAssociation {
        ReplayRowAssociation {
            source: None,
            physical_header: None,
            block_indices: Vec::new(),
        }
    }

    fn one_block_row(source: ReplayTag) -> ReplayRowAssociation {
        ReplayRowAssociation {
            source: Some(source),
            physical_header: Some("fixture-header".into()),
            block_indices: vec![0],
        }
    }

    #[test]
    fn replay_associations_bind_exact_native_split_and_assistant_rows() {
        let tag = ReplayTag {
            request_doc_id: "request-doc".into(),
            source: gents_protocol::output::OutputSource::ProviderTurn {
                scope: "inference.1".parse().unwrap(),
                turn_index: 0,
                attempt: 0,
            },
        };
        let prefix = vec![Message::user("prior")];
        let suffix = vec![Message::assistant("current")];
        let associations = ReplayAssociations {
            required: vec![tag.clone()],
            prefix_rows: vec![unassociated_row()],
            retained_rows: vec![one_block_row(tag.clone())],
        };
        assert!(
            validate_replay_associations(&associations, &prefix, &suffix, "request-doc").is_ok()
        );

        let mut changed = associations.clone();
        changed.retained_rows.clear();
        assert!(validate_replay_associations(&changed, &prefix, &suffix, "request-doc").is_err());
        changed = associations.clone();
        changed.prefix_rows[0].source = Some(tag.clone());
        assert!(validate_replay_associations(&changed, &prefix, &suffix, "request-doc").is_err());
        changed = associations.clone();
        changed.retained_rows[0].source = None;
        assert!(validate_replay_associations(&changed, &prefix, &suffix, "request-doc").is_err());
        changed = associations;
        changed.retained_rows[0]
            .source
            .as_mut()
            .unwrap()
            .request_doc_id = "foreign-doc".into();
        assert!(validate_replay_associations(&changed, &prefix, &suffix, "request-doc").is_err());
    }

    #[tokio::test]
    async fn retained_display_backfill_continues_after_a_full_page() {
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let source = escape_graphql_string(&serde_json::to_string(&boundary("doc")).unwrap());
        for index in 0..65 {
            let key = reduction_key("node-a", "session", "doc", index, 1).unwrap();
            let mutation = format!(
                r#"mutation {{ create_ProviderContextReduction(input: {{ reduction_key: "{key}", node_did: "node-a", requester_did: "alice", session_id: "session", request_id: "request", request_doc_id: "doc", request_commit_cid: "request-cid", reduction_index: 1, turn_index: {index}, pair_closed: true, source_boundary_json: "{source}", summary: "summary", messages_compacted: 2, original_tokens: 20, compacted_tokens: 4, created_at: "2026-01-01T00:00:00Z" }}) {{ _docID }} }}"#
            );
            crate::store::config_client::ConfigAccess::write_local(
                &node,
                "test.seed_reduction",
                &mutation,
            )
            .await
            .unwrap();
        }
        let cursor = backfill_display_batch(&node, "node-a", None)
            .await
            .unwrap()
            .expect("full page must continue");
        assert!(backfill_display_batch(&node, "node-a", Some(&cursor))
            .await
            .unwrap()
            .is_none());
        let response = graphql_with_transaction_retry(
            &node,
            "{ ProviderReductionDisplay { reduction_key } }",
            "check display backfill pages",
        )
        .await
        .unwrap();
        assert_eq!(
            response.data.unwrap()["ProviderReductionDisplay"]
                .as_array()
                .unwrap()
                .len(),
            65
        );
        node.shutdown().await;
    }

    #[tokio::test]
    async fn retained_display_backfill_is_scoped_and_restartable() {
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let source = escape_graphql_string(&serde_json::to_string(&boundary("doc")).unwrap());
        for (owner, requester) in [("node-a", "alice"), ("node-b", "bob")] {
            let key = reduction_key(owner, "session", "doc", 1, 1).unwrap();
            let mutation = format!(
                r#"mutation {{ create_ProviderContextReduction(input: {{ reduction_key: "{key}", node_did: "{owner}", requester_did: "{requester}", session_id: "session", request_id: "request", request_doc_id: "doc", request_commit_cid: "request-cid", reduction_index: 1, turn_index: 1, pair_closed: true, source_boundary_json: "{source}", summary: "summary", messages_compacted: 2, original_tokens: 20, compacted_tokens: 4, created_at: "2026-01-01T00:00:00Z" }}) {{ _docID }} }}"#
            );
            crate::store::config_client::ConfigAccess::write_local(
                &node,
                "test.seed_reduction",
                &mutation,
            )
            .await
            .unwrap();
        }
        let ambiguous_key = reduction_key("node-a", "session", "doc", 2, 1).unwrap();
        for index in 0..2 {
            let ambiguous = format!(
                r#"mutation {{ create_ProviderContextReduction(input: {{ reduction_key: "{ambiguous_key}", node_did: "node-a", requester_did: "alice", session_id: "session", request_doc_id: "doc", request_commit_cid: "request-cid", reduction_index: 1, turn_index: 2, pair_closed: true, source_boundary_json: "{source}", summary: "summary {index}", messages_compacted: 2, original_tokens: 20, compacted_tokens: 4, created_at: "2026-01-01T00:00:00Z" }}) {{ _docID }} }}"#
            );
            crate::store::config_client::ConfigAccess::write_local(
                &node,
                "test.seed_ambiguous_reduction",
                &ambiguous,
            )
            .await
            .unwrap();
        }
        let missing_boundary_key = reduction_key("node-a", "session", "doc", 3, 1).unwrap();
        let missing_boundary = format!(
            r#"mutation {{ create_ProviderContextReduction(input: {{ reduction_key: "{missing_boundary_key}", node_did: "node-a", requester_did: "alice", session_id: "session", request_doc_id: "doc", request_commit_cid: "request-cid", reduction_index: 1, turn_index: 3, pair_closed: true, summary: "summary", messages_compacted: 2, original_tokens: 20, compacted_tokens: 4, created_at: "2026-01-01T00:00:00Z" }}) {{ _docID }} }}"#
        );
        crate::store::config_client::ConfigAccess::write_local(
            &node,
            "test.seed_reduction_without_boundary",
            &missing_boundary,
        )
        .await
        .unwrap();
        let open_pair_key = reduction_key("node-a", "session", "doc", 4, 1).unwrap();
        let open_pair = format!(
            r#"mutation {{ create_ProviderContextReduction(input: {{ reduction_key: "{open_pair_key}", node_did: "node-a", requester_did: "alice", session_id: "session", request_doc_id: "doc", request_commit_cid: "request-cid", reduction_index: 1, turn_index: 4, pair_closed: false, source_boundary_json: "{source}", summary: "summary", messages_compacted: 2, original_tokens: 20, compacted_tokens: 4, created_at: "2026-01-01T00:00:00Z" }}) {{ _docID }} }}"#
        );
        crate::store::config_client::ConfigAccess::write_local(
            &node,
            "test.seed_open_reduction",
            &open_pair,
        )
        .await
        .unwrap();
        let (first, concurrent) = tokio::join!(
            backfill_display_batch(&node, "node-a", None),
            backfill_display_batch(&node, "node-a", None),
        );
        assert!(first.unwrap().is_none());
        assert!(concurrent.unwrap().is_none());
        assert!(backfill_display_batch(&node, "node-a", None)
            .await
            .unwrap()
            .is_none());
        let response = graphql_with_transaction_retry(
            &node,
            "{ ProviderReductionDisplay { reduction_key node_did requester_did } }",
            "check display backfill",
        )
        .await
        .unwrap();
        let rows = response.data.unwrap()["ProviderReductionDisplay"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["reduction_key"],
            reduction_key("node-a", "session", "doc", 1, 1).unwrap()
        );
        assert_eq!(rows[0]["node_did"], "node-a");
        assert_eq!(rows[0]["requester_did"], "alice");
        node.shutdown().await;
    }

    #[tokio::test]
    async fn durable_chain_is_idempotent_conflict_visible_and_restartable() {
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let prefix = vec![Message::user("old")];
        let suffix = vec![Message::assistant("current")];
        let checkpoint = checkpoint_from_suffix(&suffix, "summary");
        let source_tag = ReplayTag {
            request_doc_id: "request-doc".into(),
            source: gents_protocol::output::OutputSource::ProviderTurn {
                scope: "inference.1".parse().unwrap(),
                turn_index: 0,
                attempt: 0,
            },
        };
        let associations = ReplayAssociations {
            required: vec![source_tag.clone()],
            prefix_rows: vec![unassociated_row(); prefix.len()],
            retained_rows: vec![one_block_row(source_tag.clone())],
        };
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
                node_did: "did:key:agent",
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
                replay_associations: &associations,
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
                node_did: "did:key:agent",
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
                replay_associations: &associations,
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
                node_did: "did:key:agent",
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
                replay_associations: &associations,
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
        let tagged = restored.0.checkpoint_tagged_messages().unwrap();
        assert_eq!(tagged.len(), checkpoint.len());
        assert_eq!(tagged[0].source, None);
        assert_eq!(tagged[1].source, Some(source_tag.clone()));
        assert_eq!(
            tagged
                .into_iter()
                .map(|row| row.message)
                .collect::<Vec<_>>(),
            checkpoint
        );
        assert_eq!(restored.1, vec![first.reduction_key.clone()]);

        let second_checkpoint = checkpoint_from_suffix(&suffix, "summary 2");
        let mut second_source = source.clone();
        second_source.request_commit_cid = "request-cid-after-reclaim".to_string();
        let second = persist(
            &node,
            NewProviderContextReduction {
                node_did: "did:key:agent",
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
                replay_associations: &associations,
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
                node_did: "did:key:agent"
                requester_did: "did:key:user"
                agent_id: "behavior"
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
                node_did: "did:key:agent"
                requester_did: "did:key:user"
                agent_id: "behavior"
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
            node_did: "did:key:agent"
            requester_did: "did:key:user"
            agent_id: "behavior"
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
                node_did: "did:key:agent"
                requester_did: "did:key:user"
                agent_id: "behavior"
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
        let durable_lineage = load_for_request(&node, "request-doc").await.unwrap();
        assert_eq!(durable_lineage.len(), 2);

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
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let call = Message::Assistant {
            request_id: None,
            id: None,
            content: vec![AssistantContent::ToolCall(ToolCall {
                raw_arguments: None,
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
        #[expect(
            clippy::cloned_ref_to_slice_refs,
            reason = "checkpoint_messages moves the message that retained_suffix would borrow"
        )]
        let error = persist(
            &node,
            NewProviderContextReduction {
                node_did: "did:key:agent",
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
                replay_associations: &ReplayAssociations {
                    required: vec![],
                    prefix_rows: vec![unassociated_row()],
                    retained_rows: vec![unassociated_row()],
                },
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
            vec![crate::turn::prompt::LayeredPromptBuilder::system_reminder(
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
        let encoded = encode_checkpoint(&checkpoint, "durable summary").unwrap();
        assert_eq!(
            decode_checkpoint(&encoded, &suffix, "durable summary").unwrap(),
            checkpoint
        );
        let full = serde_json::to_string(&checkpoint).unwrap();
        assert_eq!(
            decode_checkpoint(&full, &suffix, "durable summary").unwrap(),
            checkpoint
        );
    }

    #[test]
    fn checkpoint_storage_omits_the_retained_tool_payload() {
        let suffix = vec![Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                id: "tool-call".into(),
                call_id: Some("provider-call".into()),
                content: vec![ToolResultContent::Text(Text {
                    text: "large-tool-result".repeat(50_000),
                })],
            })],
        }];
        for summary in ["", "durable summary"] {
            let checkpoint = checkpoint_from_suffix(&suffix, summary);
            let encoded = encode_checkpoint(&checkpoint, summary).unwrap();
            assert!(!encoded.contains("large-tool-result"));
            assert!(encoded.len() < 1_000);
            assert_eq!(
                decode_checkpoint(&encoded, &suffix, summary).unwrap(),
                checkpoint
            );
        }
    }

    #[test]
    fn checkpoint_storage_rejects_unknown_versions_and_invalid_splits() {
        let suffix = vec![Message::user("retained")];
        for encoded in [
            r#"{"version":2,"prefix":[]}"#,
            r#"{"version":1,"prefix":[],"extra":true}"#,
            r#"{"version":1,"prefix":[]}"#,
        ] {
            assert!(decode_checkpoint(encoded, &suffix, "summary").is_err());
        }
        assert!(decode_checkpoint(r#"{"version":1,"prefix":[]}"#, &[], "").is_err());
        let mismatched = serde_json::to_string(&vec![Message::user("different")]).unwrap();
        assert!(decode_checkpoint(&mismatched, &suffix, "").is_err());
    }

    #[tokio::test]
    async fn source_boundary_pins_bounded_canonical_high_water() {
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        for sequence in [2, 1] {
            let mutation = format!(
                r#"mutation {{ create_AgentMessage(input: {{
                    message_key: "session-boundary:{sequence}"
                    node_did: "did:test:boundary-owner"
                    requester_did: null
                    session_id: "session-boundary"
                    sequence: {sequence}
                    role: "user"
                }}) {{ _docID }} }}"#
            );
            let response = node.execute(&mutation).await;
            assert!(
                !response.has_errors(),
                "message seed failed: {:?}",
                response.errors
            );
        }
        let other_principal = node
            .execute(
                r#"mutation { create_AgentMessage(input: {
                message_key: "other-boundary:3"
                node_did: "did:test:other-owner"
                requester_did: null
                session_id: "session-boundary"
                sequence: 3
                role: "user"
            }) { _docID } }"#,
            )
            .await;
        assert!(
            !other_principal.has_errors(),
            "{:?}",
            other_principal.errors
        );
        let boundary = capture_source_boundary(
            &node,
            "session-boundary",
            "did:test:boundary-owner",
            None,
            "request-doc",
            "request-cid",
        )
        .await
        .unwrap();
        let high_water = boundary.canonical_through.expect("canonical high-water");
        assert_eq!(high_water.sequence, 2);
        assert!(!high_water.doc_id.is_empty());
        assert!(!high_water.commit_cid.is_empty());
    }

    #[tokio::test]
    async fn shared_reduction_snapshots_reload_exactly_and_reuse_pinned_blocks() {
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let prefix = vec![Message::user("prefix 🦀 ".repeat(2400))];
        let suffix = vec![Message::user("retained suffix ".repeat(2400))];
        let associations = ReplayAssociations {
            required: vec![],
            prefix_rows: vec![unassociated_row()],
            retained_rows: vec![unassociated_row()],
        };
        let sources = ["request-doc", "request-doc-2"].map(|request_doc_id| SourceBoundary {
            boundary_version: SOURCE_BOUNDARY_VERSION,
            request_doc_id: request_doc_id.to_string(),
            request_commit_cid: "claim-cid-7".to_string(),
            canonical_through: None,
        });
        let make_input = |source: usize| NewProviderContextReduction {
            node_did: "did:key:agent",
            requester_did: None,
            session_id: "session",
            request_id: "request",
            request_doc_id: &sources[source].request_doc_id,
            request_commit_cid: "claim-cid-7",
            reduction_index: 1,
            turn_index: 1,
            parent_reduction_key: None,
            producer_call: None,
            source_boundary: &sources[source],
            compacted_prefix: &prefix,
            retained_suffix: &suffix,
            checkpoint_messages: &suffix,
            replay_associations: &associations,
            summary: "",
            original_tokens: 20000,
            compacted_tokens: 10000,
        };
        let block_ids = || async {
            graphql_with_transaction_retry(
                &node,
                "{ RenderedRequestBlock { _docID } }",
                "test reduction blocks",
            )
            .await
            .unwrap()
            .data
            .unwrap()["RenderedRequestBlock"]
                .as_array()
                .unwrap()
                .iter()
                .map(|block| block["_docID"].as_str().unwrap().to_string())
                .collect::<std::collections::BTreeSet<_>>()
        };
        let first = persist(&node, make_input(0)).await.unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<Message>>(&first.compacted_prefix_json).unwrap(),
            prefix
        );
        assert_eq!(first.checkpoint_messages().unwrap(), suffix);
        let blocks = block_ids().await;
        assert!(!blocks.is_empty());
        let raw = graphql_with_transaction_retry(
            &node,
            "{ ProviderContextReduction { compacted_prefix_json retained_suffix_json } }",
            "test reduction storage",
        )
        .await
        .unwrap()
        .data
        .unwrap();
        for field in ["compacted_prefix_json", "retained_suffix_json"] {
            let encoded = raw["ProviderContextReduction"][0][field].as_str().unwrap();
            assert!(encoded.len() < 1024);
            assert!(encoded.contains("\"kind\":\"tree\""));
        }

        let second = persist(&node, make_input(1)).await.unwrap();
        assert_ne!(second.doc_id, first.doc_id);
        assert_eq!(block_ids().await, blocks);
        for request_doc_id in ["request-doc", "request-doc-2"] {
            let loaded = load_for_request(&node, request_doc_id).await.unwrap();
            assert_eq!(loaded[0].compacted_prefix_json, first.compacted_prefix_json);
            assert_eq!(loaded[0].retained_suffix_json, first.retained_suffix_json);
        }

        crate::store::config_client::ConfigAccess::write_local_response(
            &node,
            "test.delete_reduction_block",
            &format!(
                "mutation {{ delete_RenderedRequestBlock(docID: \"{}\") {{ _docID }} }}",
                escape_graphql_string(blocks.first().unwrap())
            ),
        )
        .await
        .unwrap();
        assert!(load_for_request(&node, "request-doc").await.is_err());
    }
}
