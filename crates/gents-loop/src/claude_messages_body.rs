//! Anthropic Messages request-body assembly, moved out of `gents::claude_messages`
//! (G-1): `provider_input` projects this exact body for token accounting, and
//! the real transport (native, in `gents`) builds the identical body to send.
//! The SSE response parser and the OAuth-bearing HTTP client stay native.

use gents_protocol::message::{
    AssistantContent, Message, ReasoningContent, ToolResultContent, UserContent,
};
use gents_protocol::output::OutputSource;
use rig::completion::{CompletionRequest, ToolDefinition};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const DEFAULT_MAX_TOKENS: u64 = 4096;
#[doc(hidden)]
pub const ADVERTISED_REASONING_EFFORTS_PARAM: &str = "_gents_advertised_reasoning_efforts";

/// First `system` block. The subscription token was minted for Claude Code;
/// without this identity the same token 429s on every model (write request #7).
/// Lean: `ClaudeMap.identity`.
pub const CLAUDE_CODE_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

/// Result of the owning runtime's provider-turn/capture join, never inferred
/// from an opaque signature or the buffer currently carrying the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayOrigin {
    ClaudeSubscription,
    Foreign,
    Missing,
    Ambiguous,
}

/// Exact reasoning projection retained from the owning assistant turn.
/// Positions refer to native content, before any wire omission of empty text.
pub type ReasoningWitness = Vec<(usize, Vec<ReasoningContent>)>;

pub fn reasoning_witness(content: &[AssistantContent]) -> ReasoningWitness {
    content
        .iter()
        .enumerate()
        .filter_map(|(index, block)| match block {
            AssistantContent::Reasoning(reasoning) => Some((index, reasoning.content.clone())),
            _ => None,
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
pub enum ReplayUsage<'a> {
    Historical,
    RequiredCurrent {
        origin: ReplayOrigin,
        expected: Option<&'a ReasoningWitness>,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReplayEvidenceError {
    #[error("missingContinuationOrigin: required Claude continuation has no provider provenance")]
    MissingOrigin,
    #[error("foreignContinuationOrigin: required continuation was not produced by Claude")]
    ForeignOrigin,
    #[error(
        "ambiguousContinuationOrigin: required continuation has ambiguous provider provenance"
    )]
    AmbiguousOrigin,
    #[error("missingReasoningWitness: required continuation has no canonical reasoning witness")]
    MissingWitness,
    #[error(
        "alteredReasoning: required continuation differs from its canonical reasoning witness"
    )]
    AlteredReasoning,
}

/// Lean `ClaudeMap.narrowReplay`'s native narrowing stage. Strict wire encoding
/// below then checks signature/redaction shape. The runtime must supply the
/// witness independently of `content`; rebuilding it from this argument would
/// erase the missing/altered-block check. This does not license changes to a
/// provider-bound prompt prefix (tracked separately in #1693).
pub fn narrow_assistant_content(
    content: &[AssistantContent],
    usage: ReplayUsage<'_>,
) -> Result<Vec<AssistantContent>, ReplayEvidenceError> {
    match usage {
        ReplayUsage::Historical => Ok(content
            .iter()
            .filter(|block| !matches!(block, AssistantContent::Reasoning(_)))
            .cloned()
            .collect()),
        ReplayUsage::RequiredCurrent { origin, expected } => {
            match origin {
                ReplayOrigin::ClaudeSubscription => {}
                ReplayOrigin::Foreign => return Err(ReplayEvidenceError::ForeignOrigin),
                ReplayOrigin::Missing => return Err(ReplayEvidenceError::MissingOrigin),
                ReplayOrigin::Ambiguous => return Err(ReplayEvidenceError::AmbiguousOrigin),
            }
            let expected = expected.ok_or(ReplayEvidenceError::MissingWitness)?;
            if reasoning_witness(content) != *expected {
                return Err(ReplayEvidenceError::AlteredReasoning);
            }
            Ok(content.to_vec())
        }
    }
}

/// A physical provider-output coordinate. The request document identity is
/// part of the tag; provider message IDs are not continuation identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayTag {
    pub request_doc_id: String,
    pub source: OutputSource,
}

impl ReplayTag {
    fn is_provider(&self) -> bool {
        !self.request_doc_id.is_empty() && matches!(self.source, OutputSource::ProviderTurn { .. })
    }
}

/// One selected assistant occurrence and its independently carried sidecar.
/// `id` remains native provider data; neither preparation nor restore uses it
/// to associate a canonical source with this row.
#[derive(Clone, Debug, PartialEq)]
pub struct TaggedAssistantRow {
    pub source: Option<ReplayTag>,
    pub id: Option<String>,
    pub content: Vec<AssistantContent>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReplayCheckpoint {
    pub required: Vec<ReplayTag>,
    pub prefix_rows: Vec<TaggedAssistantRow>,
    pub retained: Vec<TaggedAssistantRow>,
}

/// Supplied by the canonical header/close/capture owner, never reconstructed
/// from a row's current native content.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedReplayEvidence {
    pub origin: ReplayOrigin,
    pub reasoning: ReasoningWitness,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReplayCheckpointError {
    #[error("invalidReplaySplit")]
    InvalidSplit,
    #[error("invalidReplayAssociation")]
    InvalidAssociation,
    #[error("duplicateReplayAssociation")]
    DuplicateAssociation,
    #[error("requiredReplayInPrefix")]
    RequiredInPrefix,
    #[error("missingRequiredReplay")]
    MissingRequired,
    #[error("{0}")]
    Evidence(#[from] ReplayEvidenceError),
    #[error("{0}")]
    Codec(#[from] AssistantCodecError),
}

/// Lean `ClaudeMap.prepareReplayCheckpoint`: the selected rows and required
/// coordinates are independent inputs. Splitting is exact and keeps source
/// sidecars attached to their row occurrences; it never discovers a tag from
/// equal native bytes or a provider-generated message ID.
pub fn prepare_replay_checkpoint(
    required: Vec<ReplayTag>,
    rows: Vec<TaggedAssistantRow>,
    split: usize,
) -> Result<ReplayCheckpoint, ReplayCheckpointError> {
    if split > rows.len() {
        return Err(ReplayCheckpointError::InvalidSplit);
    }
    if required.iter().any(|tag| !tag.is_provider())
        || rows
            .iter()
            .filter_map(|row| row.source.as_ref())
            .any(|tag| !tag.is_provider())
    {
        return Err(ReplayCheckpointError::InvalidAssociation);
    }
    if required
        .iter()
        .enumerate()
        .any(|(index, tag)| required[..index].contains(tag))
        || rows.iter().enumerate().any(|(index, row)| {
            row.source.as_ref().is_some_and(|tag| {
                rows[..index]
                    .iter()
                    .any(|prior| prior.source.as_ref() == Some(tag))
            })
        })
    {
        return Err(ReplayCheckpointError::DuplicateAssociation);
    }
    if required.iter().any(|tag| {
        rows[..split]
            .iter()
            .any(|row| row.source.as_ref() == Some(tag))
    }) {
        return Err(ReplayCheckpointError::RequiredInPrefix);
    }
    if required.iter().any(|tag| {
        rows[split..]
            .iter()
            .filter(|row| row.source.as_ref() == Some(tag))
            .count()
            != 1
    }) {
        return Err(ReplayCheckpointError::MissingRequired);
    }
    Ok(ReplayCheckpoint {
        required,
        prefix_rows: rows[..split].to_vec(),
        retained: rows[split..].to_vec(),
    })
}

/// One transient, checked assistant occurrence for the same downstream body
/// projection used by estimation, capture, and the live Claude transport.
#[derive(Clone, Debug, PartialEq)]
pub struct NarrowedAssistantRow {
    pub source: Option<ReplayTag>,
    pub id: Option<String>,
    pub content: Vec<AssistantContent>,
    pub wire_blocks: Vec<Value>,
}

/// Lean `ClaudeMap.restoreAndNarrowReplay`: validate the carried split again,
/// resolve only required coordinates, narrow in row order, then invoke the
/// same strict assistant codec that the live body builder uses. The resolver
/// must be backed by the owning canonical physical header/close/capture join;
/// this pure function cannot verify a stored sidecar against that database.
pub fn restore_and_narrow_replay(
    checkpoint: &ReplayCheckpoint,
    mut resolve: impl FnMut(&ReplayTag) -> Vec<ResolvedReplayEvidence>,
) -> Result<Vec<NarrowedAssistantRow>, ReplayCheckpointError> {
    let rows = checkpoint
        .prefix_rows
        .iter()
        .chain(&checkpoint.retained)
        .cloned()
        .collect();
    let checked = prepare_replay_checkpoint(
        checkpoint.required.clone(),
        rows,
        checkpoint.prefix_rows.len(),
    )?;
    checked
        .retained
        .into_iter()
        .map(|row| {
            let evidence = match row
                .source
                .as_ref()
                .filter(|tag| checked.required.contains(tag))
            {
                Some(tag) => match resolve(tag).as_slice() {
                    [] => return Err(ReplayEvidenceError::MissingOrigin.into()),
                    [evidence] => Some(evidence.clone()),
                    _ => return Err(ReplayEvidenceError::AmbiguousOrigin.into()),
                },
                None => None,
            };
            let usage = match evidence.as_ref() {
                Some(evidence) => ReplayUsage::RequiredCurrent {
                    origin: evidence.origin,
                    expected: Some(&evidence.reasoning),
                },
                None => ReplayUsage::Historical,
            };
            let content = narrow_assistant_content(&row.content, usage)?;
            let wire_blocks = encode_assistant_content(&content)?;
            Ok(NarrowedAssistantRow {
                source: row.source,
                id: row.id,
                content,
                wire_blocks,
            })
        })
        .collect()
}

/// Anthropic Messages JSON body from a rig `CompletionRequest`. The history
/// crosses the converter seam once (`rig_compat::from_rig_message`) and the
/// body is assembled over the native message family.
pub fn build_messages_body(model: &str, request: &CompletionRequest) -> anyhow::Result<Value> {
    let history: Vec<Message> = request
        .chat_history
        .iter()
        .map(crate::rig_compat::from_rig_message)
        .collect();
    let mut body = build_messages_body_native(
        model,
        request.preamble.as_deref(),
        request.max_tokens,
        &history,
        &request.tools,
    )?;
    if let Some(params) = &request.additional_params {
        apply_reasoning_parameters(model, params, &mut body);
    }
    Ok(body)
}

/// Lean `ClaudeMap.selectedEffort`: permit one supported effort field, not a
/// wholesale merge of additional_params. Sampling and arbitrary keys stay out.
pub fn apply_reasoning_parameters(_model: &str, params: &Value, body: &mut Value) {
    let Some(supported) = params
        .get(ADVERTISED_REASONING_EFFORTS_PARAM)
        .and_then(Value::as_array)
    else {
        return;
    };
    let Some(effort) = params["output_config"]["effort"].as_str() else {
        return;
    };
    if supported.iter().any(|value| value.as_str() == Some(effort))
        && matches!(effort, "low" | "medium" | "high" | "xhigh" | "max")
    {
        body["output_config"] = json!({ "effort": effort });
        body["thinking"] = json!({ "type": "adaptive" });
    }
}

/// Body assembly over the native message family (no rig vocabulary).
///
/// Lean: `systemBlocks`, `splitSystem`, `toolsField`. Two `cache_control`
/// breakpoints: the last `system` block (identity + preamble + System rows +
/// tools prefix) and the last content block of the last message (moving
/// breakpoint across tool_result turns).
pub fn build_messages_body_native(
    model: &str,
    preamble: Option<&str>,
    max_tokens: Option<u64>,
    history: &[Message],
    tools: &[ToolDefinition],
) -> anyhow::Result<Value> {
    let mut system: Vec<Value> = vec![json!({ "type": "text", "text": CLAUDE_CODE_IDENTITY })];
    if let Some(preamble) = preamble.map(str::trim).filter(|value| !value.is_empty()) {
        system.push(json!({ "type": "text", "text": preamble }));
    }
    for row in system_rows(history) {
        system.push(json!({ "type": "text", "text": row }));
    }
    mark_ephemeral(system.last_mut());

    let mut messages = anthropic_messages(history)?;
    if let Some(last) = messages.last_mut() {
        if let Some(blocks) = last.get_mut("content").and_then(Value::as_array_mut) {
            mark_ephemeral(blocks.last_mut());
        }
    }

    let tools: Vec<Value> = tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.parameters,
            })
        })
        .collect();

    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "stream": true,
        "system": system,
        "messages": messages,
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }
    // No sampling keys: live claude-sonnet-5 400s on `temperature` / `top_p`
    // / `top_k`; `additional_params` carries those and is not merged.
    Ok(body)
}

fn mark_ephemeral(block: Option<&mut Value>) {
    if let Some(Value::Object(map)) = block {
        map.insert("cache_control".to_string(), json!({ "type": "ephemeral" }));
    }
}

/// `Message::System` rows in transcript order (Lean `splitSystem`).
fn system_rows(history: &[Message]) -> Vec<String> {
    history
        .iter()
        .filter_map(|message| match message {
            Message::System { content } if !content.trim().is_empty() => Some(content.clone()),
            _ => None,
        })
        .collect()
}

fn anthropic_messages(history: &[Message]) -> anyhow::Result<Vec<Value>> {
    let mut out = Vec::new();
    for message in history {
        match message {
            Message::User { content } => {
                let mut blocks = Vec::new();
                for block in content {
                    match block {
                        UserContent::Text(text) if !text.text.is_empty() => {
                            blocks.push(json!({"type": "text", "text": text.text}));
                        }
                        UserContent::ToolResult(result) => {
                            let body: String = result
                                .content
                                .iter()
                                .filter_map(|item| match item {
                                    ToolResultContent::Text(text) => Some(text.text.as_str()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("");
                            blocks.push(json!({
                                "type": "tool_result",
                                "tool_use_id": result.id,
                                "content": body,
                            }));
                        }
                        _ => {}
                    }
                }
                if !blocks.is_empty() {
                    out.push(json!({"role": "user", "content": blocks}));
                }
            }
            Message::Assistant { content, .. } => {
                let blocks = encode_assistant_content(content)?;
                if !blocks.is_empty() {
                    out.push(json!({"role": "assistant", "content": blocks}));
                }
            }
            // Rows were lifted into `system` by `system_rows`.
            Message::System { .. } => {}
        }
    }
    Ok(out)
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AssistantCodecError {
    #[error("unsupported assistant image in Claude replay")]
    UnsupportedImage,
    #[error("Claude thinking replay requires a nonempty signature")]
    MissingSignature,
    #[error("Claude redacted thinking replay has no data")]
    EmptyRedacted,
    #[error("unsupported native reasoning kind in Claude replay")]
    UnsupportedReasoning,
}

/// Strict assistant-side block codec shared by the live Claude body builder
/// and restored continuation. Empty ordinary text is omitted on this wire;
/// signed empty thinking is preserved.
pub fn encode_assistant_content(
    content: &[AssistantContent],
) -> Result<Vec<Value>, AssistantCodecError> {
    let mut blocks = Vec::new();
    for block in content {
        match block {
            AssistantContent::Text(text) if !text.text.is_empty() => {
                blocks.push(json!({"type": "text", "text": text.text}));
            }
            AssistantContent::Text(_) => {}
            AssistantContent::Image(_) => return Err(AssistantCodecError::UnsupportedImage),
            AssistantContent::ToolCall(call) => {
                blocks.push(json!({
                    "type": "tool_use",
                    "id": call.id,
                    "name": call.function.name,
                    "input": call.function.arguments,
                }));
            }
            AssistantContent::Reasoning(reasoning) => {
                for part in &reasoning.content {
                    match part {
                        ReasoningContent::Text { text, signature } => {
                            let signature = signature
                                .as_deref()
                                .filter(|signature| !signature.is_empty())
                                .ok_or(AssistantCodecError::MissingSignature)?;
                            blocks.push(json!({
                                "type": "thinking",
                                "thinking": text,
                                "signature": signature,
                            }));
                        }
                        ReasoningContent::Redacted { data } if !data.is_empty() => {
                            blocks.push(json!({
                                "type": "redacted_thinking",
                                "data": data,
                            }));
                        }
                        ReasoningContent::Redacted { .. } => {
                            return Err(AssistantCodecError::EmptyRedacted);
                        }
                        ReasoningContent::Encrypted(_) | ReasoningContent::Summary(_) => {
                            return Err(AssistantCodecError::UnsupportedReasoning);
                        }
                    }
                }
            }
        }
    }
    Ok(blocks)
}
