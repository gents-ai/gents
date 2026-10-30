use serde::Deserialize;

use super::{required_nullable, LeanCanonicalCoordinate};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCurrentInputHeader {
    pub(crate) request: String,
    pub(crate) kind: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCurrentInputCase {
    pub(crate) name: String,
    pub(crate) current_request: String,
    pub(crate) headers: Vec<LeanCurrentInputHeader>,
    pub(crate) retained_indices: Vec<usize>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanPromptAssemblyItem {
    pub(crate) item: String,
    pub(crate) value: u64,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum LeanAssistantOrderMode {
    Grouped,
    NativePreserved,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanPromptAssemblyAssistantOrderCase {
    pub(crate) name: String,
    pub(crate) order_mode: LeanAssistantOrderMode,
    pub(crate) input: Vec<LeanPromptAssemblyItem>,
    pub(crate) expected: Vec<LeanPromptAssemblyItem>,
    pub(crate) expected_twice: Vec<LeanPromptAssemblyItem>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanPromptAssemblyRow {
    pub(crate) role: String,
    pub(crate) kind: String,
    pub(crate) call_ids: Vec<u64>,
    pub(crate) content: Vec<LeanPromptAssemblyItem>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanPromptAssemblyModeSanitizeCase {
    pub(crate) name: String,
    pub(crate) order_mode: LeanAssistantOrderMode,
    pub(crate) input: Vec<LeanPromptAssemblyRow>,
    pub(crate) expected: Vec<LeanPromptAssemblyRow>,
    pub(crate) expected_twice: Vec<LeanPromptAssemblyRow>,
    pub(crate) association: Vec<LeanPromptAssemblyRow>,
    pub(crate) association_indices: Vec<Vec<usize>>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanPromptAssemblySplit {
    pub(crate) index: usize,
    pub(crate) expected: Vec<LeanPromptAssemblyRow>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanPromptAssemblySanitizeCase {
    pub(crate) name: String,
    pub(crate) input: Vec<LeanPromptAssemblyRow>,
    pub(crate) expected: Vec<LeanPromptAssemblyRow>,
    pub(crate) expected_twice: Vec<LeanPromptAssemblyRow>,
    pub(crate) splits: Vec<LeanPromptAssemblySplit>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanPromptAssemblyLayerCase {
    pub(crate) name: String,
    pub(crate) skill_count: usize,
    pub(crate) summary_count: usize,
    pub(crate) conversation_len: usize,
    pub(crate) slots: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanPromptAssemblyRepairCase {
    pub(crate) name: String,
    pub(crate) input: String,
    pub(crate) expected: String,
    pub(crate) expected_twice: String,
    pub(crate) payload_only: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanPromptAssemblyBudgetCase {
    pub(crate) name: String,
    pub(crate) context_window: usize,
    pub(crate) max_output_tokens: usize,
    pub(crate) threshold_basis_points: usize,
    pub(crate) configured_threshold_budget: usize,
    pub(crate) prompt_tokens: usize,
    pub(crate) request_tokens: usize,
    pub(crate) effective_input_budget: usize,
    pub(crate) compaction_input_budget: usize,
    pub(crate) effective_output_tokens: usize,
    pub(crate) should_compact: bool,
    pub(crate) provider_safe: bool,
    pub(crate) can_dispatch: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanPromptAssemblyTurnBudgetCase {
    pub(crate) name: String,
    pub(crate) context_window: usize,
    pub(crate) max_output_tokens: usize,
    pub(crate) threshold_basis_points: usize,
    pub(crate) configured_threshold_budget: usize,
    pub(crate) effective_input_budget: usize,
    pub(crate) compaction_input_budget: usize,
    pub(crate) turn_input_tokens: Vec<usize>,
    pub(crate) turn_output_tokens: Vec<usize>,
    pub(crate) turn_should_compact: Vec<bool>,
    pub(crate) turn_can_dispatch: Vec<bool>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LeanContextAnchor {
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) estimated_input_tokens: usize,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LeanReductionAdmission {
    pub(crate) input_tokens: usize,
    pub(crate) input_budget: usize,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanPromptAssemblyCompactionTriggerCase {
    pub(crate) name: String,
    pub(crate) context_window: usize,
    pub(crate) max_output_tokens: usize,
    pub(crate) threshold_basis_points: u64,
    pub(crate) prior_basis_points: u64,
    pub(crate) seeded_observations: Vec<u64>,
    pub(crate) observed: Vec<(u64, usize)>,
    pub(crate) anchor: Option<LeanContextAnchor>,
    pub(crate) invalidate: bool,
    pub(crate) raw_input_tokens: usize,
    pub(crate) ratio_basis_points: u64,
    pub(crate) calibrated_tokens: usize,
    pub(crate) anchored_tokens: Option<usize>,
    pub(crate) context_tokens: usize,
    pub(crate) compaction_input_budget: usize,
    pub(crate) admission: Option<LeanReductionAdmission>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanPromptAssemblyRetentionCase {
    pub(crate) name: String,
    pub(crate) configured_keep_recent: usize,
    pub(crate) effective_input_budget: usize,
    pub(crate) fixed_input: usize,
    pub(crate) available_input: usize,
    pub(crate) retention_target: usize,
    pub(crate) summary_max_output: usize,
    pub(crate) effective_summary_output: usize,
    pub(crate) rolling_summary_input_budget: usize,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanPromptAssemblyClaudeMapCase {
    pub(crate) name: String,
    pub(crate) surface: Vec<String>,
    pub(crate) blocks: Vec<String>,
    pub(crate) outcome: String,
    pub(crate) ids: Vec<u64>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanPromptAssemblyClaudeBodyCase {
    pub(crate) name: String,
    pub(crate) attribution: String,
    pub(crate) preamble: Option<String>,
    pub(crate) rows: Vec<String>,
    pub(crate) tools: Vec<String>,
    pub(crate) system: Vec<String>,
    pub(crate) system_marked: Vec<bool>,
    pub(crate) tools_present: bool,
    pub(crate) supported_efforts: Option<Vec<String>>,
    pub(crate) effort: Option<String>,
    pub(crate) selected_effort: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanPromptAssemblyClaudeAttributionCase {
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) fingerprint: String,
    pub(crate) entrypoint: String,
    pub(crate) provider: String,
    pub(crate) first_party_base_url: bool,
    pub(crate) workload: Option<String>,
    pub(crate) subagent: bool,
    pub(crate) previous_request_id: Option<String>,
    pub(crate) prompt_id: Option<String>,
    pub(crate) turn_origin: Option<String>,
    pub(crate) prompt_index: Option<u64>,
    pub(crate) turn_index: Option<u64>,
    pub(crate) gateway: Option<String>,
    pub(crate) project_id: Option<String>,
    pub(crate) project_source: Option<String>,
    pub(crate) header: String,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanClaudeCacheBlock {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) text: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanClaudeCacheContent {
    pub(crate) text: Option<String>,
    pub(crate) blocks: Option<Vec<LeanClaudeCacheBlock>>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanClaudeCacheMessage {
    pub(crate) role: String,
    pub(crate) content: LeanClaudeCacheContent,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanClaudeCachePlaced {
    pub(crate) role: String,
    pub(crate) text: Option<String>,
    pub(crate) blocks: Option<Vec<LeanClaudeCacheBlock>>,
    pub(crate) marker: Option<usize>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanPromptAssemblyClaudeCacheCase {
    pub(crate) name: String,
    pub(crate) caching: bool,
    pub(crate) skip_write: bool,
    pub(crate) subscriber: bool,
    pub(crate) main_thread: bool,
    pub(crate) using_overage: bool,
    pub(crate) messages: Vec<LeanClaudeCacheMessage>,
    pub(crate) placed: Vec<LeanClaudeCachePlaced>,
    pub(crate) ttl: String,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanPromptAssemblyClaudeOriginCase {
    pub(crate) name: String,
    pub(crate) cause: String,
    pub(crate) root_client: bool,
    pub(crate) scheduled: bool,
    pub(crate) kind: String,
    pub(crate) appends_system_prompt: bool,
    pub(crate) identity: String,
    pub(crate) entrypoint: String,
    pub(crate) background: bool,
    pub(crate) workload: Option<String>,
    pub(crate) subagent: bool,
    pub(crate) request_class: String,
    pub(crate) main_thread_source: bool,
    pub(crate) agentic: bool,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanPromptAssemblyClaudeStreamCase {
    pub(crate) name: String,
    pub(crate) surface: Vec<String>,
    pub(crate) events: Vec<String>,
    pub(crate) outcome: String,
    pub(crate) calls: Vec<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanPromptAssemblyClaudeThinkingStreamCase {
    pub(crate) name: String,
    pub(crate) surface: Vec<String>,
    pub(crate) events: Vec<LeanClaudeStreamEvent>,
    pub(crate) outcome: String,
    pub(crate) steps: Vec<LeanClaudeContentStep>,
    pub(crate) content: Vec<LeanClaudeStreamBlock>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanPromptAssemblyClaudeWireStartCase {
    pub(crate) name: String,
    pub(crate) start: LeanClaudeWireThinkingStart,
    pub(crate) later: Vec<LeanClaudeStreamEvent>,
    pub(crate) expected: LeanClaudeWireExpected,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanPromptAssemblyServiceTierCase {
    pub(crate) family: String,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) requested: Option<String>,
    pub(crate) allowed: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) wire: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanPromptAssemblyResponsesStorageCase {
    pub(crate) name: String,
    pub(crate) family: String,
    pub(crate) wire: String,
    pub(crate) endpoint: String,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) store: Option<bool>,
    pub(crate) encrypted_include: bool,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanPromptAssemblyResponsesEffortCase {
    pub(crate) name: String,
    pub(crate) endpoint: String,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) advertised: Option<Vec<String>>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) requested: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) expected: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanClaudeWireThinkingStart {
    pub(crate) index: u64,
    pub(crate) thinking: String,
    pub(crate) signature_present: bool,
    pub(crate) signature: serde_json::Value,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanClaudeWireExpected {
    pub(crate) kind: String,
    pub(crate) error: Option<String>,
    pub(crate) steps: Option<Vec<LeanClaudeContentStep>>,
    pub(crate) content: Option<Vec<LeanClaudeStreamBlock>>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanClaudeStreamEvent {
    pub(crate) kind: String,
    pub(crate) value: Option<String>,
    pub(crate) id: Option<u64>,
    pub(crate) name: Option<String>,
    pub(crate) input: Option<String>,
    pub(crate) index: Option<u64>,
    pub(crate) fragment: Option<String>,
    pub(crate) data: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanClaudeReasoningPart {
    pub(crate) kind: String,
    pub(crate) payload: String,
    pub(crate) signature: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanClaudeStreamBlock {
    pub(crate) kind: String,
    pub(crate) value: Option<String>,
    pub(crate) parts: Option<Vec<LeanClaudeReasoningPart>>,
    pub(crate) id: Option<u64>,
    pub(crate) name: Option<String>,
    pub(crate) arguments: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanClaudeContentStep {
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) provisional_thinking: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) provisional_signature: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) provisional_redacted: Option<String>,
    pub(crate) sealed: Vec<LeanClaudeStreamBlock>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanPromptAssemblyClaudeReplayCase {
    pub(crate) name: String,
    pub(crate) blocks: Vec<LeanClaudeReplayInputBlock>,
    pub(crate) outcome: String,
    pub(crate) replay: Vec<LeanClaudeReplayBlock>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum LeanClaudeReplayOrigin {
    ClaudeSubscription,
    AcceptedProvider,
    Foreign,
    Missing,
    Ambiguous,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanClaudeReasoningWitness {
    pub(crate) block_index: usize,
    pub(crate) parts: Vec<LeanClaudeReplayInputPart>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanPromptAssemblyClaudeCheckpointCase {
    pub(crate) name: String,
    pub(crate) required: Vec<LeanCanonicalCoordinate>,
    pub(crate) rows: Vec<LeanClaudeTaggedReplayRow>,
    pub(crate) split: usize,
    pub(crate) carrier_ids: Vec<String>,
    pub(crate) resolutions: Vec<LeanClaudeCheckpointResolution>,
    pub(crate) outcome: String,
    pub(crate) prefix_rows: Vec<LeanClaudeTaggedReplayRow>,
    pub(crate) retained: Vec<LeanClaudeTaggedReplayRow>,
    pub(crate) replay: Vec<Vec<LeanClaudeReplayBlock>>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanProtectedReplayCompactionCase {
    pub(crate) name: String,
    pub(crate) message_count: usize,
    pub(crate) raw_index: usize,
    pub(crate) required: Vec<LeanCanonicalCoordinate>,
    pub(crate) rows: Vec<LeanClaudeTaggedReplayRow>,
    pub(crate) ceiling_error: String,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) ceiling: Option<usize>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) selected_split: Option<usize>,
    pub(crate) outcome: String,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanClaudeTaggedReplayRow {
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) source: Option<LeanCanonicalCoordinate>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) physical_header: Option<String>,
    pub(crate) block_indices: Vec<usize>,
    pub(crate) blocks: Vec<LeanClaudeReplayInputBlock>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanClaudeCheckpointResolution {
    pub(crate) tag: LeanCanonicalCoordinate,
    pub(crate) origin: LeanClaudeReplayOrigin,
    pub(crate) issuer_family: String,
    pub(crate) issuer_endpoint: String,
    pub(crate) wire: LeanReasoningReplayWire,
    pub(crate) physical_header: String,
    pub(crate) complete: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) captured: Option<Vec<LeanReplayFlatItem>>,
    pub(crate) expected_reasoning: Vec<LeanClaudeReasoningWitness>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LeanReplayFlatKind {
    Ordinary,
    Reasoning,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanReplayFlatItem {
    pub(crate) kind: LeanReplayFlatKind,
    pub(crate) bytes: Vec<u8>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LeanReasoningReplayWire {
    ClaudeMessages,
    Responses,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanPromptAssemblyReasoningSuffixCase {
    pub(crate) name: String,
    pub(crate) issuer_family: String,
    pub(crate) issuer_endpoint: String,
    pub(crate) wire: LeanReasoningReplayWire,
    pub(crate) rows: Vec<LeanClaudeTaggedReplayRow>,
    pub(crate) resolutions: Vec<LeanClaudeCheckpointResolution>,
    pub(crate) stage: Vec<LeanClaudeTaggedReplayRow>,
    pub(crate) assembled: Vec<LeanReplayFlatItem>,
    pub(crate) replay: Vec<LeanClaudeTaggedReplayRow>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanPromptAssemblyReplayShapeCase {
    pub(crate) name: String,
    pub(crate) source: LeanClaudeTaggedReplayRow,
    pub(crate) retained_indices: Vec<usize>,
    pub(crate) outcome: String,
    pub(crate) shaped: Option<LeanClaudeTaggedReplayRow>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanPromptAssemblyReplayPrefixCase {
    pub(crate) name: String,
    pub(crate) captured: Vec<LeanReplayFlatItem>,
    pub(crate) current: Vec<LeanReplayFlatItem>,
    pub(crate) ordinary_equal: bool,
    pub(crate) reasoning_suffix: bool,
    pub(crate) leading_removal: bool,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanClaudeReplayInputBlock {
    pub(crate) kind: String,
    pub(crate) payload: Option<Vec<u8>>,
    pub(crate) parts: Option<Vec<LeanClaudeReplayInputPart>>,
    pub(crate) id: Option<String>,
    pub(crate) doc_id: Option<u64>,
    pub(crate) call_id: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) arguments: Option<Vec<u8>>,
    pub(crate) signature: Option<String>,
    pub(crate) additional_params: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanClaudeReplayInputPart {
    pub(crate) kind: String,
    pub(crate) payload: Vec<u8>,
    pub(crate) signature: Option<String>,
}

pub(crate) fn native_claude_reasoning_parts(
    parts: &[LeanClaudeReplayInputPart],
) -> Vec<gents_protocol::message::ReasoningContent> {
    use gents_protocol::message::ReasoningContent;
    parts
        .iter()
        .map(|part| {
            let payload = String::from_utf8(part.payload.clone()).expect("reconstructed UTF-8");
            match part.kind.as_str() {
                "text" => ReasoningContent::Text {
                    text: payload,
                    signature: part.signature.clone(),
                },
                "redacted" => ReasoningContent::Redacted { data: payload },
                "encrypted" => ReasoningContent::Encrypted(payload),
                "summary" => ReasoningContent::Summary(payload),
                other => panic!("unknown modeled reasoning part {other}"),
            }
        })
        .collect()
}

pub(crate) fn native_claude_replay_content(
    blocks: &[LeanClaudeReplayInputBlock],
) -> Vec<gents_protocol::message::AssistantContent> {
    use gents_protocol::message::{
        AssistantContent, Image, Reasoning, Text, ToolCall, ToolFunction,
    };
    let mut content = Vec::new();
    for block in blocks {
        match block.kind.as_str() {
            "text" => content.push(AssistantContent::Text(Text {
                text: String::from_utf8(block.payload.clone().expect("text payload"))
                    .expect("reconstructed UTF-8"),
            })),
            "reasoning" => {
                let parts =
                    native_claude_reasoning_parts(block.parts.as_ref().expect("reasoning parts"));
                content.push(AssistantContent::Reasoning(Reasoning {
                    id: block.id.clone(),
                    content: parts,
                }));
            }
            "toolCall" => {
                let arguments = String::from_utf8(block.arguments.clone().expect("arguments"))
                    .expect("reconstructed UTF-8");
                content.push(AssistantContent::ToolCall(ToolCall {
                    id: block.id.clone().expect("provider tool id"),
                    call_id: block.call_id.clone(),
                    function: ToolFunction {
                        name: block.name.clone().expect("tool name"),
                        arguments: serde_json::from_str(&arguments)
                            .expect("modeled JSON arguments"),
                    },
                    signature: block.signature.clone(),
                    additional_params: block.additional_params.as_ref().map(|value| {
                        serde_json::from_str(value).expect("modeled additional params JSON")
                    }),
                }));
            }
            "unsupportedMedia" => content.push(AssistantContent::Image(Image::default())),
            other => panic!("unsupported modeled replay input block {other}"),
        }
    }
    content
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct LeanClaudeReplayBlock {
    pub(crate) kind: String,
    pub(crate) payload: Option<Vec<u8>>,
    pub(crate) signature: Option<String>,
    pub(crate) id: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) arguments: Option<Vec<u8>>,
}

impl LeanClaudeReplayBlock {
    pub(crate) fn native_json(&self) -> serde_json::Value {
        let payload = || {
            String::from_utf8(self.payload.clone().expect("replay payload"))
                .expect("reconstructed UTF-8")
        };
        match self.kind.as_str() {
            "text" => serde_json::json!({"type":"text", "text":payload()}),
            "signedThinking" => serde_json::json!({
                "type":"thinking", "thinking":payload(),
                "signature":self.signature.as_deref().expect("modeled signature")
            }),
            "redactedThinking" => serde_json::json!({
                "type":"redacted_thinking", "data":payload()
            }),
            "toolUse" => {
                let arguments = String::from_utf8(self.arguments.clone().expect("arguments"))
                    .expect("reconstructed UTF-8");
                serde_json::json!({
                    "type":"tool_use", "id":self.id.as_deref().expect("provider id"),
                    "name":self.name.as_deref().expect("tool name"),
                    "input":serde_json::from_str::<serde_json::Value>(&arguments)
                        .expect("modeled JSON arguments")
                })
            }
            other => panic!("unknown modeled replay output {other}"),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanAggregateTokenBudgetCase {
    pub(crate) request_doc_id: String,
    pub(crate) prior_request_doc_ids: Vec<String>,
    pub(crate) prior_call_kinds: Vec<String>,
    pub(crate) name: String,
    pub(crate) limit: u64,
    pub(crate) used: u64,
    pub(crate) prior_prompt_tokens: Vec<u64>,
    pub(crate) prior_completion_tokens: Vec<u64>,
    pub(crate) input_tokens: u64,
    pub(crate) configured_max_output_tokens: u64,
    pub(crate) reported_input_tokens: u64,
    pub(crate) reported_output_tokens: u64,
    pub(crate) reported_total_tokens: u64,
    pub(crate) usage_present: bool,
    pub(crate) terminal_valid: bool,
    pub(crate) effective_output_tokens: u64,
    pub(crate) can_dispatch: bool,
    pub(crate) charged_tokens: u64,
    pub(crate) charge_result: String,
    pub(crate) next_used: Option<u64>,
    pub(crate) post_charge_action: String,
}
