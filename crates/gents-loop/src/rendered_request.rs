//! The durable fact record for one provider call (#840), and the typed trace
//! that makes it explainable (#523).
//!
//! `gents_protocol::rendered_request` carries the persisted vocabulary
//! (`AssemblyTrace`, `ContextAccounting`, `CaptureScopeKind`, ...), shared
//! with every peer that reads a `RenderedRequest` row. This module carries
//! the rest of the capture *mechanism*: the per-request arm/claim scope
//! (`scope`) the loop consults before it lets a completion body reach the
//! network, the capturing transport wrapper (`transport`) installed as the
//! innermost `HttpClientExt` layer, and the pure DTO-building step
//! (`build_rendered_completion_request`) that turns a captured body into the
//! durable record. `gents`'s own `rendered_request` module keeps the
//! DefraDB-backed sink and the admission-controller provenance lookup
//! (`AdmissionJoinLookup`, installed onto `RequestCaptureScope`), since
//! neither belongs in a guest.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub mod scope;
pub mod transport;

pub use gents_protocol::rendered_request::{
    AdmissionJoin, AssemblyBuildPath, AssemblyTrace, AssistantMessageId, CaptureOrderKey,
    CaptureScope, CaptureScopeKind, CaptureSeam, ContextAccounting, ContextCompactionReason,
    ContextInputComponents, ParsedProvenance, ProvenanceManifest, ProvenanceStatus,
    RenderedRequestSource, ThreadedToolResult, ASSEMBLY_TRACE_VERSION, CAPTURE_VERSION,
    CONTEXT_ACCOUNTING_VERSION, PROVENANCE_MANIFEST_VERSION,
};
pub use transport::RenderedRequestCapturingHttpClient;

/// Prefix on every capture key. Bound to the *key derivation*, not to
/// `CAPTURE_VERSION`: adding a column must not silently re-key existing facts.
const CAPTURE_KEY_PREFIX: &str = "rendered:v1";

pub type RenderedRequestCaptureSink = Arc<
    dyn Fn(RenderedCompletionRequest) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>
        + Send
        + Sync,
>;

pub type RenderedRequestCaptureFactory =
    Arc<dyn Fn(RenderedRequestContext) -> RenderedRequestCaptureSink + Send + Sync>;

/// Identity and routing for every capture belonging to one request.
///
/// Deliberately carries no provider-wire information: which wire shape was used
/// and which model the body named are read back off the captured body itself,
/// because configuration describes intent and this record describes what
/// happened.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderedRequestContext {
    /// Exact DefraDB document identity for the request being served. Unlike the
    /// logical `request_id`, this identifies one signed document even if an
    /// invalid duplicate logical id exists in the collection. Empty only for a
    /// one-shot run, which does not author an `AgentRequest` document.
    pub request_doc_id: String,
    /// Exact composite DefraDB commit returned by the mutation that claimed
    /// this request. Empty only for a one-shot run.
    pub request_commit_cid: String,
    pub request_id: String,
    pub agent_did: String,
    /// The requesting principal. Empty when the request has none — an empty DID
    /// is never a participant, so downstream authorization must treat `""` as
    /// "owner only" rather than as a DID.
    pub requester_did: String,
    pub behavior_id: String,
    pub session_id: String,
    /// The behavior's configured model. Used only when the captured body names
    /// no `model`.
    pub model_name: String,
}

/// The JSON pieces extracted from one captured provider body. Grouped so the
/// DTO builder keeps a readable arity.
pub struct RenderedRequestComponents {
    /// The complete provider request. This is the fact record; the fields below
    /// are typed views used while validating transport conversion.
    pub request_json: Value,
    pub messages_json: Value,
    pub tools_json: Value,
    pub tool_choice_json: Value,
    pub sampling_json: Value,
}

impl RenderedRequestComponents {
    /// Split a captured provider body into the payload plus derived views.
    ///
    /// Every view is read out of the body, never re-derived from the assembled
    /// `CompletionRequest`: on the Codex path the assembled request still
    /// carries `temperature`/`max_output_tokens` that the transport deleted,
    /// and a row that reported those would be describing a request nobody sent.
    pub fn from_provider_body(request_json: Value, source: RenderedRequestSource) -> Self {
        let field = |name: &str| request_json.get(name).cloned();
        let messages_json =
            field(source.messages_field()).unwrap_or_else(|| Value::Array(Vec::new()));
        let tools_json = field("tools").unwrap_or_else(|| Value::Array(Vec::new()));
        let tool_choice_json = field("tool_choice").unwrap_or(Value::Null);
        let sampling_json = json!({
            "temperature": field("temperature").unwrap_or(Value::Null),
            "top_p": field("top_p").unwrap_or(Value::Null),
            // Chat Completions calls it `max_tokens`; Responses calls it
            // `max_output_tokens`. Codex deletes both, and `null` here is the
            // honest report of that.
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
    /// `capture_key(agent_did, session_id, request_doc_id, capture_scope,
    /// turn_index, attempt)`. The unique index on the durable row and the
    /// idempotency key of the sink.
    pub capture_key: String,
    pub capture_version: u32,
    /// Exact `_docID` of the durable `AgentRequest`. The logical `request_id`
    /// remains alongside it for user-facing correlation and queries. Empty for
    /// a one-shot run, which has no `AgentRequest` document.
    pub request_doc_id: String,
    /// Exact composite version of `request_doc_id` that supplied the runtime
    /// input. The CID is DefraDB's native time-travel and integrity reference.
    pub request_commit_cid: String,
    pub request_id: String,
    /// Which completion loop inside the request issued this call, e.g.
    /// `inference.1` or `compaction.2`. See [`CaptureScopeKind`].
    pub capture_scope: String,
    pub turn_index: usize,
    pub attempt: u32,
    pub agent_did: String,
    /// Empty when the request carried no requester DID.
    pub requester_did: String,
    pub behavior_id: String,
    pub session_id: String,
    pub model_name: String,
    pub source: RenderedRequestSource,
    /// The complete rendered provider request retained as the durable payload.
    pub request_json: Value,
    pub messages_json: Value,
    pub tools_json: Value,
    pub tool_choice_json: Value,
    pub sampling_json: Value,
    pub assembly_trace: AssemblyTrace,
    /// Canonical JSON of the `ProvenanceManifest` built from `assembly_trace`.
    /// Derived by the builder so the column and the typed value cannot
    /// disagree; a reader may deserialize it back into `ProvenanceManifest`.
    pub provenance_json: Value,
}

/// Build the durable capture record from a captured body's components.
///
/// `admission_join` is the admission-controller provenance stamp for this
/// call, if any - computed by the caller (native code reads a live
/// task-local; the loop and tests pass `None`) rather than looked up here, so
/// this function stays free of the admission-controller dependency.
#[allow(clippy::too_many_arguments)]
pub fn build_rendered_completion_request(
    context: &RenderedRequestContext,
    capture_scope: &str,
    source: RenderedRequestSource,
    provider_endpoint: Option<String>,
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
        &context.agent_did,
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
    );
    let provenance_json = canonical_json(
        &serde_json::to_value(&manifest).context("encoding rendered-request provenance")?,
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
        agent_did: context.agent_did.clone(),
        requester_did: context.requester_did.clone(),
        behavior_id: context.behavior_id.clone(),
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
    })
}

/// Derive the durable capture key from the five-component identity tuple.
///
/// The tuple is exactly the one `Proofs/RenderedCapture.lean` quantifies over
/// with componentwise equality, and it is encoded as a canonical JSON *array* —
/// never as a delimited concatenation. JSON string escaping keeps the encoding
/// injective, so no component value can be chosen to forge another tuple's key.
/// That matters concretely: `session_id` is caller-controlled and unvalidated
/// (`ChatArgs::session_id` has no `value_parser`), and a `"{a}:{b}"` format
/// would let `("x:y", "z")` and `("x", "y:z")` collide into one fact.
///
/// `request_doc_id` is the DefraDB `_docID` of the durable `AgentRequest`, not its
/// user-facing `request_id`. The latter is an indexed logical correlation id,
/// but the document id is the provenance edge that identifies the exact request
/// fact this capture belongs to.
///
/// ## Why the third component is a pair
///
/// The Lean model's `requestId` names *the provider-call scope inside a
/// request*, and one request runs more than one completion loop: the owned
/// inference loop, the per-turn compaction summarizer (guided, plus a JSON
/// fallback), and conversation title generation. Each of those is a separate
/// `run_loop_stream` whose turn and attempt counters start at zero, so
/// `(request_id, 0, 0)` would name several different provider calls. The scope
/// therefore rides *inside* the third component as the nested JSON array
/// `[request_doc_id, capture_scope]`, which keeps the tuple five components wide
/// and componentwise-injective — a `"{request_doc_id}#{scope}"` string would
/// reintroduce exactly the delimiter collision the array encoding exists to
/// rule out.
pub fn capture_key(
    agent_did: &str,
    session_id: &str,
    request_doc_id: &str,
    capture_scope: &str,
    turn_index: usize,
    attempt: u32,
) -> Result<String> {
    let tuple = json!([
        agent_did,
        session_id,
        [request_doc_id, capture_scope],
        turn_index,
        attempt
    ]);
    let digest = sha256_canonical_json(&tuple)?;
    Ok(format!("{CAPTURE_KEY_PREFIX}:{digest}"))
}

/// The one canonical JSON encoder. Persisted bytes, component hashes, and the
/// capture key all go through it; there is deliberately no second
/// implementation in the sink or the reconstructor.
///
/// Key order is not free: `serde_json::Map` becomes an insertion-ordered
/// `IndexMap` for the whole build whenever any crate in the graph enables
/// `serde_json/preserve_order` — `schemars` does, via `tauri-build`. Without an
/// imposed order the "same" request would hash differently depending on which
/// workspace members were compiled.
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

/// The exact UTF-8 bytes to persist for a canonical JSON column, and the exact
/// bytes `sha256_canonical_json` digests.
pub fn canonical_json_string(value: &Value) -> Result<String> {
    serde_json::to_string(&canonical_json(value)).context("encoding canonical JSON")
}

/// SHA-256 of `canonical_json_string`, lowercase hex.
pub fn sha256_canonical_json(value: &Value) -> Result<String> {
    let digest = Sha256::digest(canonical_json_string(value)?.as_bytes());
    Ok(format!("{digest:x}"))
}

#[cfg(test)]
#[path = "rendered_request_tests.rs"]
mod tests;
