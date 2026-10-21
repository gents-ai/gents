//! Reads and writes use the node identity installed on
//! `EmbeddedNode`; the `node_did` column is application data, not proof of
//! authorship.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use defra_node::EmbeddedNode;
use serde_json::Value;

use super::{
    canonical_json, canonical_json_string, RenderedCompletionRequest,
    RenderedRequestCaptureFactory, RenderedRequestCaptureSink, RenderedRequestContext,
};
use crate::graphql::escape_graphql_string;

#[derive(Clone)]
pub struct DefraRenderedRequestSink {
    node: Arc<EmbeddedNode>,
    block_pins: Arc<std::sync::Mutex<CapturePinCache>>,
}

type CapturePinCache =
    std::collections::BTreeMap<CapturePinScope, std::collections::BTreeMap<String, BlockPin>>;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CapturePinScope {
    node_did: String,
    requester_did: String,
    session_id: String,
    source: String,
    capture_scope: String,
}

impl CapturePinScope {
    fn for_capture(rendered: &RenderedCompletionRequest) -> Result<Self> {
        Ok(Self {
            node_did: rendered.node_did.clone(),
            requester_did: rendered.requester_did.clone(),
            session_id: rendered.session_id.clone(),
            source: serde_json::to_string(&rendered.source)?,
            capture_scope: rendered.capture_scope.clone(),
        })
    }
}

impl std::fmt::Debug for DefraRenderedRequestSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefraRenderedRequestSink").finish()
    }
}

impl DefraRenderedRequestSink {
    pub fn new(node: Arc<EmbeddedNode>) -> Self {
        Self {
            node,
            block_pins: Default::default(),
        }
    }

    async fn stored_fact(&self, capture_key: &str) -> Result<Option<Value>> {
        let query = format!(
            r#"query {{
                {collection}(filter: {{ capture_key: {{ _eq: "{capture_key}" }} }}, limit: 2) {{
                    request_doc_id
                    request_commit_cid
                    request_id
                    session_id
                    node_did
                    requester_did
                    agent_id
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
            count => Err(anyhow!(
                "capture key {capture_key} matched {count} RenderedRequest rows; the unique index is not enforcing"
            )),
        }
    }

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
                    node_did: "{node_did}",
                    requester_did: "{requester_did}",
                    agent_id: "{agent_id}",
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
            node_did = escape_graphql_string(&rendered.node_did),
            requester_did = escape_graphql_string(&rendered.requester_did),
            agent_id = escape_graphql_string(&rendered.agent_id),
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

    fn inference_call_context_accounting(
        rendered: &RenderedCompletionRequest,
    ) -> Result<Option<(&str, String)>> {
        let Some(accounting) = rendered.assembly_trace.context_accounting.as_ref() else {
            return Ok(None);
        };
        let Some(call_id) = rendered
            .provenance_json
            .get("admission")
            .and_then(|value| value.get("call_id"))
            .and_then(Value::as_str)
        else {
            return Ok(None);
        };
        let accounting_json = canonical_json_string(
            &serde_json::to_value(accounting).context("encoding context accounting")?,
        )?;
        Ok(Some((call_id, accounting_json)))
    }

    async fn persist_inference_call_context_accounting_in_txn(
        txn: &crate::config_client::ConfigApplyTxn<'_>,
        call_id: &str,
        accounting_json: &str,
    ) -> Result<()> {
        let call_id = escape_graphql_string(call_id);
        let query = format!(
            r#"query {{
                InferenceCall(filter: {{ call_id: {{ _eq: "{call_id}" }} }}, limit: 2) {{
                    _docID
                    context_accounting_json
                }}
            }}"#,
        );
        let response = txn.execute(&query).await?;
        let rows = response
            .get("data")
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

        let doc_id = row
            .get("_docID")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .context("joined InferenceCall lacks physical identity")?;
        let doc_id = escape_graphql_string(doc_id);
        let mutation = format!(
            r#"mutation {{
                update_InferenceCall(
                    docID: "{doc_id}",
                    filter: {{ call_id: {{ _eq: "{call_id}" }} }},
                    input: {{
                        context_accounting_json: "{accounting_json}"
                    }}
                ) {{ _docID }}
            }}"#,
            accounting_json = escape_graphql_string(&accounting_json),
        );
        let response = txn.execute(&mutation).await?;
        if !response
            .get("data")
            .and_then(single_mutation_result)
            .is_some_and(crate::graphql::response_has_documents)
        {
            anyhow::bail!("updating InferenceCall context accounting returned no document");
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) async fn create_stored_for_test(
        &self,
        rendered: &RenderedCompletionRequest,
        request_json: &str,
    ) -> Result<()> {
        let provenance_json = canonical_json_string(&rendered.provenance_json)?;
        crate::config_client::ConfigAccess::transact_local_write(
            &self.node,
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
        let context_accounting = Self::inference_call_context_accounting(&rendered)?;
        let request_layout = super::encoding::capture_block_layout(&rendered.request_json)
            .context("encoding rendered-request request_json")?;
        let provenance_payload_layout =
            super::encoding::capture_block_layout(&rendered.provenance_payload_json)
                .context("encoding rendered-request provenance payload")?;
        let provenance_json = canonical_json_string(&rendered.provenance_json)
            .context("encoding rendered-request provenance_json")?;
        let [request_chunks, provenance_chunks] =
            super::encoding::chunk_capture_payloads([&request_layout, &provenance_payload_layout]);
        let blocks = unique_blocks(&[&request_chunks, &provenance_chunks]);

        let pin_scope = CapturePinScope::for_capture(&rendered)?;
        let cached_pins = self
            .block_pins
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&pin_scope)
            .cloned()
            .unwrap_or_default();
        let capture_container = crate::config_client::ConfigAccess::transact_local_write(
            &self.node,
            None,
            "rendered_request.capture",
            |txn| {
                let (request_chunks, provenance_chunks) = (&request_chunks, &provenance_chunks);
                let (blocks, rendered, provenance_json) = (&blocks, &rendered, &provenance_json);
                let cached_pins = &cached_pins;
                let context_accounting = &context_accounting;
                Box::pin(async move {
                    let mut pins = pin_capture_blocks(txn, rendered, blocks, cached_pins).await?;
                    let mut used = std::collections::BTreeSet::new();
                    let request =
                        encode_payload_tree(txn, rendered, request_chunks, &mut pins, &mut used)
                            .await?;
                    let provenance =
                        encode_payload_tree(txn, rendered, provenance_chunks, &mut pins, &mut used)
                            .await?;
                    let container = super::encoding::encode_container(&request, &provenance)?;
                    pins.retain(|key, _| used.contains(key));
                    Self::create_in_txn(txn, rendered, &container, provenance_json).await?;
                    if let Some((call_id, accounting_json)) = context_accounting {
                        Self::persist_inference_call_context_accounting_in_txn(
                            txn,
                            call_id,
                            accounting_json,
                        )
                        .await?;
                    }
                    Ok((container, pins))
                })
            },
        )
        .await;

        match capture_container {
            Ok((_, pins)) => {
                self.block_pins
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .insert(pin_scope, pins);
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
            Err(create_error) => match self.stored_fact(&rendered.capture_key).await {
                Ok(Some(stored)) => {
                    self.reconcile_existing(&rendered, stored, "create_conflict")
                        .await?;
                    let Some((call_id, accounting_json)) = context_accounting.as_ref() else {
                        return Ok(());
                    };
                    crate::config_client::ConfigAccess::transact_local_write(
                        &self.node,
                        None,
                        "rendered_request.reconcile_context_accounting",
                        |txn| {
                            Box::pin(Self::persist_inference_call_context_accounting_in_txn(
                                txn,
                                call_id,
                                accounting_json,
                            ))
                        },
                    )
                    .await
                }
                _ => Err(create_error),
            },
        }
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
        let access = crate::config_client::ConfigAccess::local(Arc::clone(&self.node));
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

fn canonical_capture_fact(rendered: &RenderedCompletionRequest) -> Result<Value> {
    let source =
        serde_json::to_value(rendered.source).context("encoding rendered-request source")?;
    Ok(canonical_json(&serde_json::json!({
        "request_doc_id": rendered.request_doc_id,
        "request_commit_cid": rendered.request_commit_cid,
        "request_id": rendered.request_id,
        "session_id": rendered.session_id,
        "node_did": rendered.node_did,
        "requester_did": rendered.requester_did,
        "agent_id": rendered.agent_id,
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

fn unique_blocks<'a>(
    payloads: &[&'a [super::encoding::ChunkedBlock]],
) -> Vec<&'a super::encoding::ChunkedBlock> {
    let mut seen = std::collections::BTreeSet::new();
    payloads
        .iter()
        .flat_map(|chunks| chunks.iter())
        .filter(|chunk| seen.insert(chunk.content_key.as_str()))
        .collect()
}

#[derive(Clone)]
struct BlockPin {
    doc_id: String,
    field_commit_cid: String,
}

/// Typed variables preserve canonical JSON block bytes without GraphQL escaping.
const CREATE_RENDERED_REQUEST_BLOCKS_MUTATION: &str = "mutation($input: \
[RenderedRequestBlockMutationInputArg!]!) { create_RenderedRequestBlock(input: $input) { _docID content_key } }";

const CAPTURE_BLOCK_CREATE_BATCH: usize = 32;

async fn prior_block_pins_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    rendered: &RenderedCompletionRequest,
) -> Result<std::collections::BTreeMap<String, BlockPin>> {
    let source = serde_json::to_value(rendered.source)?
        .as_str()
        .context("rendered source is not a string")?
        .to_owned();
    let query = format!(
        r#"{{ {collection}(filter: {{
                node_did: {{_eq: "{node_did}"}}, requester_did: {{_eq: "{requester_did}"}},
                session_id: {{_eq: "{session_id}"}}, source: {{_eq: "{source}"}},
                capture_scope: {{_eq: "{capture_scope}"}}
            }}, order: [{{node_did: DESC}}, {{requester_did: DESC}}, {{session_id: DESC}},
                {{source: DESC}}, {{capture_scope: DESC}}, {{created_at: DESC}}], limit: 1) {{
                capture_version request_json
            }} }}"#,
        collection = RENDERED_REQUEST_COLLECTION,
        node_did = escape_graphql_string(&rendered.node_did),
        requester_did = escape_graphql_string(&rendered.requester_did),
        session_id = escape_graphql_string(&rendered.session_id),
        source = escape_graphql_string(&source),
        capture_scope = escape_graphql_string(&rendered.capture_scope),
    );
    let response = txn.execute(&query).await?;
    let mut pins = std::collections::BTreeMap::new();
    let Some(row) = response
        .get("data")
        .and_then(|data| data.get(RENDERED_REQUEST_COLLECTION))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
    else {
        return Ok(pins);
    };
    let version = row
        .get("capture_version")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    let stored = row.get("request_json").and_then(Value::as_str);
    let (Some(version), Some(stored)) = (version, stored) else {
        return Ok(pins);
    };
    let mut cache = std::collections::BTreeMap::new();
    for kind in [
        super::CapturePayloadKind::RequestBody,
        super::CapturePayloadKind::ProvenancePayload,
    ] {
        let Ok(record) = super::encoding::decode_capture_record(version, stored, kind) else {
            continue;
        };
        let entries = match record {
            super::encoding::DecodedRecord::Manifest { blocks } => blocks,
            super::encoding::DecodedRecord::Tree(tree) => {
                super::manifest_tree_leaves(txn, &tree, &mut cache)
                    .await
                    .map_err(
                        |error| match error.downcast::<super::CaptureStoreReadError>() {
                            Ok(error) => error.0,
                            Err(error) => error,
                        },
                    )?
            }
            _ => continue,
        };
        for entry in entries {
            pins.insert(
                entry.content_key,
                BlockPin {
                    doc_id: entry.doc_id,
                    field_commit_cid: entry.field_commit_cid,
                },
            );
        }
    }
    for (doc_id, block) in cache {
        pins.insert(
            block.content_key,
            BlockPin {
                doc_id,
                field_commit_cid: block.field_commit_cid,
            },
        );
    }
    Ok(pins)
}

async fn stored_block_doc_ids_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    keys: &[&str],
) -> Result<std::collections::BTreeMap<String, String>> {
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
            _docID content_key
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
            .context("stored capture block row lacks content_key")?;
        let doc_id = row
            .get("_docID")
            .and_then(Value::as_str)
            .context("stored capture block row lacks _docID")?;
        stored.insert(content_key.to_owned(), doc_id.to_owned());
    }
    Ok(stored)
}

/// Unique content-key conflicts resolve on a fresh transaction snapshot,
/// where the existence read decides the winner.
async fn create_blocks_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    blocks: &[&super::encoding::ChunkedBlock],
) -> Result<std::collections::BTreeMap<String, String>> {
    let mut created = std::collections::BTreeMap::new();
    for batch in blocks.chunks(CAPTURE_BLOCK_CREATE_BATCH) {
        let input = batch
            .iter()
            .map(|block| {
                let payload = std::str::from_utf8(&block.bytes).context(
                    "capture block bytes must be valid UTF-8; chunk boundaries land on character boundaries",
                )?;
                Ok(serde_json::json!({
                    "content_key": block.content_key,
                    "payload": payload,
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        let response = txn
            .execute_with_variables(
                CREATE_RENDERED_REQUEST_BLOCKS_MUTATION,
                &serde_json::json!({"input": input}),
            )
            .await?;
        let rows = response
            .get("data")
            .and_then(single_mutation_result)
            .and_then(Value::as_array)
            .context("creating capture blocks returned no documents")?;
        anyhow::ensure!(
            rows.len() == batch.len(),
            "capture block create omitted documents"
        );
        let expected = batch
            .iter()
            .map(|block| block.content_key.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        for row in rows {
            let key = row
                .get("content_key")
                .and_then(Value::as_str)
                .context("created capture block lacks content_key")?;
            let doc_id = row
                .get("_docID")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .context("created capture block lacks _docID")?;
            anyhow::ensure!(
                expected.contains(key),
                "capture block create returned an unexpected key"
            );
            anyhow::ensure!(
                created.insert(key.to_owned(), doc_id.to_owned()).is_none(),
                "capture block create repeated a key"
            );
        }
    }
    Ok(created)
}

async fn pin_capture_blocks(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    rendered: &RenderedCompletionRequest,
    blocks: &[&super::encoding::ChunkedBlock],
    cached: &std::collections::BTreeMap<String, BlockPin>,
) -> Result<std::collections::BTreeMap<String, BlockPin>> {
    let mut pins = cached.clone();
    if pins.is_empty() {
        pins = prior_block_pins_in_txn(txn, rendered).await?;
    }
    let keys = blocks
        .iter()
        .map(|block| block.content_key.as_str())
        .collect::<Vec<_>>();
    let mut doc_ids = stored_block_doc_ids_in_txn(txn, &keys).await?;
    for block in blocks {
        if let Some(pin) = pins.get(&block.content_key) {
            anyhow::ensure!(
                doc_ids.get(&block.content_key) == Some(&pin.doc_id),
                "capture block {} was deleted or replaced; refusing to publish a stale reference",
                pin.doc_id
            );
        }
    }
    let unpinned: Vec<&super::encoding::ChunkedBlock> = blocks
        .iter()
        .copied()
        .filter(|block| !pins.contains_key(&block.content_key))
        .collect();
    let missing = unpinned
        .iter()
        .copied()
        .filter(|block| !doc_ids.contains_key(&block.content_key))
        .collect::<Vec<_>>();
    doc_ids.extend(create_blocks_in_txn(txn, &missing).await?);
    let witness_docs = unpinned
        .iter()
        .map(|block| doc_ids[&block.content_key].clone())
        .collect::<Vec<_>>();
    let witnesses = super::commits::field_commits_in_txn(txn, &witness_docs, "payload").await?;
    for ((block, doc_id), commit) in unpinned.iter().zip(witness_docs).zip(witnesses) {
        let commit = commit
            .with_context(|| format!("capture block {doc_id} lacks a payload field commit"))?;
        pins.insert(
            block.content_key.clone(),
            BlockPin {
                doc_id,
                field_commit_cid: commit.cid,
            },
        );
    }
    Ok(pins)
}

fn payload_entries(
    chunks: &[super::encoding::ChunkedBlock],
    pins: &std::collections::BTreeMap<String, BlockPin>,
) -> Result<Vec<super::encoding::ManifestEntry>> {
    let entries = chunks
        .iter()
        .map(|chunk| {
            let pin = pins
                .get(&chunk.content_key)
                .context("capture block was not pinned")?;
            Ok(super::encoding::ManifestEntry {
                doc_id: pin.doc_id.clone(),
                content_key: chunk.content_key.clone(),
                field_commit_cid: pin.field_commit_cid.clone(),
                byte_len: u64::try_from(chunk.bytes.len())
                    .context("capture block length overflow")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(entries)
}

async fn encode_payload_tree(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    rendered: &RenderedCompletionRequest,
    chunks: &[super::encoding::ChunkedBlock],
    pins: &mut std::collections::BTreeMap<String, BlockPin>,
    used: &mut std::collections::BTreeSet<String>,
) -> Result<super::encoding::EncodedJson> {
    use super::encoding::{self, ChunkedBlock, ManifestTree};
    anyhow::ensure!(
        !chunks.is_empty() && chunks.len() <= encoding::MAX_MANIFEST_BLOCKS,
        "capture manifest exceeds leaf bound"
    );
    used.extend(chunks.iter().map(|chunk| chunk.content_key.clone()));
    let leaves = chunks.len();
    let mut entries = payload_entries(chunks, pins)?;
    let mut height = 0;
    while entries.len() > 1 {
        anyhow::ensure!(
            height < encoding::MAX_MANIFEST_HEIGHT,
            "capture manifest exceeds maximum height"
        );
        let nodes = encoding::manifest_groups(&entries)
            .into_iter()
            .map(|group| {
                let bytes = serde_json::to_vec(group)?;
                Ok(ChunkedBlock {
                    content_key: encoding::block_content_key(&bytes),
                    bytes,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let blocks = unique_blocks(&[&nodes]);
        *pins = pin_capture_blocks(txn, rendered, &blocks, pins).await?;
        used.extend(nodes.iter().map(|node| node.content_key.clone()));
        entries = payload_entries(&nodes, pins)?;
        height += 1;
    }
    encoding::encode_tree(ManifestTree {
        root: entries.remove(0),
        height,
        leaves,
    })
}

fn single_mutation_result(data: &Value) -> Option<&Value> {
    let object = data.as_object()?;
    let mut entries = object.values();
    let first = entries.next()?;
    entries.next().is_none().then_some(first)
}

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
            node_did: "did:key:test".into(),
            requester_did: String::new(),
            agent_id: "behavior".into(),
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
            .execute(r#"{ RenderedRequestBlock { _docID content_key } }"#)
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
    async fn capture_and_context_accounting_commit_together() {
        use gents_protocol::rendered_request::{
            AdmissionJoin, ContextAccounting, ContextCompactionReason, ContextInputComponents,
            ProvenanceManifest, CONTEXT_ACCOUNTING_VERSION,
        };

        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let sink = DefraRenderedRequestSink::new(Arc::clone(&node));
        crate::config_client::ConfigAccess::write_local_response(
            &node,
            "test.create_accounting_call",
            r#"mutation { create_InferenceCall(input: {
                call_id: "accounting", request_id: "request", call_state: "running"
            }) { _docID } }"#,
        )
        .await
        .unwrap();
        let with_accounting = |turn, call_id: &str, messages| {
            let mut rendered = rendered_turn(
                turn,
                json!({"model": "m", "messages": [{"role": "user", "content": "body"}]}),
            );
            rendered.assembly_trace.context_accounting = Some(ContextAccounting {
                accounting_version: CONTEXT_ACCOUNTING_VERSION,
                turn_index: turn,
                attempt: 0,
                estimator: "serialized_json_bytes_div_4_v1".into(),
                components: ContextInputComponents {
                    messages,
                    documents: 0,
                    tool_schemas: 0,
                    additional_parameters: 0,
                    output_schema: 0,
                },
                estimated_input_tokens: messages,
                context_window: 100,
                compaction_threshold_basis_points: 7500,
                compaction_threshold_tokens: 75,
                configured_max_output_tokens: Some(10),
                effective_max_output_tokens: Some(10),
                compaction_reason: ContextCompactionReason::BelowThreshold,
                pre_compaction_input_tokens: None,
            });
            rendered.provenance_payload_json =
                serde_json::to_value(&rendered.assembly_trace).unwrap();
            rendered.provenance_json = serde_json::to_value(ProvenanceManifest::captured_only(
                rendered.capture_scope.clone(),
                None,
                Some(AdmissionJoin {
                    call_id: call_id.into(),
                    call_seq: 1,
                }),
                rendered.assembly_trace.clone(),
            ))
            .unwrap();
            rendered
        };
        let first = with_accounting(0, "accounting", 1);
        sink.capture(first.clone()).await.unwrap();
        sink.capture(first.clone()).await.unwrap();
        let call = crate::graphql::graphql_with_transaction_retry(
            &node,
            r#"{ InferenceCall(filter: {call_id: {_eq: "accounting"}}) {
                context_accounting_json
            } }"#,
            "test.read_context_accounting",
        )
        .await
        .unwrap();
        let stored: Value = serde_json::from_str(
            call.data.as_ref().unwrap()["InferenceCall"][0]["context_accounting_json"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            stored,
            serde_json::to_value(first.assembly_trace.context_accounting.as_ref().unwrap())
                .unwrap()
        );
        let original_blocks = block_rows(&node).await.len();
        for rejected in [
            with_accounting(1, "accounting", 2),
            with_accounting(2, "missing", 1),
        ] {
            assert!(sink.capture(rejected).await.is_err());
            let captures = crate::graphql::graphql_with_transaction_retry(
                &node,
                "{ RenderedRequest { _docID } }",
                "test.read_captures",
            )
            .await
            .unwrap();
            assert_eq!(
                captures.data.unwrap()["RenderedRequest"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(block_rows(&node).await.len(), original_blocks);
        }
        node.shutdown().await;
    }

    #[tokio::test]
    async fn capture_writes_blocks_and_a_manifest_that_decode_back() {
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let sink = DefraRenderedRequestSink::new(Arc::clone(&node));
        let rendered = rendered_turn(
            0,
            json!({"model": "m", "messages": [{"role": "user", "content": capture_text(1, 9 * 1024)}]}),
        );
        sink.capture(rendered.clone()).await.expect("capture");

        let row = stored_capture(node.as_ref(), &rendered.capture_key).await;
        assert_eq!(
            row["capture_version"].as_u64(),
            Some(u64::from(gents_protocol::rendered_request::CAPTURE_VERSION))
        );
        let stored = row["request_json"].as_str().unwrap();
        let container: Value = serde_json::from_str(stored).unwrap();
        assert_eq!(container["request_body"]["kind"], "tree");
        assert_eq!(container["provenance_payload"]["kind"], "tree");
        let blocks = block_rows(node.as_ref()).await;
        assert!(
            blocks.len() > 1,
            "a body spanning several blocks must be stored as blocks"
        );

        let (request, provenance) = super::super::decode_capture_pair_embedded(
            node.as_ref(),
            gents_protocol::rendered_request::CAPTURE_VERSION,
            stored,
        )
        .await
        .expect("decode captured pair");
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

    #[tokio::test]
    async fn repeated_tool_output_is_stored_once_across_wire_and_native_payloads() {
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let sink = DefraRenderedRequestSink::new(Arc::clone(&node));
        let text = capture_text(73, 8 * 1024);
        let mut rendered = rendered_turn(
            0,
            json!({
                "messages": [{"role": "tool", "content": text}]
            }),
        );
        rendered.provenance_payload_json = json!({
            "effective_messages": [{"content": [{"text": text}]}],
            "threaded_tool_results": [{"content": [{"text": text}]}]
        });
        sink.capture(rendered.clone()).await.unwrap();
        let rows = block_rows(node.as_ref()).await;
        let text_chunks =
            super::super::encoding::chunk_capture_body(&serde_json::to_string(&text).unwrap());
        for chunk in text_chunks {
            assert_eq!(
                rows.iter()
                    .filter(|row| row["content_key"] == chunk.content_key)
                    .count(),
                1
            );
        }
        let row = stored_capture(node.as_ref(), &rendered.capture_key).await;
        let decoded = super::super::decode_capture_pair_embedded(
            node.as_ref(),
            super::super::CAPTURE_VERSION,
            row["request_json"].as_str().unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            decoded,
            (rendered.request_json, rendered.provenance_payload_json)
        );
        node.shutdown().await;
    }

    #[tokio::test]
    async fn a_shared_prefix_writes_only_new_blocks_and_a_small_manifest() {
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let sink = DefraRenderedRequestSink::new(Arc::clone(&node));

        let prefix = capture_text(7, 24 * 1024);
        let first = rendered_turn(
            0,
            json!({"model": "m", "messages": [{"role": "user", "content": prefix.clone()}]}),
        );
        sink.capture(first.clone()).await.expect("first capture");
        let first_blocks = block_rows(node.as_ref()).await;

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
        assert!(
            (added.len() as f64) < 0.4 * second_blocks.len() as f64,
            "{} new of {} total blocks; the shared prefix must be reused",
            added.len(),
            second_blocks.len()
        );

        let row = stored_capture(node.as_ref(), &second.capture_key).await;
        let stored = row["request_json"].as_str().unwrap();
        assert!(
            stored.len() < 3 * prefix.len() / 4,
            "the capture row must not repeat the conversation: {} bytes for a {} byte body",
            stored.len(),
            prefix.len()
        );
        assert_eq!(
            super::super::decode_capture_json_embedded(
                node.as_ref(),
                gents_protocol::rendered_request::CAPTURE_VERSION,
                stored,
                super::super::CapturePayloadKind::RequestBody,
            )
            .await
            .unwrap(),
            super::super::canonical_json(&second.request_json)
        );
        node.shutdown().await;
    }

    #[tokio::test]
    async fn a_reused_block_pins_the_existing_row_field_commit() {
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
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
        let first_entries = manifest_entries(
            node.as_ref(),
            first_row["request_json"].as_str().unwrap(),
            "request_body",
        )
        .await;
        let first_blocks = block_rows(node.as_ref()).await;

        let mut messages = json!([{"role": "user", "content": body}]);
        messages
            .as_array_mut()
            .unwrap()
            .push(json!({"role": "assistant", "content": capture_text(13, 512)}));
        let second = rendered_turn(1, json!({"model": "m", "messages": messages}));
        sink.capture(second.clone()).await.expect("second capture");

        let second_row = stored_capture(node.as_ref(), &second.capture_key).await;
        let second_entries = manifest_entries(
            node.as_ref(),
            second_row["request_json"].as_str().unwrap(),
            "request_body",
        )
        .await;
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
        assert!(block_rows(node.as_ref()).await.len() > first_blocks.len());
        node.shutdown().await;
    }

    async fn manifest_entries(
        node: &EmbeddedNode,
        stored: &str,
        payload: &str,
    ) -> Vec<(String, String, String)> {
        let kind = if payload == "request_body" {
            super::super::CapturePayloadKind::RequestBody
        } else {
            super::super::CapturePayloadKind::ProvenancePayload
        };
        let super::super::encoding::DecodedRecord::Tree(tree) =
            super::super::encoding::decode_capture_record(
                gents_protocol::rendered_request::CAPTURE_VERSION,
                stored,
                kind,
            )
            .unwrap()
        else {
            panic!("expected shared manifest")
        };
        let mut cache = std::collections::BTreeMap::new();
        super::super::read_manifest_tree(node, &tree, &mut cache)
            .await
            .unwrap();
        cache
            .into_iter()
            .map(|(doc_id, block)| (doc_id, block.content_key, block.field_commit_cid))
            .collect()
    }

    #[tokio::test]
    async fn cached_capture_pins_survive_interleaved_scopes() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let sink = DefraRenderedRequestSink::new(Arc::clone(&node));
        let body = json!({"messages":[{"role":"user","content":capture_text(83, 24 * 1024)}]});
        let first = rendered_turn(0, body.clone());
        let mut other = first.clone();
        other.session_id = "other-session".into();
        other.capture_key = super::super::capture_key(
            &other.node_did,
            &other.session_id,
            &other.request_doc_id,
            &other.capture_scope,
            other.turn_index,
            other.attempt,
        )
        .unwrap();
        sink.capture(first.clone()).await.unwrap();
        sink.capture(other.clone()).await.unwrap();
        sink.capture(rendered_turn(1, body)).await.unwrap();
        {
            let cache = sink.block_pins.lock().unwrap();
            assert_eq!(cache.len(), 2);
            assert!(cache.contains_key(&CapturePinScope::for_capture(&first).unwrap()));
            assert!(cache.contains_key(&CapturePinScope::for_capture(&other).unwrap()));
        }
        node.shutdown().await;
    }

    #[tokio::test]
    async fn deleted_blocks_never_publish_cached_or_reloaded_references() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let sink = DefraRenderedRequestSink::new(Arc::clone(&node));
        let body = json!({"messages":[{"role":"user","content":capture_text(89, 24 * 1024)}]});
        let first = rendered_turn(0, body.clone());
        sink.capture(first.clone()).await.unwrap();
        let row = stored_capture(node.as_ref(), &first.capture_key).await;
        let record = super::super::encoding::decode_capture_record(
            super::super::CAPTURE_VERSION,
            row["request_json"].as_str().unwrap(),
            super::super::CapturePayloadKind::RequestBody,
        )
        .unwrap();
        let leaves = super::super::manifest_leaf_entries(node.as_ref(), record)
            .await
            .unwrap();
        let id = escape_graphql_string(&leaves[0].doc_id);
        crate::config_client::ConfigAccess::write_local(
            &node,
            "test.delete_capture_block",
            &format!(r#"mutation {{ delete_RenderedRequestBlock(docID: "{id}") {{ _docID }} }}"#),
        )
        .await
        .unwrap();
        let next = rendered_turn(1, body);
        let error = sink.capture(next.clone()).await.unwrap_err();
        assert!(
            format!("{error:#}").contains("stale reference"),
            "{error:#}"
        );
        assert!(sink.stored_fact(&next.capture_key).await.unwrap().is_none());
        let reopened = DefraRenderedRequestSink::new(Arc::clone(&node));
        let error = reopened.capture(next.clone()).await.unwrap_err();
        assert!(
            format!("{error:#}").contains("stale reference"),
            "{error:#}"
        );
        assert!(reopened
            .stored_fact(&next.capture_key)
            .await
            .unwrap()
            .is_none());
        node.shutdown().await;
    }

    #[tokio::test]
    async fn shared_manifest_roots_stay_small_and_reconstruct_after_reopen() {
        let _trace = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_env_filter("off,gents::rendered_request::sink=info")
                .with_test_writer()
                .finish(),
        );
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let mut sink = DefraRenderedRequestSink::new(Arc::clone(&node));
        let mut messages = vec![json!({"role": "user", "content": capture_text(71, 256 * 1024)})];
        let mut first_pins = std::collections::BTreeSet::new();
        let mut flat_bytes = 0;
        let mut root_bytes = 0;
        let mut shared_nodes = std::collections::BTreeMap::new();
        for turn in 0..12 {
            if turn == 6 {
                sink = DefraRenderedRequestSink::new(Arc::clone(&node));
            }
            messages
                .push(json!({"role": "user", "content": capture_text(100 + turn as u64, 4096)}));
            let rendered = rendered_turn(turn, json!({"model":"m","messages":messages}));
            sink.capture(rendered.clone()).await.unwrap();
            let row = stored_capture(node.as_ref(), &rendered.capture_key).await;
            let stored = row["request_json"].as_str().unwrap();
            assert!(
                stored.len() < 1200,
                "capture rewrote a growing manifest: {} bytes",
                stored.len()
            );
            root_bytes += stored.len();
            let mut cache = std::collections::BTreeMap::new();
            let mut flat = Vec::new();
            for kind in [
                super::super::CapturePayloadKind::RequestBody,
                super::super::CapturePayloadKind::ProvenancePayload,
            ] {
                let super::super::encoding::DecodedRecord::Tree(tree) =
                    super::super::encoding::decode_capture_record(
                        super::super::CAPTURE_VERSION,
                        stored,
                        kind,
                    )
                    .unwrap()
                else {
                    panic!("expected shared manifest");
                };
                let leaves = super::super::manifest_tree_leaves(node.as_ref(), &tree, &mut cache)
                    .await
                    .unwrap();
                flat.push(super::super::encoding::encode_manifest(&leaves).unwrap());
            }
            flat_bytes += super::super::encoding::encode_container(&flat[0], &flat[1])
                .unwrap()
                .len();
            if turn > 0 {
                assert!(
                    cache
                        .values()
                        .any(|block| shared_nodes.contains_key(&block.content_key)),
                    "turn {turn} must reuse internal manifest nodes, including after reopening"
                );
            }
            for block in cache.values() {
                shared_nodes.insert(block.content_key.clone(), block.bytes.len());
            }
            let pins = manifest_entries(node.as_ref(), stored, "request_body").await;
            if turn == 0 {
                first_pins.extend(pins.iter().cloned());
            } else {
                assert!(
                    pins.iter().any(|pin| first_pins.contains(pin)),
                    "turn {turn} must retain original immutable references"
                );
            }
            let decoded = super::super::decode_capture_pair_embedded(
                node.as_ref(),
                gents_protocol::rendered_request::CAPTURE_VERSION,
                stored,
            )
            .await
            .unwrap();
            assert_eq!(
                decoded.0,
                super::super::canonical_json(&rendered.request_json)
            );
            assert_eq!(
                decoded.1,
                super::super::canonical_json(&rendered.provenance_payload_json)
            );
        }
        let shared_bytes = root_bytes + shared_nodes.values().sum::<usize>();
        assert!(
            shared_bytes < flat_bytes,
            "shared={shared_bytes}, flat={flat_bytes}"
        );
        tracing::info!(
            flat_bytes,
            root_bytes,
            shared_bytes,
            "capture reference storage across growing turns"
        );
        node.shutdown().await;
    }

    #[tokio::test]
    async fn same_capture_key_cannot_rebind_its_transport_route() {
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
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

    #[tokio::test]
    async fn re_delivering_a_capture_is_idempotent() {
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
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

    #[tokio::test]
    async fn concurrent_identical_captures_store_each_block_once() {
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let sink = DefraRenderedRequestSink::new(Arc::clone(&node));

        let request_json = json!({
            "model": "m",
            "messages": [{"role": "user", "content": capture_text(23, 256 * 1024)}]
        });
        let turns = (0..4)
            .map(|turn| rendered_turn(turn, request_json.clone()))
            .collect::<Vec<_>>();
        let results =
            futures::future::join_all(turns.iter().cloned().map(|rendered| sink.capture(rendered)))
                .await;
        for result in results {
            result.expect("every concurrent capture succeeds");
        }

        let rows = block_rows(node.as_ref()).await;
        let keys = rows
            .iter()
            .map(|row| row["content_key"].as_str().unwrap().to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(rows.len(), keys.len(), "one document per content key");
        let layout = super::super::encoding::capture_block_layout(&request_json).unwrap();
        let chunks = super::super::encoding::chunk_capture_body(&layout);
        assert!(chunks.len() > CAPTURE_BLOCK_CREATE_BATCH);
        for chunk in chunks {
            assert!(keys.contains(&chunk.content_key));
        }
        for rendered in &turns {
            let row = stored_capture(node.as_ref(), &rendered.capture_key).await;
            assert_eq!(
                super::super::decode_capture_json_embedded(
                    node.as_ref(),
                    super::super::CAPTURE_VERSION,
                    row["request_json"].as_str().unwrap(),
                    super::super::CapturePayloadKind::RequestBody,
                )
                .await
                .unwrap(),
                super::super::canonical_json(&request_json)
            );
        }
        node.shutdown().await;
    }

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
