//! The durable writer behind the capture seam.
//!
//! `Proofs/RenderedCapture.lean` specifies this function exactly, and the three
//! outcomes are not negotiable:
//!
//! | store state for `capture_key` | outcome | write |
//! |---|---|---|
//! | unbound | `fresh` | create |
//! | bound to the identical canonical `request_json` | `idempotent` | none |
//! | bound to a *different* canonical `request_json` | `rejected` | none, and an error |
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
//! Writes go through `EmbeddedNode::execute_request_with_retry` with the
//! agent's DID attached to the `QueryRequest`, not through the identity-less
//! `EmbeddedNode::execute`. Today that only makes the write attributable —
//! `RenderedRequest` carries no `@policy`, and ACP is blocked on
//! defradb.rs#1318 — but it means the write path does not have to change when
//! a policy can finally be installed. A DID that does not parse degrades to an
//! anonymous write with a warning rather than failing the request: refusing to
//! capture would refuse the provider call, and a malformed principal is a
//! configuration defect, not a reason to take the agent offline.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use defra_node::{EmbeddedNode, ExecuteRetryPolicy, QueryRequest, QueryResponse};
use identity::Did;
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
    identity: Option<Did>,
}

impl std::fmt::Debug for DefraRenderedRequestSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefraRenderedRequestSink")
            .field(
                "identity",
                &self.identity.as_ref().map(|did| did.as_str().to_string()),
            )
            .finish()
    }
}

impl DefraRenderedRequestSink {
    pub fn new(node: Arc<EmbeddedNode>, agent_did: &str) -> Self {
        let identity = match Did::new(agent_did) {
            Ok(did) => Some(did),
            Err(error) => {
                tracing::warn!(
                    agent_did = %agent_did,
                    error = %error,
                    "rendered-request captures will be written anonymously: agent DID is not a did:key"
                );
                None
            }
        };
        Self { node, identity }
    }

    async fn execute(&self, graphql: &str, operation: &str) -> QueryResponse {
        let response = self
            .node
            .execute_request_with_retry(
                QueryRequest::new(graphql).with_identity(self.identity.clone()),
                ExecuteRetryPolicy::default(),
            )
            .await;
        if response.has_errors() {
            tracing::warn!(
                operation = %operation,
                errors = ?response.errors,
                "rendered-request capture statement failed"
            );
        }
        response
    }

    /// The canonical `request_json` already stored under `capture_key`, if any.
    ///
    /// A GraphQL error is an error, never "no rows": treating a failed read as
    /// an unbound key would turn a transient DB fault into a duplicate-key
    /// create and, worse, into a silent rebinding attempt.
    async fn stored_request_json(&self, capture_key: &str) -> Result<Option<String>> {
        let query = format!(
            r#"query {{
                {collection}(filter: {{ capture_key: {{ _eq: "{capture_key}" }} }}, limit: 2) {{
                    request_json
                }}
            }}"#,
            collection = RENDERED_REQUEST_COLLECTION,
            capture_key = escape_graphql_string(capture_key),
        );
        let response = self.execute(&query, "rendered_request::lookup").await;
        if response.has_errors() {
            return Err(anyhow!(
                "reading RenderedRequest by capture key failed: {:?}",
                response.errors
            ));
        }
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
            1 => Ok(Some(
                rows[0]
                    .get("request_json")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )),
            // The unique index makes this unreachable; if it ever happens the
            // fact record is already ambiguous and must not be extended.
            count => Err(anyhow!(
                "capture key {capture_key} matched {count} RenderedRequest rows; the unique index is not enforcing"
            )),
        }
    }

    async fn create(&self, rendered: &RenderedCompletionRequest, request_json: &str) -> Result<()> {
        let provenance_json = canonical_json_string(&rendered.provenance_json)
            .context("encoding rendered-request provenance_json")?;
        let source = serde_json::to_value(rendered.source)
            .ok()
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| "unknown".to_string());
        let mutation = format!(
            r#"mutation {{
                create_{collection}(input: {{
                    capture_key: "{capture_key}",
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
                    prompt_hash: "{prompt_hash}",
                    tools_hash: "{tools_hash}",
                    provenance_json: "{provenance_json}",
                    created_at: "{created_at}"
                }}) {{ _docID }}
            }}"#,
            collection = RENDERED_REQUEST_COLLECTION,
            capture_key = escape_graphql_string(&rendered.capture_key),
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
            prompt_hash = escape_graphql_string(&rendered.prompt_hash),
            tools_hash = escape_graphql_string(&rendered.tools_hash),
            provenance_json = escape_graphql_string(&provenance_json),
            created_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339()),
        );

        let response = self.execute(&mutation, "rendered_request::create").await;
        if response.has_errors() {
            return Err(anyhow!(
                "creating RenderedRequest failed: {:?}",
                response.errors
            ));
        }
        // A mutation that returns no document wrote nothing, and "no errors" is
        // not the same as "durable". The field lookup is explicit rather than
        // handing the whole `data` object to `response_has_documents`, which
        // would answer for the envelope instead of for the mutation's result.
        // A mutation that returns no document wrote nothing, and "no errors" is
        // not the same as "durable". The result field is looked up by taking the
        // envelope's single entry rather than by name: DefraDB answers a
        // `create_RenderedRequest` mutation under the key `add_RenderedRequest`,
        // and hard-coding either spelling would turn a rename into a silently
        // unverified write.
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
        // Canonicalize once. The stored bytes, the comparison, and any future
        // digest all have to be the same string or "identical" means nothing.
        let request_json = canonical_json_string(&rendered.request_json)
            .context("encoding rendered-request request_json")?;

        if let Some(stored) = self.stored_request_json(&rendered.capture_key).await? {
            return self.reconcile_existing(&rendered, &request_json, stored, "lookup");
        }

        match self.create(&rendered, &request_json).await {
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
                match self.stored_request_json(&rendered.capture_key).await {
                    Ok(Some(stored)) => {
                        self.reconcile_existing(&rendered, &request_json, stored, "create_conflict")
                    }
                    _ => Err(create_error),
                }
            }
        }
    }

    fn reconcile_existing(
        &self,
        rendered: &RenderedCompletionRequest,
        request_json: &str,
        stored: String,
        via: &str,
    ) -> Result<()> {
        // Compare canonical *values* as well as raw strings: a row written by
        // an older build could differ only in key order, and that is the same
        // fact. An unparseable stored value is `None`, never `Value::Null` —
        // "we could not read it" and "it is null" are different answers and
        // only the first may be treated as a mismatch.
        let stored_value = serde_json::from_str::<Value>(&stored)
            .ok()
            .map(|value| canonical_json(&value));
        let incoming_value = canonical_json(&rendered.request_json);
        if stored == request_json || stored_value.as_ref() == Some(&incoming_value) {
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
            stored_bytes = stored.len(),
            incoming_bytes = request_json.len(),
            "rendered-request capture key already names a different provider request"
        );
        Err(anyhow!(
            "rendered-request integrity violation: capture key {} already names a different \
             canonical request (stored {} bytes, incoming {} bytes); a capture key is never \
             rebound",
            rendered.capture_key,
            stored.len(),
            request_json.len(),
        ))
    }
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
/// through the same node under the requesting agent's DID.
pub fn defra_rendered_request_capture_factory(
    node: Arc<EmbeddedNode>,
) -> RenderedRequestCaptureFactory {
    Arc::new(move |context: RenderedRequestContext| {
        let sink = DefraRenderedRequestSink::new(Arc::clone(&node), &context.agent_did);
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
