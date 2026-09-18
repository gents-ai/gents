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

    async fn latest_compatible_base(
        &self,
        rendered: &RenderedCompletionRequest,
    ) -> Result<Option<Value>> {
        let source = serde_json::to_value(rendered.source)?
            .as_str()
            .context("rendered source is not a string")?
            .to_owned();
        let query = format!(
            r#"{{ RenderedRequest(filter: {{
                agent_did: {{_eq: "{agent_did}"}}, requester_did: {{_eq: "{requester_did}"}},
                session_id: {{_eq: "{session_id}"}}, source: {{_eq: "{source}"}},
                capture_scope: {{_eq: "{capture_scope}"}}
            }}, order: {{created_at: DESC}}, limit: 1) {{
                _docID capture_version agent_did requester_did session_id source capture_scope
                request_json
            }} }}"#,
            agent_did = escape_graphql_string(&rendered.agent_did),
            requester_did = escape_graphql_string(&rendered.requester_did),
            session_id = escape_graphql_string(&rendered.session_id),
            source = escape_graphql_string(&source),
            capture_scope = escape_graphql_string(&rendered.capture_scope),
        );
        let response = crate::graphql::graphql_with_transaction_retry(
            &self.node,
            &query,
            "rendered_request::latest_encoding_base",
        )
        .await?;
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get(RENDERED_REQUEST_COLLECTION))
            .and_then(Value::as_array)
            .context("reading rendered-request encoding base returned an unexpected shape")?;
        Ok(rows.first().cloned())
    }

    async fn encode_from_base(
        &self,
        rendered: &RenderedCompletionRequest,
        value: &Value,
        kind: super::CapturePayloadKind,
        base_row: Option<&Value>,
        prepared: Option<(&Value, &super::commits::RequestJsonCommit)>,
    ) -> Result<super::encoding::EncodedJson> {
        let (Some(base_row), Some((base_value, commit))) = (base_row, prepared) else {
            return super::encoding::encode_full(value);
        };
        let Some(doc_id) = base_row.get("_docID").and_then(Value::as_str) else {
            return super::encoding::encode_full(value);
        };
        let Some(stored) = base_row.get("request_json").and_then(Value::as_str) else {
            return super::encoding::encode_full(value);
        };
        let base_version = base_row
            .get("capture_version")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(1);
        if base_version != 2 {
            return super::encoding::encode_full(value);
        }
        let Ok(depth) = super::encoding::capture_record_depth(base_version, stored, kind) else {
            return super::encoding::encode_full(value);
        };
        // A depth-limit checkpoint cannot refer to this base. Avoid resolving
        // its chain (and reading its commit witnesses) merely to discard it.
        if depth >= super::encoding::MAX_DELTA_DEPTH {
            return super::encoding::encode_full(value);
        }
        let source = serde_json::to_value(rendered.source)?
            .as_str()
            .context("rendered source is not a string")?
            .to_owned();
        let encoded = super::encoding::encode_against(
            value,
            &base_value,
            super::encoding::BaseWitness {
                doc_id: doc_id.to_owned(),
                field_commit_cid: commit.cid.clone(),
                depth,
                agent_did: rendered.agent_did.clone(),
                requester_did: rendered.requester_did.clone(),
                session_id: rendered.session_id.clone(),
                source,
                capture_scope: rendered.capture_scope.clone(),
            },
        )?;
        let reconstructed = match super::encoding::decode_versioned_record(2, &encoded.stored)? {
            super::encoding::DecodedRecord::Full(value) => value,
            super::encoding::DecodedRecord::Delta {
                changed, removed, ..
            } => super::encoding::apply_delta(base_value.clone(), changed, removed)?,
            super::encoding::DecodedRecord::Legacy(_) => {
                anyhow::bail!("new capture encoding unexpectedly used legacy storage")
            }
        };
        anyhow::ensure!(
            canonical_json(&reconstructed) == canonical_json(value),
            "lossless capture encoding did not reconstruct the incoming value"
        );
        Ok(encoded)
    }

    async fn prepare_base(
        &self,
        base_row: Option<&Value>,
    ) -> Option<(
        Option<Value>,
        Option<Value>,
        super::commits::RequestJsonCommit,
    )> {
        let row = base_row?;
        let version = row
            .get("capture_version")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())?;
        let stored = row.get("request_json").and_then(Value::as_str)?;
        let doc_id = row.get("_docID").and_then(Value::as_str)?;
        if version != 2 {
            return None;
        }
        let request = super::encoding::capture_record_depth(
            version,
            stored,
            super::CapturePayloadKind::RequestBody,
        )
        .ok()
        .is_some_and(|depth| depth < super::encoding::MAX_DELTA_DEPTH);
        let provenance = super::encoding::capture_record_depth(
            version,
            stored,
            super::CapturePayloadKind::ProvenancePayload,
        )
        .ok()
        .is_some_and(|depth| depth < super::encoding::MAX_DELTA_DEPTH);
        if !request && !provenance {
            return None;
        }
        let access = crate::config_client::ConfigAccess::Local(Arc::clone(&self.node));
        let (request, provenance) =
            super::decode_capture_pair_selected(&access, version, stored, request, provenance)
                .await
                .ok()?;
        let commit = super::commits::field_commit(&access, doc_id, "request_json")
            .await
            .ok()??;
        Some((request, provenance, commit))
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

    async fn create(
        &self,
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

        // A duplicate-key error is an expected input to reconciliation, so the
        // create itself does not warn. A genuine failure is logged by the
        // transport after the re-read below cannot establish idempotency.
        let response = crate::config_client::ConfigAccess::write_local_response(
            &self.node,
            "rendered_request.create",
            &mutation,
        )
        .await?;
        // A mutation that returns no document wrote nothing, and "no errors" is
        // not the same as "durable". The field lookup is explicit rather than
        // handing the whole `data` object to `response_has_documents`, which
        // would answer for the envelope instead of for the mutation's result.
        // That result field is taken as the envelope's single entry rather than
        // by name: DefraDB answers a `create_RenderedRequest` mutation under the
        // key `add_RenderedRequest`, and hard-coding either spelling would turn
        // a rename into a silently unverified write.
        if !response
            .data
            .as_ref()
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
    pub async fn capture(&self, rendered: RenderedCompletionRequest) -> Result<()> {
        anyhow::ensure!(
            rendered.capture_version == gents_protocol::rendered_request::CAPTURE_VERSION,
            "cannot write rendered-request capture version {}; writer supports version {}",
            rendered.capture_version,
            gents_protocol::rendered_request::CAPTURE_VERSION
        );
        let base = self.latest_compatible_base(&rendered).await;
        self.capture_with_base_result(rendered, base).await
    }

    async fn capture_with_base_result(
        &self,
        rendered: RenderedCompletionRequest,
        base_result: Result<Option<Value>>,
    ) -> Result<()> {
        // Canonicalize once. The stored bytes and the complete-fact comparison
        // have to use the same representation or "identical" means nothing.
        let base = match base_result {
            Ok(base) => base,
            Err(error) => {
                tracing::warn!(
                    capture_key = %rendered.capture_key,
                    request_id = %rendered.request_id,
                    error = %error,
                    "optional rendered-request compression base was unavailable; storing a full capture"
                );
                None
            }
        };
        let prepared = self.prepare_base(base.as_ref()).await;
        let request_encoding = self
            .encode_from_base(
                &rendered,
                &rendered.request_json,
                super::CapturePayloadKind::RequestBody,
                base.as_ref(),
                prepared
                    .as_ref()
                    .and_then(|(request, _, commit)| request.as_ref().map(|value| (value, commit))),
            )
            .await
            .context("encoding rendered-request request_json")?;
        let provenance_json = canonical_json_string(&rendered.provenance_json)
            .context("encoding rendered-request provenance_json")?;
        let provenance_encoding = self
            .encode_from_base(
                &rendered,
                &rendered.provenance_payload_json,
                super::CapturePayloadKind::ProvenancePayload,
                base.as_ref(),
                prepared.as_ref().and_then(|(_, provenance, commit)| {
                    provenance.as_ref().map(|value| (value, commit))
                }),
            )
            .await
            .context("encoding rendered-request provenance payload")?;

        // The preceding read selects an optional immutable compression base;
        // this create remains the only write on the fresh path. Re-delivery and
        // races additionally pay for the conflict read and semantic decode.
        let capture_container =
            super::encoding::encode_container(&request_encoding, &provenance_encoding)?;
        let capture_result = match self
            .create(&rendered, &capture_container, &provenance_json)
            .await
        {
            Ok(()) => {
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
            Err(create_error) => {
                // A concurrent writer may have won the unique index between the
                // lookup and the create. Re-read: an identical value is still
                // an idempotent success, a different one is still an integrity
                // violation, and anything else keeps the original error.
                match self.stored_fact(&rendered.capture_key).await {
                    Ok(Some(stored)) => {
                        self.reconcile_existing(&rendered, stored, "create_conflict")
                            .await
                    }
                    _ => Err(create_error),
                }
            }
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

    fn rendered_fixture() -> RenderedCompletionRequest {
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
                0,
                0,
            )
            .unwrap(),
            capture_version: gents_protocol::rendered_request::CAPTURE_VERSION,
            request_doc_id: String::new(),
            request_commit_cid: String::new(),
            request_id: "request".into(),
            capture_scope: capture_scope.clone(),
            turn_index: 0,
            attempt: 0,
            agent_did: "did:key:test".into(),
            requester_did: String::new(),
            behavior_id: "behavior".into(),
            session_id: "session".into(),
            model_name: "model".into(),
            source: super::super::RenderedRequestSource::OpenAiChatCompletions,
            request_json: json!({"messages":[{"role":"user","content":"body"}]}),
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

    /// The collection name is interpolated as a bare GraphQL identifier, where
    /// escaping cannot defend. It is a compile-time constant from the protocol
    /// catalog, and this is the fence that keeps it a valid identifier if that
    /// catalog ever changes.
    #[test]
    fn the_collection_name_is_a_valid_graphql_identifier() {
        crate::graphql::validate_collection_identifier(RENDERED_REQUEST_COLLECTION)
            .expect("RenderedRequest must be a valid GraphQL identifier");
        assert_eq!(RENDERED_REQUEST_COLLECTION, "RenderedRequest");
    }

    /// The shape DefraDB actually answers a `create_RenderedRequest` mutation
    /// with — note the `add_` key. A create whose result cannot be found reads
    /// as "wrote nothing", so this is the difference between verifying the
    /// write and assuming it.
    #[test]
    fn a_create_envelope_yields_its_single_result_field() {
        use crate::graphql::response_has_documents;
        use serde_json::json;

        let created = json!({ "add_RenderedRequest": [{ "_docID": "bae-1" }] });
        assert!(response_has_documents(
            single_mutation_result(&created).expect("one result field")
        ));

        let wrote_nothing = json!({ "add_RenderedRequest": [] });
        assert!(!response_has_documents(
            single_mutation_result(&wrote_nothing).expect("one result field")
        ));

        // An envelope this sink does not recognise must not read as a write.
        assert!(single_mutation_result(&json!({ "a": [], "b": [] })).is_none());
        assert!(single_mutation_result(&json!([])).is_none());
        assert!(single_mutation_result(&json!({})).is_none());
    }

    #[tokio::test]
    async fn optional_base_lookup_failure_still_creates_a_full_capture() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let sink = DefraRenderedRequestSink::new(Arc::clone(&node));
        let rendered = rendered_fixture();

        sink.capture_with_base_result(
            rendered.clone(),
            Err(anyhow!("injected latest-base read failure")),
        )
        .await
        .expect("optional lookup failure must not block a new full capture");

        let response = node
            .execute(&format!(
                r#"{{RenderedRequest(filter:{{capture_key:{{_eq:"{}"}}}}){{capture_version request_json}}}}"#,
                escape_graphql_string(&rendered.capture_key)
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let row = &response.data.unwrap()["RenderedRequest"][0];
        let stored = row["request_json"].as_str().unwrap();
        let container: Value = serde_json::from_str(stored).unwrap();
        assert_eq!(container["request_body"]["kind"], "full");
        assert_eq!(container["provenance_payload"]["kind"], "full");
        assert_eq!(
            super::super::decode_capture_json_embedded(
                node.as_ref(),
                2,
                stored,
                super::super::CapturePayloadKind::RequestBody,
            )
            .await
            .unwrap(),
            super::super::canonical_json(&rendered.request_json)
        );
    }
}
