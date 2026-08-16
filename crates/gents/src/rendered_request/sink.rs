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
        let response = crate::graphql::graphql_mutation_with_transaction_retry(
            &self.node,
            &mutation,
            "rendered_request::create",
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

    /// Persist one capture. See the outcome table at the top of this module.
    pub async fn capture(&self, rendered: RenderedCompletionRequest) -> Result<()> {
        // Canonicalize once. The stored bytes and the complete-fact comparison
        // have to use the same representation or "identical" means nothing.
        let request_json = canonical_json_string(&rendered.request_json)
            .context("encoding rendered-request request_json")?;
        let provenance_json = canonical_json_string(&rendered.provenance_json)
            .context("encoding rendered-request provenance_json")?;

        // Create first. Fresh captures are overwhelmingly the common path, and
        // now cost one durable statement rather than a lookup plus a mutation.
        // Only re-delivery and races pay for the conflict read.
        match self
            .create(&rendered, &request_json, &provenance_json)
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
                    }
                    _ => Err(create_error),
                }
            }
        }
    }

    fn reconcile_existing(
        &self,
        rendered: &RenderedCompletionRequest,
        stored: Value,
        via: &str,
    ) -> Result<()> {
        let incoming = canonical_capture_fact(rendered)?;
        let stored = canonical_stored_fact(stored)?;
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
        "capture_version": rendered.capture_version,
        "model_name": rendered.model_name,
        "source": source,
        "request_json": canonical_json(&rendered.request_json),
        "provenance_json": canonical_json(&rendered.provenance_json),
    })))
}

fn canonical_stored_fact(mut stored: Value) -> Result<Value> {
    let object = stored
        .as_object_mut()
        .ok_or_else(|| anyhow!("stored RenderedRequest fact was not an object"))?;
    for field in ["request_json", "provenance_json"] {
        let encoded = object
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("stored RenderedRequest {field} was not a string"))?;
        let decoded: Value = serde_json::from_str(encoded)
            .with_context(|| format!("decoding stored RenderedRequest {field}"))?;
        object.insert(field.to_string(), canonical_json(&decoded));
    }
    Ok(canonical_json(&stored))
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
}
