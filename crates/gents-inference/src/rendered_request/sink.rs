//! Captures, shared payload blocks and inference accounting use the selected agent
//! actor. Routing and requester columns do not confer document authority.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use defra_node::Node as EmbeddedNode;
use serde_json::Value;

async fn capture_session_binding(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    rendered: &RenderedCompletionRequest,
) -> Result<(String, String)> {
    let scope: gents_protocol::rendered_request::CaptureScope = rendered.capture_scope.parse()?;
    session_child_binding_for_request_in_txn(
        txn,
        &rendered.node_did,
        &rendered.session_id,
        (!rendered.requester_did.is_empty()).then_some(rendered.requester_did.as_str()),
        &rendered.request_doc_id,
        SessionChildSource::Capture(&scope),
    )
    .await
}

use super::{
    canonical_json, canonical_json_string, RenderedCompletionRequest,
    RenderedRequestCaptureFactory, RenderedRequestCaptureSink, RenderedRequestContext,
};
use gents_store::config_client::session_children::{
    session_child_binding_for_request_in_txn, SessionChildSource,
};
use gents_store::graphql::escape_graphql_string;

#[derive(Clone)]
pub struct DefraRenderedRequestSink {
    node: Arc<EmbeddedNode>,
    agent_signer: Option<gents_store::identity::ManagedAgentSigner>,
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
            agent_signer: None,
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn with_signer(
        node: Arc<EmbeddedNode>,
        agent_signer: gents_store::identity::ManagedAgentSigner,
    ) -> Self {
        Self {
            node,
            agent_signer: Some(agent_signer),
        }
    }

    pub fn new_managed(
        node: Arc<EmbeddedNode>,
        identity: &dyn gents_store::identity::NodeIdentity,
    ) -> Result<Self> {
        let signer = identity
            .managed_agent_signer()
            .context("rendered request capture requires a managed Agent signer")?;
        anyhow::ensure!(
            identity.did() == signer.did() && node.node_identity_did() == Some(signer.node_did()),
            "capture identity differs from managed Agent signer or node"
        );
        Ok(Self {
            node,
            agent_signer: Some(signer),
        })
    }

    fn access_for_capture(
        &self,
        rendered: &RenderedCompletionRequest,
    ) -> Result<gents_store::config_client::ConfigAccess> {
        let actor = identity::Did::new(rendered.agent_did.clone())?;
        if let Some(signer) = &self.agent_signer {
            anyhow::ensure!(
                signer.did() == rendered.agent_did && signer.node_did() == rendered.node_did,
                "rendered request differs from managed Agent signer"
            );
            Ok(
                gents_store::config_client::ConfigAccess::local_with_agent_signer(
                    self.node.clone(),
                    actor,
                    signer.clone(),
                )?,
            )
        } else {
            anyhow::bail!("rendered request requires a managed Agent signer")
        }
    }

    pub async fn stored_fact(&self, rendered: &RenderedCompletionRequest) -> Result<Option<Value>> {
        let capture_key = &rendered.capture_key;
        let response = self
            .access_for_capture(rendered)?
            .transact_readonly("rendered_request.lookup", |txn| {
                Box::pin(async move {
                    let (session_doc_id, _) = capture_session_binding(txn, rendered).await?;
                    use gents_protocol::document_query::{
                        fields::rendered_request as fields, Filter, Query, Selection,
                    };
                    let query = Query::select(
                        Selection::new()
                            .field(fields::REQUEST_DOC_ID)
                            .field(fields::REQUEST_COMMIT_CID)
                            .field(fields::REQUEST_ID)
                            .field(fields::SESSION_ID)
                            .field(fields::NODE_DID)
                            .field(fields::REQUESTER_DID)
                            .field(fields::AGENT_DID)
                            .field(fields::CAPTURE_SCOPE)
                            .field(fields::TURN_INDEX)
                            .field(fields::ATTEMPT)
                            .field(fields::CAPTURE_VERSION)
                            .field(fields::MODEL_NAME)
                            .field(fields::SOURCE)
                            .field(fields::REQUEST_JSON)
                            .field(fields::PROVENANCE_JSON),
                    )
                    .filter(Filter::eq(fields::SESSION_DOC_ID, session_doc_id.as_str()))
                    .filter(Filter::eq(fields::CAPTURE_KEY, capture_key))
                    .limit(2)
                    .render();
                    txn.run_local_response(&query)
                        .await
                        .map_err(anyhow::Error::from)
                })
            })
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
        txn: &gents_store::config_client::ConfigApplyTxn<'_>,
        rendered: &RenderedCompletionRequest,
        session_doc_id: &str,
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
        let mutation = {
            use gents_protocol::document_query::{
                fields::rendered_request as fields, Create, Input,
            };
            Create::new(
                Input::new()
                    .set(fields::CAPTURE_KEY, &rendered.capture_key)
                    .set(fields::REQUEST_DOC_ID, &rendered.request_doc_id)
                    .set(fields::REQUEST_COMMIT_CID, &rendered.request_commit_cid)
                    .set(fields::REQUEST_ID, &rendered.request_id)
                    .set(fields::SESSION_ID, &rendered.session_id)
                    .set(fields::SESSION_DOC_ID, session_doc_id)
                    .set(fields::NODE_DID, &rendered.node_did)
                    .set(fields::REQUESTER_DID, &rendered.requester_did)
                    .set(fields::AGENT_DID, &rendered.agent_did)
                    .set(fields::CAPTURE_SCOPE, &rendered.capture_scope)
                    .set(fields::TURN_INDEX, rendered.turn_index)
                    .set(fields::ATTEMPT, rendered.attempt)
                    .set(fields::CAPTURE_VERSION, rendered.capture_version)
                    .set(fields::MODEL_NAME, &rendered.model_name)
                    .set(fields::SOURCE, source.as_str())
                    .set(fields::REQUEST_JSON, request_json)
                    .set(fields::PROVENANCE_JSON, provenance_json)
                    .set(fields::CREATED_AT, chrono::Utc::now().to_rfc3339()),
            )
            .render()
        };

        let response = txn.run(&mutation).await?;
        if !response
            .get("data")
            .and_then(single_mutation_result)
            .is_some_and(gents_store::graphql::response_has_documents)
        {
            return Err(anyhow!(
                "creating RenderedRequest returned no document; the capture is not durable"
            ));
        }
        let doc_id = gents_store::graphql::created_doc_id(&response, RENDERED_REQUEST_COLLECTION)?;
        txn.add_resource_relationship(
            RENDERED_REQUEST_COLLECTION,
            &doc_id,
            "parent",
            "sessions",
            session_doc_id,
        )
        .await?;
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
        txn: &gents_store::config_client::ConfigApplyTxn<'_>,
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
            let incoming: Value = serde_json::from_str(accounting_json)
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
            accounting_json = escape_graphql_string(accounting_json),
        );
        let response = txn.execute(&mutation).await?;
        if !response
            .get("data")
            .and_then(single_mutation_result)
            .is_some_and(gents_store::graphql::response_has_documents)
        {
            anyhow::bail!("updating InferenceCall context accounting returned no document");
        }
        Ok(())
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub async fn create_stored_for_test(
        &self,
        rendered: &RenderedCompletionRequest,
        request_json: &str,
    ) -> Result<()> {
        let provenance_json = canonical_json_string(&rendered.provenance_json)?;
        self.access_for_capture(rendered)?
            .transact_write("rendered_request.create_stored_for_test", |txn| {
                let rendered = rendered.clone();
                let request_json = request_json.to_owned();
                let provenance_json = provenance_json.clone();
                Box::pin(async move {
                    let (session_doc_id, _) = capture_session_binding(txn, &rendered).await?;
                    Self::create_in_txn(
                        txn,
                        &rendered,
                        &session_doc_id,
                        &request_json,
                        &provenance_json,
                    )
                    .await
                })
            })
            .await
    }

    pub async fn capture(&self, rendered: RenderedCompletionRequest) -> Result<()> {
        anyhow::ensure!(
            rendered.capture_version == gents_protocol::rendered_request::CAPTURE_VERSION,
            "cannot write rendered-request capture version {}; writer supports version {}",
            rendered.capture_version,
            gents_protocol::rendered_request::CAPTURE_VERSION
        );
        let access = self.access_for_capture(&rendered)?;
        let context_accounting = Self::inference_call_context_accounting(&rendered)?;
        let request_layout = super::encoding::capture_block_layout(&rendered.request_json)
            .context("encoding rendered-request request_json")?;
        let provenance_payload_layout =
            super::encoding::capture_block_layout(&rendered.provenance_payload_json)
                .context("encoding rendered-request provenance payload")?;
        let provenance_json = canonical_json_string(&rendered.provenance_json)
            .context("encoding rendered-request provenance_json")?;
        let request_chunks = super::encoding::chunk_capture_layout(&request_layout);
        let provenance_chunks = super::encoding::chunk_capture_layout(&provenance_payload_layout);
        let blocks = unique_blocks(&[&request_chunks, &provenance_chunks]);

        let capture_container = access
            .transact_write("rendered_request.capture", |txn| {
                let (request_chunks, provenance_chunks) = (&request_chunks, &provenance_chunks);
                let (blocks, rendered, provenance_json) = (&blocks, &rendered, &provenance_json);
                let context_accounting = &context_accounting;
                Box::pin(async move {
                    let (session_doc_id, agent_did) =
                        capture_session_binding(txn, rendered).await?;
                    anyhow::ensure!(
                        agent_did == rendered.agent_did,
                        "capture agent differs from session agent"
                    );
                    let scope = BlockScope {
                        session_doc_id,
                        agent_did,
                    };
                    let mut pins = pin_blocks_in_txn(txn, &scope, blocks).await?;
                    let request =
                        encode_payload_tree(txn, &scope, request_chunks, &mut pins).await?;
                    let provenance =
                        encode_payload_tree(txn, &scope, provenance_chunks, &mut pins).await?;
                    let container = super::encoding::encode_container(&request, &provenance)?;
                    Self::create_in_txn(
                        txn,
                        rendered,
                        &scope.session_doc_id,
                        &container,
                        provenance_json,
                    )
                    .await?;
                    if let Some((call_id, accounting_json)) = context_accounting {
                        Self::persist_inference_call_context_accounting_in_txn(
                            txn,
                            call_id,
                            accounting_json,
                        )
                        .await?;
                    }
                    Ok(container)
                })
            })
            .await;

        match capture_container {
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
            Err(create_error) => match self.stored_fact(&rendered).await {
                Ok(Some(stored)) => {
                    self.reconcile_existing(&rendered, stored, "create_conflict")
                        .await?;
                    let Some((call_id, accounting_json)) = context_accounting.as_ref() else {
                        return Ok(());
                    };
                    access
                        .transact_write("rendered_request.reconcile_context_accounting", |txn| {
                            Box::pin(Self::persist_inference_call_context_accounting_in_txn(
                                txn,
                                call_id,
                                accounting_json,
                            ))
                        })
                        .await
                }
                Ok(None) => Err(create_error.context(
                    "no RenderedRequest matches the capture key to reconcile with the failed create",
                )),
                Err(lookup_error) => Err(create_error.context(format!(
                    "re-reading the RenderedRequest by capture key after the failed create also failed: {lookup_error:#}"
                ))),
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
        let stored = self.canonical_stored_fact(stored, rendered).await?;
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

    async fn canonical_stored_fact(
        &self,
        mut stored: Value,
        rendered: &RenderedCompletionRequest,
    ) -> Result<Value> {
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
        let access = self.access_for_capture(rendered)?;
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
        "agent_did": rendered.agent_did,
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

pub const RENDERED_REQUEST_COLLECTION: &str = gents_protocol::schemas::RENDERED_REQUEST_NAME;

pub const RENDERED_REQUEST_BLOCK_COLLECTION: &str =
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

pub const CAPTURE_BLOCK_CREATE_BATCH: usize = 32;

pub struct BlockScope {
    pub session_doc_id: String,
    pub agent_did: String,
}

impl BlockScope {
    pub fn key<'a>(&'a self, content_key: &'a str) -> (&'a str, &'a str) {
        (&self.session_doc_id, content_key)
    }
}

async fn stored_block_doc_ids_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    scope: &BlockScope,
    keys: &[&str],
) -> Result<std::collections::BTreeMap<String, String>> {
    let mut stored = std::collections::BTreeMap::new();
    if keys.is_empty() {
        return Ok(stored);
    }
    let query = {
        use gents_protocol::document_query::{
            fields::rendered_request_block as fields, Filter, Query, Selection,
        };
        Query::select(Selection::doc_id().field(fields::CONTENT_KEY))
            .filter(Filter::eq(fields::SESSION_DOC_ID, scope.key("").0))
            .filter(Filter::any_of(
                fields::CONTENT_KEY,
                keys.iter().map(|key| scope.key(key).1),
            ))
            .render()
    };
    let response = txn.run(&query).await?;
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
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    scope: &BlockScope,
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
                    "session_doc_id": scope.session_doc_id,
                    "agent_did": scope.agent_did,
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
            txn.add_resource_relationship(
                RENDERED_REQUEST_BLOCK_COLLECTION,
                doc_id,
                "parent",
                "sessions",
                &scope.session_doc_id,
            )
            .await?;
        }
    }
    Ok(created)
}

async fn pin_blocks_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    scope: &BlockScope,
    blocks: &[&super::encoding::ChunkedBlock],
) -> Result<std::collections::BTreeMap<String, BlockPin>> {
    let mut pins = std::collections::BTreeMap::new();
    let keys = blocks
        .iter()
        .map(|block| block.content_key.as_str())
        .collect::<Vec<_>>();
    let mut doc_ids = stored_block_doc_ids_in_txn(txn, scope, &keys).await?;
    let missing = blocks
        .iter()
        .copied()
        .filter(|block| !doc_ids.contains_key(&block.content_key))
        .collect::<Vec<_>>();
    doc_ids.extend(create_blocks_in_txn(txn, scope, &missing).await?);
    let witness_docs = blocks
        .iter()
        .map(|block| doc_ids[&block.content_key].clone())
        .collect::<Vec<_>>();
    let witnesses = super::commits::field_commits_in_txn(txn, &witness_docs, "payload").await?;
    for ((block, doc_id), commit) in blocks.iter().zip(witness_docs).zip(witnesses) {
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
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    scope: &BlockScope,
    chunks: &[super::encoding::ChunkedBlock],
    pins: &mut std::collections::BTreeMap<String, BlockPin>,
) -> Result<super::encoding::EncodedJson> {
    use super::encoding::{self, ChunkedBlock, ManifestTree};
    anyhow::ensure!(
        !chunks.is_empty() && chunks.len() <= encoding::MAX_MANIFEST_BLOCKS,
        "capture manifest exceeds leaf bound"
    );
    let leaves = chunks.len();
    let mut entries = payload_entries(chunks, pins)?;
    let mut boundary_keys = chunks
        .iter()
        .map(|chunk| chunk.content_key.clone())
        .collect::<Vec<_>>();
    let mut height = 0;
    while entries.len() > 1 {
        anyhow::ensure!(
            height < encoding::MAX_MANIFEST_HEIGHT,
            "capture manifest exceeds maximum height"
        );
        let groups = encoding::manifest_groups(&boundary_keys);
        let nodes = groups
            .iter()
            .map(|range| {
                let bytes = serde_json::to_vec(&entries[range.clone()])?;
                Ok(ChunkedBlock {
                    content_key: encoding::block_content_key(&bytes),
                    bytes,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        boundary_keys = groups
            .into_iter()
            .map(|range| encoding::manifest_node_boundary_key(&boundary_keys[range]))
            .collect();
        let blocks = unique_blocks(&[&nodes]);
        pins.extend(pin_blocks_in_txn(txn, scope, &blocks).await?);
        entries = payload_entries(&nodes, pins)?;
        height += 1;
    }
    encoding::encode_tree(ManifestTree {
        root: entries.remove(0),
        height,
        leaves,
    })
}

pub async fn store_shared_json_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    scope: &BlockScope,
    json: &str,
) -> Result<String> {
    if json.len() < super::encoding::MIN_COMPRESSION_BYTES {
        return Ok(json.to_owned());
    }
    let chunks = super::encoding::chunk_capture_body(json);
    if chunks.len() > super::encoding::MAX_MANIFEST_BLOCKS {
        return Ok(json.to_owned());
    }
    let blocks = unique_blocks(&[&chunks]);
    let mut pins = pin_blocks_in_txn(txn, scope, &blocks).await?;
    Ok(encode_payload_tree(txn, scope, &chunks, &mut pins)
        .await?
        .stored)
}

fn single_mutation_result(data: &Value) -> Option<&Value> {
    let object = data.as_object()?;
    let mut entries = object.values();
    let first = entries.next()?;
    entries.next().is_none().then_some(first)
}

pub fn defra_rendered_request_capture_factory_for_agent(
    node: Arc<EmbeddedNode>,
    identity: &dyn gents_store::identity::NodeIdentity,
) -> Result<RenderedRequestCaptureFactory> {
    Ok(capture_factory(DefraRenderedRequestSink::new_managed(
        node, identity,
    )?))
}

/// Fixture factory for a claimed request whose lifecycle holds the Agent's
/// managed signer; captures persist as that Agent, as in production.
#[cfg(any(test, feature = "test-fixtures"))]
pub fn defra_rendered_request_capture_factory_for_signer(
    node: Arc<EmbeddedNode>,
    signer: gents_store::identity::ManagedAgentSigner,
) -> Result<RenderedRequestCaptureFactory> {
    anyhow::ensure!(
        node.node_identity_did() == Some(signer.node_did()),
        "capture signer differs from the node"
    );
    Ok(capture_factory(DefraRenderedRequestSink {
        node,
        agent_signer: Some(signer),
    }))
}

fn capture_factory(sink: DefraRenderedRequestSink) -> RenderedRequestCaptureFactory {
    Arc::new(move |_context: RenderedRequestContext| {
        let sink = sink.clone();
        let sink: RenderedRequestCaptureSink = Arc::new(move |rendered| {
            let sink = sink.clone();
            Box::pin(async move {
                sink.capture(rendered).await.map_err(|error| {
                    gents_loop::rendered_request::RenderedRequestError::Sink(error.into())
                })
            })
        });
        sink
    })
}
