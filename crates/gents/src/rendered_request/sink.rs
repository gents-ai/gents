//! The durable writer behind the capture seam.
//!
//! `Proofs/RenderedCapture.lean` specifies this function exactly, and the three
//! outcomes are not negotiable:
//!
//! | store state for `capture_key` | outcome | write |
//! |---|---|---|
//! | unbound | `fresh` | create |
//! | bound to the identical canonical capture fact | `idempotent` | none |
//! | bound to a *different* canonical capture fact | `rejected` | none, and an error |
//!
//! `capture_rejects_rebinding` is why the third row is an error rather than an
//! update: one capture key names one provider request for the life of the
//! store. `capture_failure_blocks_send` is why an error here has to reach the
//! transport — the caller is
//! [`crate::rendered_request::transport::RenderedRequestCapturingHttpClient`],
//! which refuses the HTTP call on any error this returns.
//!
//! ## Storage shape
//!
//! A capture is stored as content-defined byte blocks plus a manifest row that
//! references them (#2333). The blocks and the manifest row commit in one
//! transaction, so a capture is never durable with a manifest naming blocks
//! that are not, and a failed capture leaves no orphan blocks. Both container
//! payloads are blockified: the provider body and the assembly trace each grow
//! with the conversation, so storing either inline would restore the quadratic
//! cost the blocks exist to remove.
//!
//! ## Identity
//!
//! Reads and writes use the node identity installed on `EmbeddedNode`. DefraDB
//! therefore signs the commit at the same boundary used by every other runtime
//! write. The `agent_did` column remains application data; it is not treated as
//! proof of authorship.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use defra_node::EmbeddedNode;
use serde_json::Value;

use super::{
    canonical_json, canonical_json_string, RenderedCompletionRequest,
    RenderedRequestCaptureFactory, RenderedRequestCaptureSink, RenderedRequestContext,
};
use crate::graphql::escape_graphql_string;

/// The DefraDB-backed capture sink.
#[derive(Clone)]
pub struct DefraRenderedRequestSink {
    node: Arc<EmbeddedNode>,
}

impl std::fmt::Debug for DefraRenderedRequestSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefraRenderedRequestSink").finish()
    }
}

impl DefraRenderedRequestSink {
    pub fn new(node: Arc<EmbeddedNode>) -> Self {
        Self { node }
    }

    /// The immutable capture fact already stored under `capture_key`, if any.
    ///
    /// A GraphQL error is an error, never "no rows": treating a failed read as
    /// an unbound key would turn a transient DB fault into a duplicate-key
    /// create and, worse, into a silent rebinding attempt.
    async fn stored_fact(&self, capture_key: &str) -> Result<Option<Value>> {
        let query = format!(
            r#"query {{
                {collection}(filter: {{ capture_key: {{ _eq: "{capture_key}" }} }}, limit: 2) {{
                    request_doc_id
                    request_commit_cid
                    request_id
                    session_id
                    agent_did
                    requester_did
                    behavior_id
                    capture_scope
                    turn_index
                    attempt
                    capture_version
                    model_name
                    source
                    request_json
                    provenance_json
                }}
            }}"#,
            collection = RENDERED_REQUEST_COLLECTION,
            capture_key = escape_graphql_string(capture_key),
        );
        let response = crate::graphql::graphql_with_transaction_retry(
            &self.node,
            &query,
            "rendered_request::lookup",
        )
        .await?;
        let data = response
            .data
            .ok_or_else(|| anyhow!("reading RenderedRequest by capture key returned no data"))?;
        let rows = data
            .get(RENDERED_REQUEST_COLLECTION)
            .and_then(Value::as_array)
            .ok_or_else(|| {
                anyhow!("reading RenderedRequest by capture key returned an unexpected shape")
            })?;
        match rows.len() {
            0 => Ok(None),
            1 => Ok(Some(rows[0].clone())),
            // The unique index makes this unreachable; if it ever happens the
            // fact record is already ambiguous and must not be extended.
            count => Err(anyhow!(
                "capture key {capture_key} matched {count} RenderedRequest rows; the unique index is not enforcing"
            )),
        }
    }

    /// Create the capture manifest row inside `txn`, after the blocks it names.
    ///
    /// A duplicate-key error is an expected input to reconciliation, so the
    /// create itself does not warn; a genuine failure is logged by the
    /// transport after the re-read outside the transaction cannot establish
    /// idempotency.
    async fn create_in_txn(
        txn: &crate::config_client::ConfigApplyTxn<'_>,
        rendered: &RenderedCompletionRequest,
        request_json: &str,
        provenance_json: &str,
    ) -> Result<()> {
        if !rendered.request_doc_id.is_empty() && rendered.request_commit_cid.is_empty() {
            anyhow::bail!(
                "AgentRequest {} capture has no claimed DefraDB commit CID",
                rendered.request_doc_id
            );
        }
        let source = serde_json::to_value(rendered.source)
            .ok()
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| "unknown".to_string());
        let mutation = format!(
            r#"mutation {{
                create_{collection}(input: {{
                    capture_key: "{capture_key}",
                    request_doc_id: "{request_doc_id}",
                    request_commit_cid: "{request_commit_cid}",
                    request_id: "{request_id}",
                    session_id: "{session_id}",
                    agent_did: "{agent_did}",
                    requester_did: "{requester_did}",
                    behavior_id: "{behavior_id}",
                    capture_scope: "{capture_scope}",
                    turn_index: {turn_index},
                    attempt: {attempt},
                    capture_version: {capture_version},
                    model_name: "{model_name}",
                    source: "{source}",
                    request_json: "{request_json}",
                    provenance_json: "{provenance_json}",
                    created_at: "{created_at}"
                }}) {{ _docID }}
            }}"#,
            collection = RENDERED_REQUEST_COLLECTION,
            capture_key = escape_graphql_string(&rendered.capture_key),
            request_doc_id = escape_graphql_string(&rendered.request_doc_id),
            request_commit_cid = escape_graphql_string(&rendered.request_commit_cid),
            request_id = escape_graphql_string(&rendered.request_id),
            session_id = escape_graphql_string(&rendered.session_id),
            agent_did = escape_graphql_string(&rendered.agent_did),
            requester_did = escape_graphql_string(&rendered.requester_did),
            behavior_id = escape_graphql_string(&rendered.behavior_id),
            capture_scope = escape_graphql_string(&rendered.capture_scope),
            turn_index = rendered.turn_index,
            attempt = rendered.attempt,
            capture_version = rendered.capture_version,
            model_name = escape_graphql_string(&rendered.model_name),
            source = escape_graphql_string(&source),
            request_json = escape_graphql_string(request_json),
            provenance_json = escape_graphql_string(provenance_json),
            created_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339()),
        );

        let response = txn.execute(&mutation).await?;
        // A mutation that returns no document wrote nothing, and "no errors" is
        // not the same as "durable". The field lookup is explicit rather than
        // handing the whole `data` object to `response_has_documents`, which
        // would answer for the envelope instead of for the mutation's result.
        // That result field is taken as the envelope's single entry rather than
        // by name: DefraDB answers a `create_RenderedRequest` mutation under the
        // key `add_RenderedRequest`, and hard-coding either spelling would turn
        // a rename into a silently unverified write.
        if !response
            .get("data")
            .and_then(single_mutation_result)
            .is_some_and(crate::graphql::response_has_documents)
        {
            return Err(anyhow!(
                "creating RenderedRequest returned no document; the capture is not durable"
            ));
        }
        Ok(())
    }

    async fn persist_inference_call_context_accounting(
        &self,
        rendered: &RenderedCompletionRequest,
    ) -> Result<()> {
        let Some(accounting) = rendered.assembly_trace.context_accounting.as_ref() else {
            return Ok(());
        };
        let Some(call_id) = rendered
            .provenance_json
            .get("admission")
            .and_then(|value| value.get("call_id"))
            .and_then(Value::as_str)
        else {
            // One-shot calls have no InferenceCall join. Their accounting is
            // still durable in RenderedRequest.provenance_json.
            return Ok(());
        };
        let accounting_json = canonical_json_string(
            &serde_json::to_value(accounting).context("encoding context accounting")?,
        )?;
        let call_id = escape_graphql_string(call_id);
        let query = format!(
            r#"query {{
                InferenceCall(filter: {{ call_id: {{ _eq: "{call_id}" }} }}, limit: 2) {{
                    context_accounting_json
                }}
            }}"#,
        );
        let response = crate::graphql::graphql_with_transaction_retry(
            &self.node,
            &query,
            "rendered_request::lookup_inference_context_accounting",
        )
        .await?;
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("InferenceCall"))
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("reading joined InferenceCall returned an unexpected shape"))?;
        let [row] = rows.as_slice() else {
            anyhow::bail!(
                "rendered request admission call {call_id} matched {} InferenceCall rows",
                rows.len()
            );
        };
        if let Some(stored) = row
            .get("context_accounting_json")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            let stored: Value = serde_json::from_str(stored)
                .context("decoding stored InferenceCall context_accounting_json")?;
            let incoming: Value = serde_json::from_str(&accounting_json)
                .context("decoding incoming InferenceCall context_accounting_json")?;
            if canonical_json(&stored) == canonical_json(&incoming) {
                return Ok(());
            }
            anyhow::bail!("InferenceCall {call_id} already carries different context accounting");
        }

        let mutation = format!(
            r#"mutation {{
                update_InferenceCall(
                    filter: {{ call_id: {{ _eq: "{call_id}" }} }},
                    input: {{
                        context_accounting_json: "{accounting_json}"
                    }}
                ) {{ _docID }}
            }}"#,
            accounting_json = escape_graphql_string(&accounting_json),
        );
        let response = crate::config_client::ConfigAccess::write_local_response(
            &self.node,
            "rendered_request.persist_context_accounting",
            &mutation,
        )
        .await?;
        if !response
            .data
            .as_ref()
            .and_then(single_mutation_result)
            .is_some_and(crate::graphql::response_has_documents)
        {
            anyhow::bail!("updating InferenceCall context accounting returned no document");
        }
        Ok(())
    }

    /// Persist one capture. See the outcome table at the top of this module.
    /// Writes exactly the given stored bytes, bypassing chunking and block
    /// writes, so tests can create captures the reader must reject.
    #[cfg(test)]
    pub(crate) async fn create_stored_for_test(
        &self,
        rendered: &RenderedCompletionRequest,
        request_json: &str,
    ) -> Result<()> {
        let provenance_json = canonical_json_string(&rendered.provenance_json)?;
        crate::config_client::ConfigAccess::transact_local(
            self.node.as_ref(),
            None,
            "rendered_request.create_stored_for_test",
            |txn| {
                let rendered = rendered.clone();
                let request_json = request_json.to_owned();
                let provenance_json = provenance_json.clone();
                Box::pin(async move {
                    Self::create_in_txn(txn, &rendered, &request_json, &provenance_json).await
                })
            },
        )
        .await
    }

    pub async fn capture(&self, rendered: RenderedCompletionRequest) -> Result<()> {
        anyhow::ensure!(
            rendered.capture_version == gents_protocol::rendered_request::CAPTURE_VERSION,
            "cannot write rendered-request capture version {}; writer supports version {}",
            rendered.capture_version,
            gents_protocol::rendered_request::CAPTURE_VERSION
        );
        // Canonicalize once. The stored bytes and the complete-fact comparison
        // have to use the same representation or "identical" means nothing.
        let request_canonical = canonical_json_string(&rendered.request_json)
            .context("encoding rendered-request request_json")?;
        let provenance_payload_canonical = canonical_json_string(&rendered.provenance_payload_json)
            .context("encoding rendered-request provenance payload")?;
        let provenance_json = canonical_json_string(&rendered.provenance_json)
            .context("encoding rendered-request provenance_json")?;
        let blocks = pending_capture_blocks(&request_canonical, &provenance_payload_canonical);

        // Blocks and the manifest row commit together. The existence read runs
        // inside the same transaction as the creates it gates: a snapshot that
        // disagrees with the writes would turn a concurrent same-content writer
        // into a failed capture instead of an idempotent reuse, and the
        // transaction owner retries this whole closure on conflict, where a
        // fresh snapshot re-decides what is missing. A block create that
        // nevertheless loses the unique content key reconciles against the
        // winning row inside the attempt, since the retry owner classifies
        // neither that error nor the manifest's duplicate key as a conflict.
        let capture_container = crate::config_client::ConfigAccess::transact_local(
            self.node.as_ref(),
            None,
            "rendered_request.capture",
            |txn| {
                let blocks = blocks.clone();
                let request_canonical = request_canonical.clone();
                let provenance_payload_canonical = provenance_payload_canonical.clone();
                let provenance_json = provenance_json.clone();
                let rendered = rendered.clone();
                Box::pin(async move {
                    let mut stored = StoredCaptureBlocks::default();
                    let request_entries =
                        write_capture_blocks(txn, &blocks, &request_canonical, &mut stored).await?;
                    verify_created_block_readback(txn, &request_entries, &stored).await?;
                    let request_encoding = super::encoding::encode_manifest(&request_entries)?;
                    let provenance_entries = write_capture_blocks(
                        txn,
                        &blocks,
                        &provenance_payload_canonical,
                        &mut stored,
                    )
                    .await?;
                    let provenance_encoding =
                        super::encoding::encode_manifest(&provenance_entries)?;
                    let container =
                        super::encoding::encode_container(&request_encoding, &provenance_encoding)?;
                    Self::create_in_txn(txn, &rendered, &container, &provenance_json).await?;
                    Ok(container)
                })
            },
        )
        .await;

        let capture_result = match capture_container {
            Ok(_) => {
                tracing::debug!(
                    capture_key = %rendered.capture_key,
                    request_id = %rendered.request_id,
                    capture_scope = %rendered.capture_scope,
                    turn_index = rendered.turn_index,
                    attempt = rendered.attempt,
                    outcome = "fresh",
                    "persisted rendered provider request"
                );
                Ok(())
            }
            // A concurrent writer may have won the unique index between the
            // lookup and the create, taking the block writes down with the
            // transaction. Re-read: an identical value is still an idempotent
            // success, a different one is still an integrity violation, and
            // anything else keeps the original error.
            Err(create_error) => match self.stored_fact(&rendered.capture_key).await {
                Ok(Some(stored)) => {
                    self.reconcile_existing(&rendered, stored, "create_conflict")
                        .await
                }
                _ => Err(create_error),
            },
        };
        capture_result?;
        self.persist_inference_call_context_accounting(&rendered)
            .await
    }

    async fn reconcile_existing(
        &self,
        rendered: &RenderedCompletionRequest,
        stored: Value,
        via: &str,
    ) -> Result<()> {
        let incoming = canonical_capture_fact(rendered)?;
        let stored = self.canonical_stored_fact(stored).await?;
        if stored == incoming {
            tracing::debug!(
                capture_key = %rendered.capture_key,
                request_id = %rendered.request_id,
                capture_scope = %rendered.capture_scope,
                turn_index = rendered.turn_index,
                attempt = rendered.attempt,
                outcome = "idempotent",
                via,
                "rendered provider request was already durable"
            );
            return Ok(());
        }

        tracing::error!(
            capture_key = %rendered.capture_key,
            request_id = %rendered.request_id,
            session_id = %rendered.session_id,
            capture_scope = %rendered.capture_scope,
            turn_index = rendered.turn_index,
            attempt = rendered.attempt,
            outcome = "rejected",
            via,
            stored_bytes = canonical_json_string(&stored).map(|value| value.len()).unwrap_or_default(),
            incoming_bytes = canonical_json_string(&incoming).map(|value| value.len()).unwrap_or_default(),
            "rendered-request capture key already names a different immutable fact"
        );
        Err(anyhow!(
            "rendered-request integrity violation: capture key {} already names a different \
             canonical capture fact; a capture key is never rebound",
            rendered.capture_key,
        ))
    }

    async fn canonical_stored_fact(&self, mut stored: Value) -> Result<Value> {
        let version = stored
            .get("capture_version")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .context("stored RenderedRequest lacks a numeric capture_version")?;
        let provenance_raw = stored
            .get("provenance_json")
            .and_then(Value::as_str)
            .context("stored RenderedRequest provenance_json was not a string")?
            .to_owned();
        let provenance_value: Value = serde_json::from_str(&provenance_raw)?;
        let encoded = stored
            .get("request_json")
            .and_then(Value::as_str)
            .context("stored RenderedRequest request_json was not a string")?
            .to_owned();
        let access = crate::config_client::ConfigAccess::Local(Arc::clone(&self.node));
        let decoded_pair = if version == 1 {
            None
        } else {
            Some(super::decode_capture_pair(&access, version, &encoded).await?)
        };
        stored["request_json"] = canonical_json(&match decoded_pair.as_ref() {
            Some((request, _)) => request.clone(),
            None => {
                super::decode_capture_json(
                    &access,
                    version,
                    &encoded,
                    super::CapturePayloadKind::RequestBody,
                )
                .await?
            }
        });
        stored["provenance_payload_json"] = if version == 1 {
            canonical_json(
                provenance_value
                    .get("assembly_trace")
                    .context("legacy provenance lacks assembly_trace")?,
            )
        } else {
            canonical_json(&decoded_pair.context("missing decoded capture pair")?.1)
        };
        let parsed = gents_protocol::rendered_request::ProvenanceManifest::parse(&provenance_raw)
            .map_err(|error| anyhow!("decoding stored provenance manifest: {error}"))?;
        stored["provenance_json"] = match parsed {
            gents_protocol::rendered_request::ParsedProvenance::Manifest(manifest) => {
                canonical_json(&serde_json::to_value(manifest)?)
            }
            gents_protocol::rendered_request::ParsedProvenance::Unsupported {
                manifest_version,
            } => anyhow::bail!("unsupported provenance manifest version {manifest_version}"),
        };
        stored
            .as_object_mut()
            .context("stored RenderedRequest fact was not an object")?
            .remove("capture_version");
        Ok(canonical_json(&stored))
    }
}

/// Canonical equality surface for idempotency. `created_at` is intentionally
/// excluded because it records when the winning writer created the row. The
/// request commit CID is included: a retry may not rebind the same provider
/// attempt to a different source version.
fn canonical_capture_fact(rendered: &RenderedCompletionRequest) -> Result<Value> {
    let source =
        serde_json::to_value(rendered.source).context("encoding rendered-request source")?;
    Ok(canonical_json(&serde_json::json!({
        "request_doc_id": rendered.request_doc_id,
        "request_commit_cid": rendered.request_commit_cid,
        "request_id": rendered.request_id,
        "session_id": rendered.session_id,
        "agent_did": rendered.agent_did,
        "requester_did": rendered.requester_did,
        "behavior_id": rendered.behavior_id,
        "capture_scope": rendered.capture_scope,
        "turn_index": rendered.turn_index,
        "attempt": rendered.attempt,
        "model_name": rendered.model_name,
        "source": source,
        "request_json": canonical_json(&rendered.request_json),
        "provenance_json": canonical_json(&rendered.provenance_json),
        "provenance_payload_json": canonical_json(&rendered.provenance_payload_json),
    })))
}

const RENDERED_REQUEST_COLLECTION: &str = gents_protocol::schemas::RENDERED_REQUEST_NAME;

const RENDERED_REQUEST_BLOCK_COLLECTION: &str =
    gents_protocol::schemas::RENDERED_REQUEST_BLOCK_NAME;

/// Split one capture payload into its unique content-defined blocks, in
/// first-seen order. A body may contain byte-identical chunks; the content key
/// is the collection's unique identity, so each is stored once and referenced
/// as many times as it occurs.
fn pending_capture_blocks(
    request_canonical: &str,
    provenance_canonical: &str,
) -> Vec<PendingBlock> {
    let mut blocks = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for chunk in super::encoding::chunk_capture_body(request_canonical)
        .iter()
        .chain(super::encoding::chunk_capture_body(provenance_canonical).iter())
    {
        if seen.insert(chunk.content_key.clone()) {
            blocks.push(PendingBlock {
                content_key: chunk.content_key.clone(),
                bytes: chunk.bytes.clone(),
            });
        }
    }
    blocks
}

/// One unique block a capture needs stored: the content key of its bytes and
/// the bytes themselves.
#[derive(Clone)]
struct PendingBlock {
    content_key: String,
    bytes: Vec<u8>,
}

/// Create mutation for one capture block. The payload travels as a typed
/// GraphQL variable, never as rendered GraphQL text: block bytes are canonical
/// JSON text, and the variable encoder preserves them exactly where rendered
/// text would have to escape its way back to the same bytes.
const CREATE_RENDERED_REQUEST_BLOCK_MUTATION: &str = "mutation($input: \
RenderedRequestBlockMutationInputArg!) { create_RenderedRequestBlock(input: $input) { _docID } }";

/// Read the block rows already stored under `keys`, in one batched query.
///
/// Returns `content_key -> (doc_id, payload)`. A row whose payload does not
/// hash to its own content key is an integrity error, not a reusable block:
/// pinning it would name bytes the key does not describe.
async fn stored_blocks_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    keys: &[String],
) -> Result<std::collections::BTreeMap<String, (String, String)>> {
    let mut stored = std::collections::BTreeMap::new();
    if keys.is_empty() {
        return Ok(stored);
    }
    let rendered_keys = keys
        .iter()
        .map(|key| format!("\"{}\"", escape_graphql_string(key)))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        r#"{{ {collection}(filter: {{ content_key: {{ _in: [{rendered_keys}] }} }}) {{
            _docID content_key payload
        }} }}"#,
        collection = RENDERED_REQUEST_BLOCK_COLLECTION,
    );
    let response = txn.execute(&query).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get(RENDERED_REQUEST_BLOCK_COLLECTION))
        .and_then(Value::as_array)
        .context("reading stored capture blocks returned an unexpected shape")?;
    for row in rows {
        let content_key = row
            .get("content_key")
            .and_then(Value::as_str)
            .context("stored capture block row lacks content_key")?
            .to_owned();
        let doc_id = row
            .get("_docID")
            .and_then(Value::as_str)
            .context("stored capture block row lacks _docID")?
            .to_owned();
        let payload = row
            .get("payload")
            .and_then(Value::as_str)
            .context("stored capture block row lacks payload")?
            .to_owned();
        anyhow::ensure!(
            super::encoding::block_content_key(payload.as_bytes()) == content_key,
            "stored capture block {doc_id} payload does not match its content key"
        );
        if stored
            .insert(content_key.clone(), (doc_id, payload))
            .is_some()
        {
            anyhow::bail!(
                "capture block content key {content_key} matched more than one row; the unique \
                 index is not enforcing"
            );
        }
    }
    Ok(stored)
}

/// Block documents this capture has already resolved or written, shared by both
/// payload writes so the batched existence read runs once per capture.
#[derive(Default)]
struct StoredCaptureBlocks {
    doc_ids: std::collections::BTreeMap<String, String>,
    /// Blocks this attempt created, keyed by content key. Reused rows had
    /// their stored payload byte-compared on read; created rows did not, which
    /// is what the post-create read-back exists to close.
    created: std::collections::BTreeMap<String, Vec<u8>>,
}

/// A unique-index violation surfaced through a transaction statement: DefraDB's
/// shared `UniqueConstraintViolation` message, the same error class
/// `gents-migration` tolerates at boot. The transaction owner classifies only
/// snapshot conflicts as replayable, so this class neither retries nor
/// reconciles on its own; the caller must.
fn is_unique_index_violation(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.to_string().contains("violates unique index"))
}

/// Create one capture block document and return its document id.
///
/// The content key is unique on the collection, so the create can lose to a
/// concurrent writer that stored the same block first. The loser reconciles
/// inside this same attempt instead of failing the capture: the winning row is
/// re-read by content key, byte-compared, and its document id returned so the
/// manifest names the row that is actually durable — the entry loop then pins
/// that row's current field commit. Only a winner holding different bytes for
/// the same content key is an integrity error.
async fn create_block_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    block: &PendingBlock,
) -> Result<String> {
    let payload = String::from_utf8(block.bytes.clone()).context(
        "capture block bytes must be valid UTF-8; chunk boundaries land on character boundaries",
    )?;
    let input = serde_json::json!({
        "content_key": block.content_key,
        "payload": payload,
        "byte_len": block.bytes.len(),
        "created_at": chrono::Utc::now().to_rfc3339(),
    });
    let variables = serde_json::json!({ "input": input });
    let response = match txn
        .execute_with_variables(CREATE_RENDERED_REQUEST_BLOCK_MUTATION, &variables)
        .await
    {
        Ok(response) => response,
        Err(error) if is_unique_index_violation(&error) => {
            return reconcile_created_block(txn, block).await;
        }
        Err(error) => return Err(error),
    };
    // The result field is taken as the response's single mutation entry
    // rather than by name, for the same reason the capture-row verification
    // above avoids both spellings: DefraDB answers a `create_X` mutation
    // under an `add_X` key, and hard-coding either would turn a rename into
    // a silently unverified write.
    response
        .get("data")
        .and_then(single_mutation_result)
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(Value::as_str)
        .with_context(|| {
            format!(
                "creating capture block returned no document: {}",
                block.content_key
            )
        })
        .map(ToOwned::to_owned)
}

/// Resolve the winner of a lost block create: the row already holding the
/// content key, which must carry the same bytes or the store is lying about
/// content it addresses.
async fn reconcile_created_block(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    block: &PendingBlock,
) -> Result<String> {
    let (doc_id, payload) = stored_blocks_in_txn(txn, &[block.content_key.clone()])
        .await?
        .get(&block.content_key)
        .with_context(|| {
            format!(
                "capture block create lost the unique content key {} but its winner is unreadable",
                block.content_key
            )
        })?
        .clone();
    anyhow::ensure!(
        payload.as_bytes() == block.bytes.as_slice(),
        "concurrent capture block {doc_id} holds different bytes under the same content key"
    );
    Ok(doc_id)
}

/// Write the blocks one capture payload needs and return its manifest entries,
/// reusing rows another capture already stored.
///
/// The function gates its own commit the way `encode_full` does: it reassembles
/// the stored bytes back over the canonical body and refuses to return entries
/// that do not reproduce it exactly, so no manifest is ever written for a body
/// the store cannot give back. Reused rows are byte-compared on read, created
/// rows are read back once through [`verify_created_block_readback`].
///
/// Every entry pins the *current* field commit of its block document, read in
/// this same transaction for created and reused rows alike, so the manifest
/// never trusts a prior manifest's witness. The byte length is pinned beside
/// it so a later short read fails closed instead of reassembling a truncated
/// body.
async fn write_capture_blocks(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    blocks: &[PendingBlock],
    canonical: &str,
    stored: &mut StoredCaptureBlocks,
) -> Result<Vec<super::encoding::ManifestEntry>> {
    let unresolved: Vec<&PendingBlock> = blocks
        .iter()
        .filter(|block| !stored.doc_ids.contains_key(&block.content_key))
        .collect();
    if !unresolved.is_empty() {
        let keys = unresolved
            .iter()
            .map(|block| block.content_key.clone())
            .collect::<Vec<_>>();
        let present = stored_blocks_in_txn(txn, &keys).await?;
        for block in &unresolved {
            if let Some((doc_id, payload)) = present.get(&block.content_key) {
                anyhow::ensure!(
                    payload.as_bytes() == block.bytes.as_slice(),
                    "stored capture block {doc_id} holds different bytes under the same content key"
                );
                stored
                    .doc_ids
                    .insert(block.content_key.clone(), doc_id.clone());
            }
        }
    }
    for block in blocks {
        if stored.doc_ids.contains_key(&block.content_key) {
            continue;
        }
        let doc_id = create_block_in_txn(txn, block).await?;
        stored
            .created
            .insert(block.content_key.clone(), block.bytes.clone());
        stored.doc_ids.insert(block.content_key.clone(), doc_id);
    }

    let mut entries = Vec::new();
    let mut assembled = Vec::new();
    for chunk in super::encoding::chunk_capture_body(canonical) {
        let doc_id = stored
            .doc_ids
            .get(&chunk.content_key)
            .context("capture block document id was not resolved")?;
        let commit = super::commits::field_commit_in_txn(txn, doc_id, "payload")
            .await?
            .with_context(|| format!("capture block {doc_id} lacks a payload field commit"))?;
        assembled.extend_from_slice(&chunk.bytes);
        entries.push(super::encoding::ManifestEntry {
            doc_id: doc_id.clone(),
            content_key: chunk.content_key.clone(),
            field_commit_cid: commit.cid,
            byte_len: u64::try_from(chunk.bytes.len()).context("capture block length overflow")?,
        });
    }
    anyhow::ensure!(
        assembled == canonical.as_bytes(),
        "chunked capture blocks did not reassemble into the canonical body"
    );
    Ok(entries)
}

/// Read one block this capture created back through the same transaction and
/// byte-compare it with the bytes the writer holds.
///
/// The reassemble gate inside `write_capture_blocks` compares in-memory chunk
/// bytes, and reused rows were byte-compared against a store read; created
/// rows were the one path store-side mangling could survive unseen until a
/// reader failed closed. One read per capture, on the first block of the
/// request-body manifest the capture created; a manifest built entirely from
/// already-stored rows read everything it names.
async fn verify_created_block_readback(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    entries: &[super::encoding::ManifestEntry],
    stored: &StoredCaptureBlocks,
) -> Result<()> {
    let Some(entry) = entries
        .iter()
        .find(|entry| stored.created.contains_key(&entry.content_key))
    else {
        return Ok(());
    };
    let Some(bytes) = stored.created.get(&entry.content_key) else {
        return Ok(());
    };
    let (doc_id, payload) = stored_blocks_in_txn(txn, &[entry.content_key.clone()])
        .await?
        .get(&entry.content_key)
        .with_context(|| {
            format!(
                "created capture block {} was unreadable in its own transaction",
                entry.content_key
            )
        })?
        .clone();
    anyhow::ensure!(
        payload.as_bytes() == bytes.as_slice(),
        "created capture block {doc_id} did not read back the bytes it was written with"
    );
    Ok(())
}

/// The single result field of a single-operation mutation envelope.
///
/// `None` when the envelope is not a one-entry object, which is the honest
/// answer for a response this sink does not recognise — treating it as "wrote
/// something" is the failure mode the caller is checking for.
fn single_mutation_result(data: &Value) -> Option<&Value> {
    let object = data.as_object()?;
    let mut entries = object.values();
    let first = entries.next()?;
    entries.next().is_none().then_some(first)
}

/// The production capture factory: one sink per request context, all writing
/// through the same identity-configured node.
pub(crate) fn defra_rendered_request_capture_factory(
    node: Arc<EmbeddedNode>,
) -> RenderedRequestCaptureFactory {
    Arc::new(move |_context: RenderedRequestContext| {
        let sink = DefraRenderedRequestSink::new(Arc::clone(&node));
        let sink: RenderedRequestCaptureSink = Arc::new(move |rendered| {
            let sink = sink.clone();
            Box::pin(async move { sink.capture(rendered).await })
        });
        sink
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// Deterministic pseudo-text for capture bodies under test: a fixed-seed
    /// xorshift64 stream mapped onto `a..z`. Long enough bodies to span several
    /// content-defined blocks is what makes the block-count assertions below
    /// meaningful, so every body here is built from it.
    fn capture_text(seed: u64, len: usize) -> String {
        let mut state = seed;
        let mut out = String::with_capacity(len);
        while out.len() < len {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            out.push(char::from(b'a' + (state % 26) as u8));
        }
        out
    }

    fn rendered_turn(turn_index: usize, request_json: Value) -> RenderedCompletionRequest {
        let capture_scope = "inference.1".to_string();
        let assembly_trace = super::super::AssemblyTrace::from_effective_messages(
            super::super::AssemblyBuildPath::Budgeted,
            Vec::new(),
        );
        RenderedCompletionRequest {
            capture_key: super::super::capture_key(
                "did:key:test",
                "session",
                "",
                &capture_scope,
                turn_index,
                0,
            )
            .unwrap(),
            capture_version: gents_protocol::rendered_request::CAPTURE_VERSION,
            request_doc_id: String::new(),
            request_commit_cid: String::new(),
            request_id: "request".into(),
            capture_scope: capture_scope.clone(),
            turn_index,
            attempt: 0,
            agent_did: "did:key:test".into(),
            requester_did: String::new(),
            behavior_id: "behavior".into(),
            session_id: "session".into(),
            model_name: "model".into(),
            source: super::super::RenderedRequestSource::OpenAiChatCompletions,
            request_json,
            messages_json: json!([]),
            tools_json: json!([]),
            tool_choice_json: Value::Null,
            sampling_json: Value::Null,
            provenance_json: serde_json::to_value(super::super::ProvenanceManifest::captured_only(
                capture_scope,
                None,
                None,
                assembly_trace.clone(),
            ))
            .unwrap(),
            provenance_payload_json: serde_json::to_value(&assembly_trace).unwrap(),
            assembly_trace,
        }
    }

    async fn block_rows(node: &EmbeddedNode) -> Vec<Value> {
        let response = node
            .execute(r#"{ RenderedRequestBlock { _docID content_key byte_len } }"#)
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        response.data.unwrap()["RenderedRequestBlock"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    async fn stored_capture(node: &EmbeddedNode, capture_key: &str) -> Value {
        let response = node
            .execute(&format!(
                r#"{{ RenderedRequest(filter: {{ capture_key: {{ _eq: "{}" }} }}, limit: 2) {{ capture_version request_json }} }}"#,
                escape_graphql_string(capture_key)
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let rows = response.data.unwrap()["RenderedRequest"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(rows.len(), 1, "exactly one capture row per key");
        rows.into_iter().next().unwrap()
    }

    #[tokio::test]
    async fn capture_writes_blocks_and_a_manifest_that_decode_back() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let sink = DefraRenderedRequestSink::new(Arc::clone(&node));
        let rendered = rendered_turn(
            0,
            json!({"model": "m", "messages": [{"role": "user", "content": capture_text(1, 9 * 1024)}]}),
        );
        sink.capture(rendered.clone()).await.expect("capture");

        let row = stored_capture(node.as_ref(), &rendered.capture_key).await;
        assert_eq!(row["capture_version"].as_u64(), Some(3));
        let stored = row["request_json"].as_str().unwrap();
        let container: Value = serde_json::from_str(stored).unwrap();
        assert_eq!(container["request_body"]["kind"], "manifest");
        assert_eq!(container["provenance_payload"]["kind"], "manifest");
        let blocks = block_rows(node.as_ref()).await;
        assert!(
            blocks.len() > 1,
            "a body spanning several blocks must be stored as blocks"
        );

        let (request, provenance) =
            super::super::decode_capture_pair_embedded(node.as_ref(), 3, stored)
                .await
                .expect("decode v3 capture pair");
        assert_eq!(
            request,
            super::super::canonical_json(&rendered.request_json)
        );
        assert_eq!(
            provenance,
            super::super::canonical_json(&rendered.provenance_payload_json)
        );
        node.shutdown().await;
    }

    /// The regression #2333 exists to fix: two turns sharing a conversation
    /// prefix write only the new blocks and a manifest row, so total capture
    /// storage grows linearly with the conversation instead of quadratically.
    #[tokio::test]
    async fn a_shared_prefix_writes_only_new_blocks_and_a_small_manifest() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let sink = DefraRenderedRequestSink::new(Arc::clone(&node));

        let prefix = capture_text(7, 24 * 1024);
        let first = rendered_turn(
            0,
            json!({"model": "m", "messages": [{"role": "user", "content": prefix.clone()}]}),
        );
        sink.capture(first.clone()).await.expect("first capture");
        let first_blocks = block_rows(node.as_ref()).await;

        // Turn two repeats the whole conversation and appends one message.
        let mut second_messages = first.request_json["messages"].clone();
        second_messages
            .as_array_mut()
            .unwrap()
            .push(json!({"role": "assistant", "content": capture_text(9, 1024)}));
        let second = rendered_turn(1, json!({"model": "m", "messages": second_messages}));
        sink.capture(second.clone()).await.expect("second capture");
        let second_blocks = block_rows(node.as_ref()).await;

        assert!(
            second_blocks.len() > first_blocks.len(),
            "the new message must add blocks"
        );
        let added: std::collections::BTreeSet<String> = second_blocks
            .iter()
            .map(|row| row["content_key"].as_str().unwrap().to_owned())
            .collect::<std::collections::BTreeSet<_>>()
            .difference(
                &first_blocks
                    .iter()
                    .map(|row| row["content_key"].as_str().unwrap().to_owned())
                    .collect(),
            )
            .cloned()
            .collect();
        // Every block of the edited body but the ones covering the edit and the
        // JSON tail is re-referenced, never rewritten.
        assert!(
            (added.len() as f64) < 0.4 * second_blocks.len() as f64,
            "{} new of {} total blocks; the shared prefix must be reused",
            added.len(),
            second_blocks.len()
        );

        let row = stored_capture(node.as_ref(), &second.capture_key).await;
        let stored = row["request_json"].as_str().unwrap();
        assert!(
            // Two manifests (request body + provenance payload) of small
            // references, against the full canonical body.
            stored.len() < 3 * prefix.len() / 4,
            "the capture row must not repeat the conversation: {} bytes for a {} byte body",
            stored.len(),
            prefix.len()
        );
        assert_eq!(
            super::super::decode_capture_json_embedded(
                node.as_ref(),
                3,
                stored,
                super::super::CapturePayloadKind::RequestBody,
            )
            .await
            .unwrap(),
            super::super::canonical_json(&second.request_json)
        );
        node.shutdown().await;
    }

    /// A block another capture already stored keeps that row's document and
    /// field commit: the manifest pins the existing witness, never a fresh one,
    /// and never rewrites the block.
    #[tokio::test]
    async fn a_reused_block_pins_the_existing_row_field_commit() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let sink = DefraRenderedRequestSink::new(Arc::clone(&node));

        let body = capture_text(11, 12 * 1024);
        let first = rendered_turn(
            0,
            json!({"model": "m", "messages": [{"role": "user", "content": body}]}),
        );
        sink.capture(first).await.expect("first capture");
        let first_row =
            stored_capture(node.as_ref(), &rendered_turn(0, json!({})).capture_key).await;
        let first_entries =
            manifest_entries(&first_row["request_json"].as_str().unwrap(), "request_body");
        let first_blocks = block_rows(node.as_ref()).await;

        // Turn two appends, so it shares the leading blocks.
        let mut messages = json!([{"role": "user", "content": body}]);
        messages
            .as_array_mut()
            .unwrap()
            .push(json!({"role": "assistant", "content": capture_text(13, 512)}));
        let second = rendered_turn(1, json!({"model": "m", "messages": messages}));
        sink.capture(second.clone()).await.expect("second capture");

        let second_row = stored_capture(node.as_ref(), &second.capture_key).await;
        let second_entries = manifest_entries(
            &second_row["request_json"].as_str().unwrap(),
            "request_body",
        );
        let by_key = |entries: &[(String, String, String)]| {
            entries
                .iter()
                .map(|(doc_id, key, cid)| (key.clone(), (doc_id.clone(), cid.clone())))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let first_map = by_key(&first_entries);
        let second_map = by_key(&second_entries);
        let shared = second_map
            .keys()
            .filter(|key| first_map.contains_key(*key))
            .count();
        assert!(shared > 0, "the turns must share blocks");
        for (key, (doc_id, cid)) in &second_map {
            if let Some((first_doc_id, first_cid)) = first_map.get(key) {
                assert_eq!(
                    doc_id, first_doc_id,
                    "shared block {key} must reuse the row"
                );
                assert_eq!(
                    cid, first_cid,
                    "shared block {key} must pin the existing witness"
                );
            }
        }
        // Nothing was rewritten: the block rows are exactly the ones the first
        // capture created plus the new content.
        assert!(block_rows(node.as_ref()).await.len() > first_blocks.len());
        node.shutdown().await;
    }

    fn manifest_entries(stored: &str, payload: &str) -> Vec<(String, String, String)> {
        let container: Value = serde_json::from_str(stored).unwrap();
        container[payload]["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                (
                    entry["doc_id"].as_str().unwrap().to_owned(),
                    entry["content_key"].as_str().unwrap().to_owned(),
                    entry["field_commit_cid"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    /// A block row whose payload does not hash to its content key names bytes
    /// the key does not describe; reusing it would store a body nobody sent, so
    /// the capture fails closed instead.
    #[tokio::test]
    async fn a_block_row_whose_payload_mismatches_its_content_key_fails_closed() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let sink = DefraRenderedRequestSink::new(Arc::clone(&node));

        let body = capture_text(17, 6 * 1024);
        let rendered = rendered_turn(
            0,
            json!({"model": "m", "messages": [{"role": "user", "content": body}]}),
        );
        let canonical = canonical_json_string(&rendered.request_json).unwrap();
        let first = super::super::encoding::chunk_capture_body(&canonical)
            .into_iter()
            .next()
            .expect("body chunks");
        let tampered = format!("{}tampered", first.bytes.len());
        let response = node
            .execute(&format!(
                r#"mutation {{ create_RenderedRequestBlock(input: {{ content_key: "{}", payload: "{}", byte_len: {}, created_at: "2026-01-01T00:00:00Z" }}) {{ _docID }} }}"#,
                escape_graphql_string(&first.content_key),
                escape_graphql_string(&tampered),
                tampered.len(),
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);

        let error = sink.capture(rendered).await.unwrap_err();
        assert!(
            error.to_string().contains("does not match its content key"),
            "{error:#}"
        );
        node.shutdown().await;
    }

    #[tokio::test]
    async fn same_capture_key_cannot_rebind_its_transport_route() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let sink = DefraRenderedRequestSink::new(Arc::clone(&node));
        let mut first = rendered_turn(
            0,
            json!({"messages": [{"role": "user", "content": capture_text(3, 3 * 1024)}]}),
        );
        first.provenance_json["provider_family"] = json!("OpenAiCompatible");
        first.provenance_json["provider_endpoint"] = json!("https://example.test");
        first.provenance_json["provider_route_path_sha256"] = json!("path-a");
        sink.capture(first.clone()).await.expect("first capture");

        let mut rebound = first;
        rebound.provenance_json["provider_route_path_sha256"] = json!("path-b");
        let error = sink.capture(rebound).await.unwrap_err();
        assert!(
            error.to_string().contains("integrity violation"),
            "{error:#}"
        );
        node.shutdown().await;
    }

    /// Re-delivering the identical capture is idempotent: the block writes roll
    /// back with the conflicting manifest create, and the stored fact already
    /// names the same canonical value.
    #[tokio::test]
    async fn re_delivering_a_capture_is_idempotent() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let sink = DefraRenderedRequestSink::new(Arc::clone(&node));
        let rendered = rendered_turn(
            0,
            json!({"messages": [{"role": "user", "content": capture_text(5, 5 * 1024)}]}),
        );
        sink.capture(rendered.clone()).await.expect("first capture");
        let blocks_after_first = block_rows(node.as_ref()).await.len();

        sink.capture(rendered.clone())
            .await
            .expect("re-delivery is idempotent");
        assert_eq!(
            block_rows(node.as_ref()).await.len(),
            blocks_after_first,
            "no block may be written twice"
        );
        node.shutdown().await;
    }

    /// The block-create race: a concurrent writer stored the same content key
    /// first, so the create loses the unique index rather than seeing the row
    /// in the existence read. The write closure reconciles against the winning
    /// row in the same attempt — identical bytes complete the work on the
    /// winner's row, different bytes under the same key are an integrity
    /// error — and the capture the reconciled block belongs to still succeeds.
    #[tokio::test]
    async fn a_lost_block_create_reconciles_the_winning_row() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let sink = DefraRenderedRequestSink::new(Arc::clone(&node));

        let request_json = json!({
            "model": "m",
            "messages": [{"role": "user", "content": capture_text(23, 6 * 1024)}]
        });
        let canonical = canonical_json_string(&request_json).unwrap();
        let blocks: Vec<PendingBlock> = super::super::encoding::chunk_capture_body(&canonical)
            .iter()
            .map(|chunk| PendingBlock {
                content_key: chunk.content_key.clone(),
                bytes: chunk.bytes.clone(),
            })
            .collect();
        assert!(blocks.len() > 1, "the body must span several blocks");

        // The winner: identical content under its own created_at, committed
        // before the transaction below, so the create finds its unique index
        // entry rather than a reusable row in the existence read's place.
        let winner = blocks[0].clone();
        let winner_payload = String::from_utf8(winner.bytes.clone()).unwrap();
        let response = node
            .execute(&format!(
                r#"mutation {{ create_RenderedRequestBlock(input: {{ content_key: "{}", payload: "{}", byte_len: {}, created_at: "2026-01-01T00:00:00Z" }}) {{ _docID }} }}"#,
                escape_graphql_string(&winner.content_key),
                escape_graphql_string(&winner_payload),
                winner.bytes.len(),
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let winner_doc_id = response.data.unwrap()["add_RenderedRequestBlock"][0]["_docID"]
            .as_str()
            .unwrap()
            .to_owned();

        let expected_doc = winner_doc_id.clone();
        let reconciled = crate::config_client::ConfigAccess::transact_local(
            node.as_ref(),
            None,
            "test.reconcile_lost_block_create",
            |txn| {
                let winner = winner.clone();
                let expected_doc = expected_doc.clone();
                Box::pin(async move {
                    let doc_id = create_block_in_txn(txn, &winner).await?;
                    anyhow::ensure!(
                        doc_id == expected_doc,
                        "the reconciled reference must name the winner, not a new row"
                    );
                    Ok(doc_id)
                })
            },
        )
        .await
        .expect("a create lost to identical bytes reconciles");
        assert_eq!(reconciled, winner_doc_id);

        // The failed create wrote nothing: the winner is still the only row
        // under the content key.
        let rows = node
            .execute(&format!(
                r#"{{ RenderedRequestBlock(filter: {{ content_key: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                escape_graphql_string(&winner.content_key),
            ))
            .await;
        assert!(!rows.has_errors(), "{:?}", rows.errors);
        let holding = rows.data.unwrap()["RenderedRequestBlock"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(holding.len(), 1, "one row per content key");
        assert_eq!(
            holding[0]["_docID"].as_str(),
            Some(winner_doc_id.as_str()),
            "the winner keeps the content key"
        );

        // A winner holding different bytes under the same content key is an
        // integrity error, not a block to pin.
        let other_canonical = canonical_json_string(&json!({
            "model": "m",
            "messages": [{"role": "user", "content": capture_text(29, 6 * 1024)}]
        }))
        .unwrap();
        let other = super::super::encoding::chunk_capture_body(&other_canonical)
            .into_iter()
            .next()
            .expect("other body chunks");
        let tampered = format!("{}tampered", other.bytes.len());
        let response = node
            .execute(&format!(
                r#"mutation {{ create_RenderedRequestBlock(input: {{ content_key: "{}", payload: "{}", byte_len: {}, created_at: "2026-01-01T00:00:00Z" }}) {{ _docID }} }}"#,
                escape_graphql_string(&other.content_key),
                escape_graphql_string(&tampered),
                tampered.len(),
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let error = crate::config_client::ConfigAccess::transact_local(
            node.as_ref(),
            None,
            "test.reconcile_lost_block_create",
            |txn| {
                let other = PendingBlock {
                    content_key: other.content_key.clone(),
                    bytes: other.bytes.clone(),
                };
                Box::pin(async move { create_block_in_txn(txn, &other).await })
            },
        )
        .await
        .expect_err("a create lost to different bytes is an integrity error");
        assert!(
            error.to_string().contains("does not match its content key"),
            "{error:#}"
        );

        // The reconciled winner is the row the real writer uses: the capture
        // completes over it and decodes back through the reader.
        let rendered = rendered_turn(0, request_json);
        sink.capture(rendered.clone()).await.expect("capture");
        let row = stored_capture(node.as_ref(), &rendered.capture_key).await;
        let stored = row["request_json"].as_str().unwrap();
        assert_eq!(
            super::super::decode_capture_json_embedded(
                node.as_ref(),
                3,
                stored,
                super::super::CapturePayloadKind::RequestBody,
            )
            .await
            .unwrap(),
            super::super::canonical_json(&rendered.request_json)
        );
        node.shutdown().await;
    }

    /// The collection names are interpolated as bare GraphQL identifiers, where
    /// escaping cannot defend. They are compile-time constants from the protocol
    /// catalog, and this is the fence that keeps them valid identifiers if that
    /// catalog ever changes.
    #[test]
    fn the_collection_names_are_valid_graphql_identifiers() {
        crate::graphql::validate_collection_identifier(RENDERED_REQUEST_COLLECTION)
            .expect("RenderedRequest must be a valid GraphQL identifier");
        assert_eq!(RENDERED_REQUEST_COLLECTION, "RenderedRequest");
        crate::graphql::validate_collection_identifier(RENDERED_REQUEST_BLOCK_COLLECTION)
            .expect("RenderedRequestBlock must be a valid GraphQL identifier");
        assert_eq!(RENDERED_REQUEST_BLOCK_COLLECTION, "RenderedRequestBlock");
    }
}
