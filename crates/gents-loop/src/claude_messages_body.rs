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

use crate::provider_input::replay_frontier::{
    admissible_turn_drop, anchored, ords, FlatItem, Turn,
};

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
    AcceptedProvider,
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
    pub physical_header: Option<String>,
    pub block_indices: Vec<usize>,
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
    pub issuer: ReplayIssuer,
    pub wire: ReplayWire,
    pub physical_header: String,
    pub complete: bool,
    /// The flattened accepted request that produced the turn; `None` when the
    /// capture is missing or undecodable.
    pub captured: Option<Vec<FlatItem>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayIssuer {
    pub family: String,
    /// Opaque, injectively encoded transport route identity supplied by the
    /// capture owner; never infer this from a provider-assigned message ID.
    pub endpoint: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayWire {
    ClaudeMessages,
    Responses,
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

/// Lean `ClaudeMap.restoreContiguousReplay`: validate the carried split, then
/// encode each already selected row with the strict codec the live body
/// builder uses.
pub fn restore_contiguous_replay(
    checkpoint: &ReplayCheckpoint,
) -> Result<Vec<Vec<Value>>, ReplayCheckpointError> {
    let rows = checkpoint
        .prefix_rows
        .iter()
        .chain(&checkpoint.retained)
        .cloned()
        .collect();
    let checked = prepare_replay_checkpoint(Vec::new(), rows, checkpoint.prefix_rows.len())?;
    checked
        .retained
        .iter()
        .map(|row| Ok(encode_assistant_content(&row.content)?))
        .collect()
}

struct ReasoningCandidate<'a> {
    source: Option<&'a ReplayTag>,
    physical_header: Option<&'a str>,
    block_index: Option<usize>,
    indices_complete: bool,
    parts: &'a [ReasoningContent],
}

fn row_candidates(row: &TaggedAssistantRow) -> Vec<ReasoningCandidate<'_>> {
    let indices_complete = row.block_indices.len() == row.content.len()
        && row.block_indices.windows(2).all(|pair| pair[0] < pair[1]);
    row.content
        .iter()
        .enumerate()
        .filter_map(|(position, block)| match block {
            AssistantContent::Reasoning(reasoning) => Some(ReasoningCandidate {
                source: row.source.as_ref(),
                physical_header: row.physical_header.as_deref(),
                block_index: row.block_indices.get(position).copied(),
                indices_complete,
                parts: &reasoning.content,
            }),
            _ => None,
        })
        .collect()
}

/// Lean `ClaudeMap.TurnGroup`: one accepted row's reasoning and whether the row
/// keeps an encodable ordinary block once its reasoning is removed.
struct TurnGroup<'a> {
    candidates: Vec<ReasoningCandidate<'a>>,
    ordinary: bool,
}

fn turn_groups(rows: &[TaggedAssistantRow]) -> Vec<TurnGroup<'_>> {
    rows.iter()
        .filter_map(|row| {
            let candidates = row_candidates(row);
            (!candidates.is_empty()).then(|| TurnGroup {
                candidates,
                ordinary: row.content.iter().any(|block| match block {
                    AssistantContent::Text(text) => !text.text.is_empty(),
                    AssistantContent::ToolCall(_) => true,
                    AssistantContent::Reasoning(_) | AssistantContent::Image(_) => false,
                }),
            })
        })
        .collect()
}

fn replayable_reasoning(wire: ReplayWire, parts: &[ReasoningContent]) -> bool {
    if parts.is_empty() {
        return false;
    }
    match wire {
        ReplayWire::ClaudeMessages => encode_assistant_content(&[AssistantContent::Reasoning(
            gents_protocol::message::Reasoning {
                id: None,
                content: parts.to_vec(),
            },
        )])
        .is_ok(),
        ReplayWire::Responses => {
            parts
                .iter()
                .any(|part| matches!(part, ReasoningContent::Encrypted(bytes) if !bytes.is_empty()))
                && parts.iter().all(|part| match part {
                    ReasoningContent::Encrypted(bytes) => !bytes.is_empty(),
                    ReasoningContent::Summary(_) => true,
                    _ => false,
                })
        }
    }
}

/// Lean `ClaudeMap.wireReasoningCount`.
fn wire_reasoning_count(wire: ReplayWire, parts: &[ReasoningContent]) -> usize {
    match wire {
        ReplayWire::ClaudeMessages => parts.len(),
        ReplayWire::Responses => 1,
    }
}

/// Lean `ClaudeMap.turnHeaderOffset`.
fn turn_header_offset(wire: ReplayWire) -> usize {
    match wire {
        ReplayWire::ClaudeMessages => 1,
        ReplayWire::Responses => 0,
    }
}

/// Evidence resolved once per selection; the owner queries every tag.
struct Selection<'a> {
    rows: &'a [TaggedAssistantRow],
    issuer: &'a ReplayIssuer,
    wire: ReplayWire,
    groups: Vec<TurnGroup<'a>>,
    resolved: Vec<(ReplayTag, Vec<ResolvedReplayEvidence>)>,
}

impl<'a> Selection<'a> {
    fn new(
        rows: &'a [TaggedAssistantRow],
        issuer: &'a ReplayIssuer,
        wire: ReplayWire,
        mut resolve: impl FnMut(&ReplayTag) -> Vec<ResolvedReplayEvidence>,
    ) -> Self {
        let groups = turn_groups(rows);
        let mut resolved: Vec<(ReplayTag, Vec<ResolvedReplayEvidence>)> = Vec::new();
        for tag in groups
            .iter()
            .flat_map(|group| &group.candidates)
            .filter_map(|candidate| candidate.source)
        {
            if !resolved.iter().any(|(known, _)| known == tag) {
                resolved.push((tag.clone(), resolve(tag)));
            }
        }
        Self {
            rows,
            issuer,
            wire,
            groups,
            resolved,
        }
    }

    fn evidence(&self, tag: &ReplayTag) -> &[ResolvedReplayEvidence] {
        self.resolved
            .iter()
            .find(|(known, _)| known == tag)
            .map(|(_, evidence)| evidence.as_slice())
            .unwrap_or_default()
    }

    /// Lean `ClaudeMap.validForReplayProjection`.
    fn candidate_valid(&self, candidate: &ReasoningCandidate<'_>) -> bool {
        let (Some(tag), Some(header), Some(index)) = (
            candidate.source,
            candidate.physical_header,
            candidate.block_index,
        ) else {
            return false;
        };
        if !tag.is_provider()
            || !candidate.indices_complete
            || self
                .groups
                .iter()
                .flat_map(|group| &group.candidates)
                .filter(|other| {
                    other.source == Some(tag)
                        && other.physical_header == Some(header)
                        && other.block_index == Some(index)
                })
                .count()
                != 1
            || !replayable_reasoning(self.wire, candidate.parts)
        {
            return false;
        }
        let [evidence] = self.evidence(tag) else {
            return false;
        };
        evidence.origin == ReplayOrigin::AcceptedProvider
            && &evidence.issuer == self.issuer
            && evidence.wire == self.wire
            && evidence.physical_header == header
            && evidence.complete
            && evidence.reasoning.iter().any(|(block_index, parts)| {
                *block_index == index && parts.as_slice() == candidate.parts
            })
    }

    /// Lean `ClaudeMap.turnBase`.
    fn turn_base(&self, group: &TurnGroup<'_>) -> bool {
        !group.candidates.is_empty()
            && (self.wire == ReplayWire::Responses || group.ordinary)
            && group
                .candidates
                .iter()
                .all(|candidate| self.candidate_valid(candidate))
    }

    /// Lean `ClaudeMap.turnCaptured`.
    fn turn_captured(&self, group: &TurnGroup<'_>) -> Option<Vec<FlatItem>> {
        let tag = group.candidates.first()?.source?;
        match self.evidence(tag) {
            [evidence] => evidence.captured.clone(),
            _ => None,
        }
    }

    /// Lean `ClaudeMap.replayBaseKept`, as the index of its first group.
    fn base_start(&self) -> usize {
        self.groups
            .iter()
            .rposition(|group| !self.turn_base(group))
            .map_or(0, |index| index + 1)
    }

    fn candidates_before(&self, group: usize) -> usize {
        self.groups[..group]
            .iter()
            .map(|group| group.candidates.len())
            .sum()
    }
}

/// Lean `ClaudeMap.locateTurns`: each assembled turn's ordinary prefix and its
/// own anchored reasoning items. Any disagreement with the expected wire
/// counts fails closed.
fn locate_turns(
    offset: usize,
    body: &[FlatItem],
    counts: &[usize],
) -> Option<Vec<(Vec<Vec<u8>>, Vec<(usize, Vec<u8>)>)>> {
    let positions = body
        .iter()
        .enumerate()
        .filter_map(|(index, item)| matches!(item, FlatItem::Reasoning(_)).then_some(index))
        .collect::<Vec<_>>();
    if counts.iter().sum::<usize>() != positions.len() || counts.contains(&0) {
        return None;
    }
    let anchored_items = anchored(body);
    let mut seen = 0;
    let mut layouts = Vec::with_capacity(counts.len());
    for count in counts {
        let position = *positions.get(seen)?;
        let start = position.checked_sub(offset)?;
        if offset != 0 && !matches!(body.get(start), Some(FlatItem::Ordinary(_))) {
            return None;
        }
        layouts.push((
            ords(&body[..start])
                .into_iter()
                .map(<[u8]>::to_vec)
                .collect(),
            anchored_items[seen..seen + count]
                .iter()
                .map(|(anchor, bytes)| (*anchor, bytes.to_vec()))
                .collect(),
        ));
        seen += count;
    }
    Some(layouts)
}

/// Lean `ClaudeMap.stripFirstReasoningRows`: ordinary blocks and their original
/// indices stay in place.
fn strip_first_reasoning(rows: &[TaggedAssistantRow], mut count: usize) -> Vec<TaggedAssistantRow> {
    rows.iter()
        .map(|row| {
            let valid_indices = row.block_indices.len() == row.content.len()
                && row.block_indices.windows(2).all(|pair| pair[0] < pair[1]);
            let mut narrowed = row.clone();
            if !valid_indices {
                narrowed.physical_header = None;
            }
            narrowed.content.clear();
            narrowed.block_indices.clear();
            for (position, block) in row.content.iter().enumerate() {
                if matches!(block, AssistantContent::Reasoning(_)) && count > 0 {
                    count -= 1;
                    continue;
                }
                narrowed.content.push(block.clone());
                if let Some(index) = row.block_indices.get(position) {
                    narrowed.block_indices.push(*index);
                }
            }
            narrowed
        })
        .collect()
}

/// Lean `ClaudeMap.replayStage`: the rows the owned loop assembles so the
/// provenance-valid turns can be located in the actual provider body.
pub fn replay_stage(
    rows: &[TaggedAssistantRow],
    issuer: &ReplayIssuer,
    wire: ReplayWire,
    resolve: impl FnMut(&ReplayTag) -> Vec<ResolvedReplayEvidence>,
) -> Vec<TaggedAssistantRow> {
    let selection = Selection::new(rows, issuer, wire, resolve);
    strip_first_reasoning(rows, selection.candidates_before(selection.base_start()))
}

/// Lean `ClaudeMap.restoreHistoricalReasoningSuffix`. `stage_body` is the
/// flattened provider body the owned loop assembled from `replay_stage`. Only
/// the longest suffix of whole accepted turns whose assembled prefix is their
/// accepted producing prefix minus a leading reasoning run keeps reasoning;
/// ordinary blocks stay in their rows.
pub fn select_replay(
    rows: &[TaggedAssistantRow],
    issuer: &ReplayIssuer,
    wire: ReplayWire,
    resolve: impl FnMut(&ReplayTag) -> Vec<ResolvedReplayEvidence>,
    stage_body: &[FlatItem],
) -> Vec<TaggedAssistantRow> {
    let selection = Selection::new(rows, issuer, wire, resolve);
    let total = selection.candidates_before(selection.groups.len());
    let base_start = selection.base_start();
    let base_kept = &selection.groups[base_start..];
    let counts = base_kept
        .iter()
        .map(|group| {
            group
                .candidates
                .iter()
                .map(|candidate| wire_reasoning_count(wire, candidate.parts))
                .sum()
        })
        .collect::<Vec<usize>>();
    let kept_start = match locate_turns(turn_header_offset(wire), stage_body, &counts) {
        None => selection.groups.len(),
        Some(layouts) => {
            let turns = base_kept
                .iter()
                .zip(layouts)
                .map(|(group, (prefix_ords, items))| Turn {
                    base: selection.turn_base(group),
                    prefix_ords,
                    items,
                    captured: selection.turn_captured(group),
                })
                .collect::<Vec<_>>();
            base_start + admissible_turn_drop(&turns)
        }
    };
    let kept = total - selection.candidates_before(kept_start);
    strip_first_reasoning(selection.rows, total - kept)
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
