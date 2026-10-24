use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub mod scope;
pub mod transport;

pub use gents_protocol::rendered_request::{
    AdmissionJoin, AssemblyBuildPath, AssemblyTrace, AssemblyTraceMetadata, AssistantMessageId,
    CaptureOrderKey, CaptureScope, CaptureScopeKind, CaptureSeam, ContextAccounting,
    ContextCompactionReason, ContextInputComponents, ParsedProvenance, ProvenanceManifest,
    ProvenanceStatus, RenderedRequestSource, ThreadedToolResult, ASSEMBLY_TRACE_VERSION,
    CAPTURE_VERSION, CONTEXT_ACCOUNTING_VERSION, PROVENANCE_MANIFEST_VERSION,
};
pub use transport::RenderedRequestCapturingHttpClient;

const CAPTURE_KEY_PREFIX: &str = "rendered:v1";

#[derive(Debug, thiserror::Error)]
pub enum RenderedRequestError {
    #[error("encoding rendered-request provenance")]
    EncodeProvenance(#[source] serde_json::Error),
    #[error("encoding rendered-request provenance payload")]
    EncodeProvenancePayload(#[source] serde_json::Error),
    #[error("encoding canonical JSON")]
    EncodeCanonicalJson(#[source] serde_json::Error),
}

type Result<T, E = RenderedRequestError> = std::result::Result<T, E>;

pub type RenderedRequestCaptureSink = Arc<
    dyn Fn(RenderedCompletionRequest) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>
        + Send
        + Sync,
>;

pub type RenderedRequestCaptureFactory =
    Arc<dyn Fn(RenderedRequestContext) -> RenderedRequestCaptureSink + Send + Sync>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderedRequestContext {
    pub request_doc_id: String,
    pub request_commit_cid: String,
    pub request_id: String,
    pub node_did: String,
    pub requester_did: String,
    pub agent_id: String,
    pub session_id: String,
    pub model_name: String,
    pub provider_family: Option<String>,
}

pub struct RenderedRequestComponents {
    pub request_json: Value,
    pub messages_json: Value,
    pub tools_json: Value,
    pub tool_choice_json: Value,
    pub sampling_json: Value,
}

impl RenderedRequestComponents {
    pub fn from_provider_body(request_json: Value, source: RenderedRequestSource) -> Self {
        let field = |name: &str| request_json.get(name).cloned();
        let messages_json =
            field(source.messages_field()).unwrap_or_else(|| Value::Array(Vec::new()));
        let tools_json = field("tools").unwrap_or_else(|| Value::Array(Vec::new()));
        let tool_choice_json = field("tool_choice").unwrap_or(Value::Null);
        let sampling_json = json!({
            "temperature": field("temperature").unwrap_or(Value::Null),
            "top_p": field("top_p").unwrap_or(Value::Null),
            "max_tokens": field("max_tokens")
                .or_else(|| field("max_output_tokens"))
                .unwrap_or(Value::Null),
            "reasoning": field("reasoning").unwrap_or(Value::Null),
        });

        Self {
            request_json,
            messages_json,
            tools_json,
            tool_choice_json,
            sampling_json,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderedCompletionRequest {
    pub capture_key: String,
    pub capture_version: u32,
    pub request_doc_id: String,
    pub request_commit_cid: String,
    pub request_id: String,
    pub capture_scope: String,
    pub turn_index: usize,
    pub attempt: u32,
    pub node_did: String,
    pub requester_did: String,
    pub agent_id: String,
    pub session_id: String,
    pub model_name: String,
    pub source: RenderedRequestSource,
    pub request_json: Value,
    pub messages_json: Value,
    pub tools_json: Value,
    pub tool_choice_json: Value,
    pub sampling_json: Value,
    pub assembly_trace: AssemblyTrace,
    pub provenance_json: Value,
    pub provenance_payload_json: Value,
}

#[allow(clippy::too_many_arguments)]
pub fn build_rendered_completion_request(
    context: &RenderedRequestContext,
    capture_scope: &str,
    source: RenderedRequestSource,
    provider_endpoint: Option<String>,
    provider_route_path_sha256: Option<String>,
    turn_index: usize,
    attempt: u32,
    assembly_trace: AssemblyTrace,
    components: RenderedRequestComponents,
    admission_join: Option<AdmissionJoin>,
) -> Result<RenderedCompletionRequest> {
    let RenderedRequestComponents {
        request_json,
        messages_json,
        tools_json,
        tool_choice_json,
        sampling_json,
    } = components;

    let capture_key = capture_key(
        &context.node_did,
        &context.session_id,
        &context.request_doc_id,
        capture_scope,
        turn_index,
        attempt,
    )?;
    let manifest = ProvenanceManifest::captured_only(
        capture_scope.to_string(),
        provider_endpoint,
        admission_join,
        assembly_trace.clone(),
    )
    .with_provider_route(context.provider_family.clone(), provider_route_path_sha256);
    let provenance_json = canonical_json(
        &serde_json::to_value(&manifest).map_err(RenderedRequestError::EncodeProvenance)?,
    );
    let provenance_payload_json = canonical_json(
        &serde_json::to_value(&assembly_trace)
            .map_err(RenderedRequestError::EncodeProvenancePayload)?,
    );
    let model_name = request_json
        .get("model")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| context.model_name.clone());

    Ok(RenderedCompletionRequest {
        capture_key,
        capture_version: CAPTURE_VERSION,
        request_doc_id: context.request_doc_id.clone(),
        request_commit_cid: context.request_commit_cid.clone(),
        request_id: context.request_id.clone(),
        capture_scope: capture_scope.to_string(),
        turn_index,
        attempt,
        node_did: context.node_did.clone(),
        requester_did: context.requester_did.clone(),
        agent_id: context.agent_id.clone(),
        session_id: context.session_id.clone(),
        model_name,
        source,
        request_json,
        messages_json,
        tools_json,
        tool_choice_json,
        sampling_json,
        assembly_trace,
        provenance_json,
        provenance_payload_json,
    })
}

/// Encode the unvalidated identity tuple as a JSON array so delimiters in IDs
/// cannot make distinct captures share a key.
pub fn capture_key(
    node_did: &str,
    session_id: &str,
    request_doc_id: &str,
    capture_scope: &str,
    turn_index: usize,
    attempt: u32,
) -> Result<String> {
    let tuple = json!([
        node_did,
        session_id,
        [request_doc_id, capture_scope],
        turn_index,
        attempt
    ]);
    let digest = sha256_canonical_json(&tuple)?;
    Ok(format!("{CAPTURE_KEY_PREFIX}:{digest}"))
}

/// Sort keys explicitly because workspace `serde_json/preserve_order` changes
/// map iteration order and therefore would otherwise change persisted hashes.
pub fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonical_json).collect()),
        Value::Object(map) => {
            let sorted = map
                .iter()
                .map(|(key, value)| (key.clone(), canonical_json(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect())
        }
        other => other.clone(),
    }
}

pub fn canonical_json_string(value: &Value) -> Result<String> {
    serde_json::to_string(&canonical_json(value)).map_err(RenderedRequestError::EncodeCanonicalJson)
}

pub fn sha256_canonical_json(value: &Value) -> Result<String> {
    let digest = Sha256::digest(canonical_json_string(value)?.as_bytes());
    Ok(format!("{digest:x}"))
}

#[cfg(test)]
#[path = "rendered_request_tests.rs"]
mod tests;
