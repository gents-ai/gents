//! The durable fact record for one provider call (#840), and the typed trace
//! that makes it explainable (#523).
//!
//! Two things live here and they are deliberately separate:
//!
//! * `RenderedCompletionRequest` is the *capture DTO*. It carries the exact
//!   rendered provider request plus the identity, routing, and provenance a
//!   `RenderedRequest` row needs. It is built once, immediately before
//!   `model.stream`, and handed to the capture sink.
//! * `AssemblyTrace` is the *leak set*. Prompt assembly reads durable
//!   documents, but four of its inputs are created in memory and never written
//!   anywhere a reconstructor could find them. Those four are enumerated on
//!   `AssemblyTrace` with the citation that proves each one is lost.
//!
//! ## Integrity is the field commit, not a column
//!
//! There is no `request_hash`. A stored digest is self-attested: the same code
//! that chooses the bytes also chooses the digest, so the two always agree and
//! an auditor learns nothing. `RenderedRequest` is `@branchable`, so DefraDB
//! writes a per-field commit block for `request_json` whose CID is computed
//! over the value actually stored. That CID is the content address, it
//! replicates with the document, and it is what a future Merkle-DAG proof can
//! attest over.
//!
//! `prompt_hash` and `tools_hash` survive only as query indexes — "find every
//! capture sharing this tool surface". Treating either as proof of content is a
//! bug.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::backend_provider::BackendProviderKind;
use crate::llm::message::{Message, ToolResultContent, UserContent};
use crate::openai_wire::OpenAiWireApi;

/// Capture format version stamped onto every row. Bump when the *set of
/// columns* a reader must understand changes.
pub const CAPTURE_VERSION: u32 = 1;

/// Provenance manifest version. Bump when `ProvenanceManifest`'s serialized
/// shape changes. A reader that does not know this number must report
/// `UnsupportedManifest` rather than guessing.
pub const PROVENANCE_MANIFEST_VERSION: u32 = 1;

/// Assembly-trace version. Bump when `AssemblyTrace`'s serialized shape
/// changes. Versioned independently of the manifest so a manifest that later
/// gains pinned config CIDs does not have to re-version the trace.
pub const ASSEMBLY_TRACE_VERSION: u32 = 1;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderedRequestSource {
    #[serde(rename = "openai_responses")]
    OpenAiResponses,
    #[serde(rename = "openai_chat_completions")]
    OpenAiChatCompletions,
}

impl RenderedRequestSource {
    pub(crate) fn for_behavior_provider(
        kind: BackendProviderKind,
        openai_wire_api: OpenAiWireApi,
    ) -> Self {
        match kind {
            BackendProviderKind::OpenAiCompatible => match openai_wire_api {
                OpenAiWireApi::Responses => Self::OpenAiResponses,
                OpenAiWireApi::ChatCompletions => Self::OpenAiChatCompletions,
            },
            BackendProviderKind::OpenRouter => Self::OpenAiChatCompletions,
            BackendProviderKind::ChatGptCodex => Self::OpenAiResponses,
            BackendProviderKind::XaiGrokOAuth => match openai_wire_api {
                OpenAiWireApi::Responses => Self::OpenAiResponses,
                OpenAiWireApi::ChatCompletions => Self::OpenAiChatCompletions,
            },
        }
    }
}

/// Which of the owned loop's two request builders produced the captured
/// `CompletionRequest`.
///
/// This is one of the four unrecoverable inputs. `build_budgeted_request`
/// applies `clamp_request_output_budget` before returning
/// (`agent/loop_stream.rs`), but the completion-retry `Repair` directive calls
/// `build_request` directly (`agent/loop_stream.rs:353,447`) and never clamps.
/// A repaired attempt therefore carries the raw configured `max_tokens` while
/// the original attempt for the same turn carries the clamped one. Without this
/// discriminator a reconstructor cannot tell which of the two it should
/// reproduce, and both are legal.
///
/// The clamp *value* is deliberately not stored: it is a pure function of the
/// assembled request plus durable config, and `completion_request_input_estimate`
/// does not read `max_tokens`, so a single reconstruction pass reproduces it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssemblyBuildPath {
    /// `build_budgeted_request`: ordinary assembly, output clamp applied, and
    /// per-turn compaction when the request exceeded the input budget.
    Budgeted,
    /// `build_request` invoked directly by a completion-retry repair. No output
    /// clamp is applied on this path.
    Repair,
}

impl AssemblyBuildPath {
    /// Whether the loop applied `clamp_request_output_budget` on this path.
    pub fn applies_output_clamp(self) -> bool {
        matches!(self, Self::Budgeted)
    }
}

/// A provider-assigned assistant message id, positioned in the effective
/// message list.
///
/// One of the four unrecoverable inputs. `close_streaming_turn` stamps the
/// provider's `MessageId` event onto the threaded assistant message
/// (`agent/loop_stream.rs:802-806`) because OpenAI Responses and ChatGPT Codex
/// follow-up requests reference prior `msg_` ids. The persistence path builds
/// its assistant message with `id: None`
/// (`agent/stream_processor.rs:305`), so the id exists in the provider request
/// and nowhere in the durable transcript.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantMessageId {
    /// Index into `AssemblyTrace::effective_messages`.
    pub message_index: usize,
    pub message_id: String,
}

/// The exact tool-result content threaded back into provider history for one
/// tool call.
///
/// One of the four unrecoverable inputs. The loop threads
/// `truncate_text(outcome.model_facing_text(), tool_result_truncation_mode(name),
/// &TruncationLimits::default())` (`agent/loop_stream.rs:655-658`). Persistence
/// re-derives its text from the stored `AgentToolCall.result` with
/// `TruncationMode::Head`, the hook's own `truncation_limits`, and
/// `model_observation_for_tool_result`
/// (`hook/persistence/message_spawn.rs:296-324`). Those are different functions
/// over different inputs, so replaying from the transcript does not reproduce
/// the bytes the model actually saw.
///
/// `content` is the full threaded `Vec<ToolResultContent>`, not a flattened
/// string: `ToolResultContent::from_tool_output` can split a JSON payload into
/// several parts, and that split is part of what the provider received.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThreadedToolResult {
    /// Index into `AssemblyTrace::effective_messages`.
    pub message_index: usize,
    /// `ToolResult.id` — rig's locally minted tool-call id.
    pub tool_call_id: String,
    /// `ToolResult.call_id` — the provider-side call id when one exists.
    pub call_id: Option<String>,
    pub content: Vec<ToolResultContent>,
}

/// The genuinely unrecoverable inputs to one rendered provider request.
///
/// Everything else that shapes a request is either durable (transcript rows,
/// behavior/profile/backend/skill documents) or a pure function of durable data.
/// These four are not:
///
/// 1. `assistant_message_ids` — provider-assigned, persisted as `None`.
/// 2. `threaded_tool_results` — the loop and the persistence path derive
///    different text from different sources.
/// 3. `effective_messages` — per-turn compaction is a *sticky* mutation
///    (`*history = compacted; *new_messages = vec![compacted_prompt]`,
///    `agent/loop_stream.rs:1274-1275`), so one turn's model-generated summary
///    governs every later turn of the same request, and that summary is never
///    written as an `AgentCompactionEntry`. Re-running the summarizer does not
///    produce the same words.
/// 4. `build_path` — see `AssemblyBuildPath`.
///
/// `assistant_message_ids` and `threaded_tool_results` are projections of
/// `effective_messages`, derived by the one constructor
/// (`AssemblyTrace::from_effective_messages`) so they cannot drift from it.
/// They are carried explicitly because a reconstructor rebuilds its message
/// list from `AgentMessage` rows and needs these as an *overlay* keyed by
/// position and call id; `effective_messages` is the oracle it checks itself
/// against.
///
/// ## Size
///
/// `effective_messages` is the conversation again, in native form, next to the
/// provider-wire copy in `request_json`. That is deliberate — the wire form is
/// not invertible (ChatGPT-Codex hoists system text into `instructions`, and
/// reasoning blocks, tool-call signatures, and `additional_params` do not
/// survive every conversion) — but it roughly doubles an already quadratic
/// per-turn payload. Compressing or content-addressing the fact record is the
/// known optimization; dropping the list is not, because nothing else records
/// a per-turn compaction summary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssemblyTrace {
    pub trace_version: u32,
    pub build_path: AssemblyBuildPath,
    /// The full effective provider message list at capture time — post
    /// sanitization, post request-context filtering, and post any per-turn
    /// compaction. Native `Message`s, not provider wire shapes: this is the
    /// *input* to assembly, whereas `request_json` is its output.
    pub effective_messages: Vec<Message>,
    pub assistant_message_ids: Vec<AssistantMessageId>,
    pub threaded_tool_results: Vec<ThreadedToolResult>,
}

impl AssemblyTrace {
    /// The only constructor that keeps the overlays consistent with
    /// `effective_messages`. Build traces with this, never with a struct
    /// literal.
    pub fn from_effective_messages(
        build_path: AssemblyBuildPath,
        effective_messages: Vec<Message>,
    ) -> Self {
        let mut assistant_message_ids = Vec::new();
        let mut threaded_tool_results = Vec::new();

        for (message_index, message) in effective_messages.iter().enumerate() {
            match message {
                Message::Assistant {
                    id: Some(message_id),
                    ..
                } => assistant_message_ids.push(AssistantMessageId {
                    message_index,
                    message_id: message_id.clone(),
                }),
                Message::User { content } => {
                    for item in content {
                        if let UserContent::ToolResult(result) = item {
                            threaded_tool_results.push(ThreadedToolResult {
                                message_index,
                                tool_call_id: result.id.clone(),
                                call_id: result.call_id.clone(),
                                content: result.content.clone(),
                            });
                        }
                    }
                }
                _ => {}
            }
        }

        Self {
            trace_version: ASSEMBLY_TRACE_VERSION,
            build_path,
            effective_messages,
            assistant_message_ids,
            threaded_tool_results,
        }
    }
}

/// How much this capture claims about reconstructibility.
///
/// Explicit, never inferred from an absent field. A manifest that simply omits
/// pinned CIDs must not read as "nothing needed pinning".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceStatus {
    /// The rendered request is durable and exact, but no durable source
    /// versions are pinned alongside it, so a reconstruction cannot be
    /// verified against it. This is the only status version 1 emits.
    CapturedOnly,
}

/// Versioned provenance travelling in the `provenance_json` column.
///
/// Version 1 carries the assembly trace and an honest `CapturedOnly` status.
/// Pinned config/transcript CIDs are a later version; when they arrive, a
/// version-1 row must still be readable and must still report `CapturedOnly`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceManifest {
    pub manifest_version: u32,
    pub status: ProvenanceStatus,
    /// Why this row is not `Verified`, in words, so a projection can say so
    /// without the reader reverse-engineering it from missing fields.
    pub status_reason: String,
    pub assembly_trace: AssemblyTrace,
}

impl ProvenanceManifest {
    const CAPTURED_ONLY_REASON: &'static str =
        "provenance manifest v1 pins no config or transcript versions, so a \
         reconstruction cannot be verified against this capture";

    pub fn captured_only(assembly_trace: AssemblyTrace) -> Self {
        Self {
            manifest_version: PROVENANCE_MANIFEST_VERSION,
            status: ProvenanceStatus::CapturedOnly,
            status_reason: Self::CAPTURED_ONLY_REASON.to_string(),
            assembly_trace,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderedRequestContext {
    pub request_id: String,
    pub agent_did: String,
    /// The requesting principal. Empty when the request has none — an empty DID
    /// is never a participant, so downstream authorization must treat `""` as
    /// "owner only" rather than as a DID.
    pub requester_did: String,
    pub behavior_id: String,
    pub session_id: String,
    pub model_name: String,
    pub source: RenderedRequestSource,
    pub normalize_responses_wire: bool,
}

impl RenderedRequestContext {
    pub(crate) fn for_request(
        request: &crate::watcher::AgentRequest,
        model_name: String,
        source: RenderedRequestSource,
        normalize_responses_wire: bool,
    ) -> Self {
        Self {
            request_id: request.request_id.clone(),
            agent_did: request.agent_did.clone(),
            requester_did: request.requester_did.clone().unwrap_or_default(),
            behavior_id: request.behavior_id.clone().unwrap_or_default(),
            session_id: request.session_id.clone(),
            model_name,
            source,
            normalize_responses_wire,
        }
    }
}

/// The JSON pieces `llm::rig_compat` extracts from one rendered provider
/// request. Grouped so the DTO builder keeps a readable arity.
pub(crate) struct RenderedRequestComponents {
    /// The complete provider request. This is the fact record; the four fields
    /// below are query conveniences derived from it.
    pub(crate) request_json: Value,
    pub(crate) messages_json: Value,
    pub(crate) tools_json: Value,
    pub(crate) tool_choice_json: Value,
    pub(crate) sampling_json: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderedCompletionRequest {
    /// `capture_key(agent_did, session_id, request_id, turn_index, attempt)`.
    /// The unique index on the durable row and the idempotency key of the sink.
    pub capture_key: String,
    pub capture_version: u32,
    pub request_id: String,
    pub turn_index: usize,
    pub attempt: u32,
    pub agent_did: String,
    /// Empty when the request carried no requester DID.
    pub requester_did: String,
    pub behavior_id: String,
    pub session_id: String,
    pub model_name: String,
    pub source: RenderedRequestSource,
    /// The complete rendered provider request. Retained in full: component
    /// hashes are indexes, not a substitute for the payload.
    pub request_json: Value,
    pub messages_json: Value,
    pub tools_json: Value,
    pub tool_choice_json: Value,
    pub sampling_json: Value,
    /// Query index over `messages_json`. Not an integrity mechanism.
    pub prompt_hash: String,
    /// Query index over `tools_json`. Not an integrity mechanism.
    pub tools_hash: String,
    pub assembly_trace: AssemblyTrace,
    /// Canonical JSON of the `ProvenanceManifest` built from `assembly_trace`.
    /// Derived by the builder so the column and the typed value cannot
    /// disagree; a reader may deserialize it back into `ProvenanceManifest`.
    pub provenance_json: Value,
}

pub(crate) fn build_rendered_completion_request(
    context: &RenderedRequestContext,
    turn_index: usize,
    attempt: u32,
    assembly_trace: AssemblyTrace,
    components: RenderedRequestComponents,
) -> Result<RenderedCompletionRequest> {
    let RenderedRequestComponents {
        request_json,
        messages_json,
        tools_json,
        tool_choice_json,
        sampling_json,
    } = components;

    let prompt_hash = sha256_canonical_json(&messages_json)?;
    let tools_hash = sha256_canonical_json(&tools_json)?;
    let capture_key = capture_key(
        &context.agent_did,
        &context.session_id,
        &context.request_id,
        turn_index,
        attempt,
    )?;
    let manifest = ProvenanceManifest::captured_only(assembly_trace.clone());
    let provenance_json = canonical_json(
        &serde_json::to_value(&manifest).context("encoding rendered-request provenance")?,
    );

    Ok(RenderedCompletionRequest {
        capture_key,
        capture_version: CAPTURE_VERSION,
        request_id: context.request_id.clone(),
        turn_index,
        attempt,
        agent_did: context.agent_did.clone(),
        requester_did: context.requester_did.clone(),
        behavior_id: context.behavior_id.clone(),
        session_id: context.session_id.clone(),
        model_name: context.model_name.clone(),
        source: context.source,
        request_json,
        messages_json,
        tools_json,
        tool_choice_json,
        sampling_json,
        prompt_hash,
        tools_hash,
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
/// `agent_did` and `session_id` are load-bearing, not decoration:
/// `AgentRequest.request_id` is `@index` but **not** `@index(unique: true)`, so
/// it is not globally unique on its own.
pub fn capture_key(
    agent_did: &str,
    session_id: &str,
    request_id: &str,
    turn_index: usize,
    attempt: u32,
) -> Result<String> {
    let tuple = json!([agent_did, session_id, request_id, turn_index, attempt]);
    let digest = sha256_canonical_json(&tuple)?;
    Ok(format!("{CAPTURE_KEY_PREFIX}:{digest}"))
}

pub(crate) fn sampling_json(
    temperature: Option<f64>,
    max_tokens: Option<u64>,
    additional_params: Option<Value>,
) -> Value {
    json!({
        "temperature": temperature,
        "max_tokens": max_tokens,
        "additional_params": additional_params.unwrap_or(Value::Null),
    })
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
pub(crate) fn canonical_json(value: &Value) -> Value {
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
pub(crate) fn canonical_json_string(value: &Value) -> Result<String> {
    serde_json::to_string(&canonical_json(value)).context("encoding canonical JSON")
}

/// SHA-256 of `canonical_json_string`, lowercase hex.
pub(crate) fn sha256_canonical_json(value: &Value) -> Result<String> {
    let digest = Sha256::digest(canonical_json_string(value)?.as_bytes());
    Ok(format!("{digest:x}"))
}

#[cfg(test)]
mod tests {
    use gents_protocol::message::{AssistantContent, Text, ToolCall, ToolFunction};
    use serde_json::json;

    use super::*;

    fn context() -> RenderedRequestContext {
        RenderedRequestContext {
            request_id: "req-1".to_string(),
            agent_did: "did:key:test".to_string(),
            requester_did: "did:key:requester".to_string(),
            behavior_id: "behavior".to_string(),
            session_id: "session".to_string(),
            model_name: "test-model".to_string(),
            source: RenderedRequestSource::OpenAiChatCompletions,
            normalize_responses_wire: false,
        }
    }

    fn components() -> RenderedRequestComponents {
        RenderedRequestComponents {
            request_json: json!({"messages": [{"role": "user", "content": "hi"}]}),
            messages_json: json!([{"role": "user", "content": "hi"}]),
            tools_json: json!([{"type": "function", "function": {"name": "read_file"}}]),
            tool_choice_json: Value::Null,
            sampling_json: sampling_json(
                Some(0.2),
                Some(512),
                Some(json!({"reasoning": {"effort": "medium"}})),
            ),
        }
    }

    fn empty_trace() -> AssemblyTrace {
        AssemblyTrace::from_effective_messages(AssemblyBuildPath::Budgeted, Vec::new())
    }

    fn tool_result_message(id: &str, call_id: Option<&str>, text: &str) -> Message {
        Message::User {
            content: vec![match call_id {
                Some(call_id) => UserContent::tool_result_with_call_id(
                    id,
                    call_id.to_string(),
                    vec![ToolResultContent::text(text)],
                ),
                None => UserContent::tool_result(id, vec![ToolResultContent::text(text)]),
            }],
        }
    }

    fn assistant_with_tool_call(id: Option<&str>, tool_call_id: &str) -> Message {
        Message::Assistant {
            id: id.map(str::to_string),
            content: vec![AssistantContent::ToolCall(ToolCall::new(
                tool_call_id.to_string(),
                ToolFunction {
                    name: "read_file".to_string(),
                    arguments: json!({"path": "a.txt"}),
                },
            ))],
        }
    }

    #[test]
    fn canonical_hash_sorts_object_keys() {
        let left = json!({ "b": 1, "a": { "d": 2, "c": 3 } });
        let right = json!({ "a": { "c": 3, "d": 2 }, "b": 1 });

        assert_eq!(
            sha256_canonical_json(&left).unwrap(),
            sha256_canonical_json(&right).unwrap()
        );
    }

    /// The persisted bytes and the digest must come from the same encoder. If a
    /// second serialization ever creeps into the sink, this is the test that
    /// notices the digest no longer describes the stored string.
    #[test]
    fn component_hashes_digest_exactly_the_canonical_bytes() {
        let value = json!({ "b": [3, {"z": 1, "a": 2}], "a": "x" });

        let bytes = canonical_json_string(&value).unwrap();
        assert_eq!(bytes, r#"{"a":"x","b":[3,{"a":2,"z":1}]}"#);

        let digest = Sha256::digest(bytes.as_bytes());
        assert_eq!(
            sha256_canonical_json(&value).unwrap(),
            format!("{digest:x}")
        );
    }

    #[test]
    fn canonical_json_sorts_nested_arrays_of_objects() {
        let value = json!([{ "b": 1, "a": 2 }, { "d": 3, "c": 4 }]);
        assert_eq!(
            canonical_json_string(&value).unwrap(),
            r#"[{"a":2,"b":1},{"c":4,"d":3}]"#
        );
    }

    /// Reconstruction compares the *parsed* persisted string against a freshly
    /// rendered request. That comparison is only meaningful if canonicalization
    /// reorders keys and changes nothing else — array order, numeric form,
    /// nulls, and escapes all have to survive the round trip.
    #[test]
    fn canonical_bytes_parse_back_to_the_same_value() {
        let value = json!({
            "model": "test-model",
            "messages": [
                { "role": "system", "content": "b\u{0007}\"quoted\"\n" },
                { "role": "user", "content": null },
            ],
            "tools": [],
            "temperature": 0.2,
            "max_tokens": 512,
            "nested": { "z": [1, 2, 3], "a": { "deep": true } },
        });

        let parsed: Value = serde_json::from_str(&canonical_json_string(&value).unwrap()).unwrap();
        assert_eq!(parsed, value);
        assert_eq!(parsed, canonical_json(&value));
    }

    /// Absent tools and tool choice must still hash — an empty list is a real
    /// tool surface, not a missing one, and it has to be findable by index.
    #[test]
    fn empty_tools_and_tool_choice_still_produce_component_hashes() {
        let rendered = build_rendered_completion_request(
            &context(),
            0,
            0,
            empty_trace(),
            RenderedRequestComponents {
                request_json: json!({"model": "test-model", "tools": []}),
                messages_json: json!([]),
                tools_json: json!([]),
                tool_choice_json: Value::Null,
                sampling_json: sampling_json(None, None, None),
            },
        )
        .expect("rendered request");

        assert_eq!(
            rendered.prompt_hash,
            sha256_canonical_json(&json!([])).unwrap()
        );
        assert_eq!(
            rendered.tools_hash,
            sha256_canonical_json(&json!([])).unwrap()
        );
        assert_eq!(rendered.tool_choice_json, Value::Null);
        assert_eq!(rendered.sampling_json["temperature"], Value::Null);
    }

    #[test]
    fn grok_rendered_source_follows_effective_wire_api() {
        assert_eq!(
            RenderedRequestSource::for_behavior_provider(
                BackendProviderKind::XaiGrokOAuth,
                OpenAiWireApi::Responses,
            ),
            RenderedRequestSource::OpenAiResponses
        );
        assert_eq!(
            RenderedRequestSource::for_behavior_provider(
                BackendProviderKind::XaiGrokOAuth,
                OpenAiWireApi::ChatCompletions,
            ),
            RenderedRequestSource::OpenAiChatCompletions
        );
    }

    #[test]
    fn rendered_source_uses_effective_backend_wire_api() {
        assert_eq!(
            RenderedRequestSource::for_behavior_provider(
                BackendProviderKind::OpenAiCompatible,
                OpenAiWireApi::Responses,
            ),
            RenderedRequestSource::OpenAiResponses
        );
        assert_eq!(
            RenderedRequestSource::for_behavior_provider(
                BackendProviderKind::OpenAiCompatible,
                OpenAiWireApi::ChatCompletions,
            ),
            RenderedRequestSource::OpenAiChatCompletions
        );
        assert_eq!(
            RenderedRequestSource::for_behavior_provider(
                BackendProviderKind::OpenRouter,
                OpenAiWireApi::Responses,
            ),
            RenderedRequestSource::OpenAiChatCompletions
        );
        assert_eq!(
            RenderedRequestSource::for_behavior_provider(
                BackendProviderKind::ChatGptCodex,
                OpenAiWireApi::ChatCompletions,
            ),
            RenderedRequestSource::OpenAiResponses
        );
    }

    #[test]
    fn rendered_completion_request_hashes_prompt_and_tools() {
        let rendered =
            build_rendered_completion_request(&context(), 0, 0, empty_trace(), components())
                .expect("rendered request");

        assert_eq!(rendered.request_id, "req-1");
        assert_eq!(rendered.turn_index, 0);
        assert_eq!(rendered.attempt, 0);
        assert_eq!(rendered.requester_did, "did:key:requester");
        assert_eq!(rendered.capture_version, CAPTURE_VERSION);
        assert_eq!(
            rendered.source,
            RenderedRequestSource::OpenAiChatCompletions
        );
        assert_eq!(rendered.messages_json[0]["role"], "user");
        assert_eq!(rendered.tools_json[0]["function"]["name"], "read_file");
        assert_eq!(rendered.sampling_json["temperature"], 0.2);
        assert_eq!(rendered.sampling_json["max_tokens"], 512);
        assert_eq!(rendered.prompt_hash.len(), 64);
        assert_eq!(rendered.tools_hash.len(), 64);
    }

    /// The payload survives capture. Component hashes are indexes; they never
    /// replace `request_json`.
    #[test]
    fn rendered_completion_request_retains_the_full_payload() {
        let rendered =
            build_rendered_completion_request(&context(), 0, 0, empty_trace(), components())
                .expect("rendered request");

        assert_eq!(
            rendered.request_json,
            json!({"messages": [{"role": "user", "content": "hi"}]})
        );
    }

    #[test]
    fn capture_key_is_stable_and_prefixed() {
        let key = capture_key("did:key:a", "session-1", "request-1", 3, 2).unwrap();
        assert!(key.starts_with("rendered:v1:"), "unexpected key {key}");
        assert_eq!(key.len(), "rendered:v1:".len() + 64);
        assert_eq!(
            key,
            capture_key("did:key:a", "session-1", "request-1", 3, 2).unwrap()
        );
    }

    /// Every one of the five components has to move the key. A dropped
    /// component silently merges two provider attempts into one durable fact,
    /// which is precisely what `capture_key_determines_request` forbids.
    #[test]
    fn every_capture_key_component_changes_the_key() {
        let base = capture_key("did:key:a", "session-1", "request-1", 0, 0).unwrap();

        for varied in [
            capture_key("did:key:b", "session-1", "request-1", 0, 0).unwrap(),
            capture_key("did:key:a", "session-2", "request-1", 0, 0).unwrap(),
            capture_key("did:key:a", "session-1", "request-2", 0, 0).unwrap(),
            capture_key("did:key:a", "session-1", "request-1", 1, 0).unwrap(),
            capture_key("did:key:a", "session-1", "request-1", 0, 1).unwrap(),
        ] {
            assert_ne!(base, varied);
        }
    }

    /// A delimited `"{a}:{b}"` encoding would make these one fact. `session_id`
    /// is caller-supplied and unvalidated, so the encoding — not a convention —
    /// has to rule the collision out.
    #[test]
    fn capture_key_does_not_collide_across_component_boundaries() {
        assert_ne!(
            capture_key("did:key:a", "s:1", "request-1", 0, 0).unwrap(),
            capture_key("did:key:a:s", "1", "request-1", 0, 0).unwrap(),
        );
        assert_ne!(
            capture_key("did:key:a", "session", "r:1", 0, 0).unwrap(),
            capture_key("did:key:a", "session:r", "1", 0, 0).unwrap(),
        );
        // `request_id` is `@index` but not unique, so the same id under two
        // sessions must stay two facts.
        assert_ne!(
            capture_key("did:key:a", "session-1", "shared", 0, 0).unwrap(),
            capture_key("did:key:a", "session-2", "shared", 0, 0).unwrap(),
        );
    }

    #[test]
    fn assembly_trace_records_assistant_message_ids_by_position() {
        let trace = AssemblyTrace::from_effective_messages(
            AssemblyBuildPath::Budgeted,
            vec![
                Message::user("hi"),
                assistant_with_tool_call(Some("msg_abc"), "call-1"),
                tool_result_message("call-1", Some("fc_1"), "ok"),
                // An assistant turn the provider gave no id for.
                Message::assistant("done"),
            ],
        );

        assert_eq!(
            trace.assistant_message_ids,
            vec![AssistantMessageId {
                message_index: 1,
                message_id: "msg_abc".to_string(),
            }]
        );
    }

    #[test]
    fn assembly_trace_records_threaded_tool_results_by_call_identity() {
        let trace = AssemblyTrace::from_effective_messages(
            AssemblyBuildPath::Budgeted,
            vec![
                assistant_with_tool_call(Some("msg_abc"), "call-1"),
                tool_result_message("call-1", Some("fc_1"), "threaded bytes"),
                tool_result_message("call-2", None, "no provider call id"),
            ],
        );

        assert_eq!(
            trace.threaded_tool_results,
            vec![
                ThreadedToolResult {
                    message_index: 1,
                    tool_call_id: "call-1".to_string(),
                    call_id: Some("fc_1".to_string()),
                    content: vec![ToolResultContent::Text(Text {
                        text: "threaded bytes".to_string()
                    })],
                },
                ThreadedToolResult {
                    message_index: 2,
                    tool_call_id: "call-2".to_string(),
                    call_id: None,
                    content: vec![ToolResultContent::Text(Text {
                        text: "no provider call id".to_string()
                    })],
                },
            ]
        );
    }

    /// The overlays index into `effective_messages`; a projection that reads
    /// them has to land back on the message it came from.
    #[test]
    fn assembly_trace_overlay_indexes_address_the_effective_messages() {
        let messages = vec![
            Message::user("hi"),
            assistant_with_tool_call(Some("msg_abc"), "call-1"),
            tool_result_message("call-1", Some("fc_1"), "threaded bytes"),
        ];
        let trace =
            AssemblyTrace::from_effective_messages(AssemblyBuildPath::Budgeted, messages.clone());

        assert_eq!(trace.effective_messages, messages);
        for overlay in &trace.assistant_message_ids {
            assert!(matches!(
                &trace.effective_messages[overlay.message_index],
                Message::Assistant { id: Some(id), .. } if *id == overlay.message_id
            ));
        }
        for overlay in &trace.threaded_tool_results {
            assert!(matches!(
                &trace.effective_messages[overlay.message_index],
                Message::User { .. }
            ));
        }
    }

    /// Per-turn compaction rewrites `history` and `new_messages` in place, so
    /// the trace has to describe the *post*-compaction list. Nothing else
    /// records it: the summary is model-generated and never becomes an
    /// `AgentCompactionEntry`.
    #[test]
    fn assembly_trace_carries_the_post_compaction_message_list() {
        let compacted = vec![
            Message::system("<system-reminder>continuation checkpoint</system-reminder>"),
            Message::user("continue"),
        ];
        let trace =
            AssemblyTrace::from_effective_messages(AssemblyBuildPath::Budgeted, compacted.clone());

        assert_eq!(trace.effective_messages, compacted);
        assert_eq!(trace.trace_version, ASSEMBLY_TRACE_VERSION);
    }

    #[test]
    fn build_path_records_whether_the_output_clamp_ran() {
        assert!(AssemblyBuildPath::Budgeted.applies_output_clamp());
        assert!(!AssemblyBuildPath::Repair.applies_output_clamp());

        let budgeted =
            build_rendered_completion_request(&context(), 0, 0, empty_trace(), components())
                .expect("budgeted");
        let repaired = build_rendered_completion_request(
            &context(),
            0,
            1,
            AssemblyTrace::from_effective_messages(AssemblyBuildPath::Repair, Vec::new()),
            components(),
        )
        .expect("repaired");

        assert_eq!(
            budgeted.assembly_trace.build_path,
            AssemblyBuildPath::Budgeted
        );
        assert_eq!(
            repaired.assembly_trace.build_path,
            AssemblyBuildPath::Repair
        );
        assert_ne!(budgeted.provenance_json, repaired.provenance_json);
    }

    #[test]
    fn provenance_json_round_trips_to_a_captured_only_manifest() {
        let trace = AssemblyTrace::from_effective_messages(
            AssemblyBuildPath::Repair,
            vec![
                assistant_with_tool_call(Some("msg_abc"), "call-1"),
                tool_result_message("call-1", Some("fc_1"), "threaded bytes"),
            ],
        );
        let rendered =
            build_rendered_completion_request(&context(), 2, 1, trace.clone(), components())
                .expect("rendered request");

        let manifest: ProvenanceManifest =
            serde_json::from_value(rendered.provenance_json.clone()).expect("manifest round-trip");

        assert_eq!(manifest.manifest_version, PROVENANCE_MANIFEST_VERSION);
        assert_eq!(manifest.status, ProvenanceStatus::CapturedOnly);
        assert!(!manifest.status_reason.is_empty());
        assert_eq!(manifest.assembly_trace, trace);
        assert_eq!(rendered.assembly_trace, trace);
    }

    /// `provenance_json` is what lands in the column, so its key order must not
    /// depend on which workspace members turned on `serde_json/preserve_order`.
    #[test]
    fn provenance_json_is_canonically_ordered() {
        let rendered =
            build_rendered_completion_request(&context(), 0, 0, empty_trace(), components())
                .expect("rendered request");

        assert_eq!(
            canonical_json(&rendered.provenance_json),
            rendered.provenance_json
        );
    }

    /// Version 1 declares `captured_only` positively. An absent field is never
    /// the evidence — a reader must be able to see the claim, not infer it.
    #[test]
    fn version_one_provenance_never_claims_verification() {
        let rendered =
            build_rendered_completion_request(&context(), 0, 0, empty_trace(), components())
                .expect("rendered request");

        assert_eq!(rendered.provenance_json["status"], "captured_only");
        assert_eq!(
            rendered.provenance_json["manifest_version"],
            PROVENANCE_MANIFEST_VERSION
        );
        assert!(rendered.provenance_json.get("assembly_trace").is_some());
    }

    fn agent_request() -> crate::watcher::AgentRequest {
        crate::watcher::AgentRequest {
            doc_id: "doc-1".to_string(),
            request_id: "request-1".to_string(),
            agent_did: "did:key:test".to_string(),
            requester_did: None,
            behavior_id: Some("behavior".to_string()),
            session_id: "session".to_string(),
            content: "hi".to_string(),
            temperature: None,
            top_p: None,
            top_k: None,
            max_tokens: None,
            metadata: None,
            execution_origin: None,
            created_at: String::new(),
            deadline: None,
            subagent_depth: 0,
            caused_by_parent_request_id: None,
            caused_by_parent_tool_call_id: None,
        }
    }

    #[test]
    fn context_for_request_carries_an_absent_requester_as_empty() {
        let mut request = agent_request();
        request.requester_did = None;
        let context = RenderedRequestContext::for_request(
            &request,
            "test-model".to_string(),
            RenderedRequestSource::OpenAiChatCompletions,
            false,
        );
        assert_eq!(context.requester_did, "");

        request.requester_did = Some("did:key:requester".to_string());
        let context = RenderedRequestContext::for_request(
            &request,
            "test-model".to_string(),
            RenderedRequestSource::OpenAiChatCompletions,
            false,
        );
        assert_eq!(context.requester_did, "did:key:requester");
    }
}
