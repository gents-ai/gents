use serde::Deserialize;

use super::{required_nullable, LeanCanonicalCoordinate};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCurrentInputHeader {
    pub request: String,
    pub kind: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCurrentInputCase {
    pub name: String,
    pub current_request: String,
    pub headers: Vec<LeanCurrentInputHeader>,
    pub retained_indices: Vec<usize>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanPromptAssemblyItem {
    pub item: String,
    pub value: u64,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LeanAssistantOrderMode {
    Grouped,
    NativePreserved,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyAssistantOrderCase {
    pub name: String,
    pub order_mode: LeanAssistantOrderMode,
    pub input: Vec<LeanPromptAssemblyItem>,
    pub expected: Vec<LeanPromptAssemblyItem>,
    pub expected_twice: Vec<LeanPromptAssemblyItem>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanPromptAssemblyRow {
    pub role: String,
    pub kind: String,
    pub call_ids: Vec<u64>,
    pub content: Vec<LeanPromptAssemblyItem>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyModeSanitizeCase {
    pub name: String,
    pub order_mode: LeanAssistantOrderMode,
    pub input: Vec<LeanPromptAssemblyRow>,
    pub expected: Vec<LeanPromptAssemblyRow>,
    pub expected_twice: Vec<LeanPromptAssemblyRow>,
    pub association: Vec<LeanPromptAssemblyRow>,
    pub association_indices: Vec<Vec<usize>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanPromptAssemblySplit {
    pub index: usize,
    pub expected: Vec<LeanPromptAssemblyRow>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanPromptAssemblySanitizeCase {
    pub name: String,
    pub input: Vec<LeanPromptAssemblyRow>,
    pub expected: Vec<LeanPromptAssemblyRow>,
    pub expected_twice: Vec<LeanPromptAssemblyRow>,
    pub splits: Vec<LeanPromptAssemblySplit>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanPromptAssemblyLayerCase {
    pub name: String,
    pub skill_count: usize,
    pub summary_count: usize,
    pub conversation_len: usize,
    pub slots: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanPromptAssemblyRepairCase {
    pub name: String,
    pub input: String,
    pub expected: String,
    pub expected_twice: String,
    pub payload_only: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanPromptAssemblyBudgetCase {
    pub name: String,
    pub context_window: usize,
    pub max_output_tokens: usize,
    pub threshold_basis_points: usize,
    pub configured_threshold_budget: usize,
    pub prompt_tokens: usize,
    pub request_tokens: usize,
    pub effective_input_budget: usize,
    pub compaction_input_budget: usize,
    pub effective_output_tokens: usize,
    pub should_compact: bool,
    pub provider_safe: bool,
    pub can_dispatch: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanPromptAssemblyTurnBudgetCase {
    pub name: String,
    pub context_window: usize,
    pub max_output_tokens: usize,
    pub threshold_basis_points: usize,
    pub configured_threshold_budget: usize,
    pub effective_input_budget: usize,
    pub compaction_input_budget: usize,
    pub turn_input_tokens: Vec<usize>,
    pub turn_output_tokens: Vec<usize>,
    pub turn_should_compact: Vec<bool>,
    pub turn_can_dispatch: Vec<bool>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct LeanContextAnchor {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated_input_tokens: usize,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct LeanReductionAdmission {
    pub input_tokens: usize,
    pub input_budget: usize,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanPromptAssemblyCompactionTriggerCase {
    pub name: String,
    pub context_window: usize,
    pub max_output_tokens: usize,
    pub threshold_basis_points: u64,
    pub prior_basis_points: u64,
    pub seeded_observations: Vec<u64>,
    pub observed: Vec<(u64, usize)>,
    pub anchor: Option<LeanContextAnchor>,
    pub invalidate: bool,
    pub raw_input_tokens: usize,
    pub ratio_basis_points: u64,
    pub calibrated_tokens: usize,
    pub anchored_tokens: Option<usize>,
    pub context_tokens: usize,
    pub compaction_input_budget: usize,
    pub admission: Option<LeanReductionAdmission>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanPromptAssemblyRetentionCase {
    pub name: String,
    pub configured_keep_recent: usize,
    pub effective_input_budget: usize,
    pub fixed_input: usize,
    pub available_input: usize,
    pub retention_target: usize,
    pub summary_max_output: usize,
    pub effective_summary_output: usize,
    pub rolling_summary_input_budget: usize,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanPromptAssemblyClaudeMapCase {
    pub name: String,
    pub surface: Vec<String>,
    pub blocks: Vec<String>,
    pub outcome: String,
    pub ids: Vec<u64>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanPromptAssemblyClaudeBodyCase {
    pub name: String,
    pub model: String,
    pub first_user_text: String,
    pub fingerprint_placeholder: String,
    pub prompt_id: String,
    pub attribution: String,
    pub preamble: Option<String>,
    pub rows: Vec<String>,
    pub tools: Vec<String>,
    pub system: Vec<String>,
    pub system_marked: Vec<bool>,
    pub tools_present: bool,
    pub supported_efforts: Option<Vec<String>>,
    pub effort: Option<String>,
    pub selected_effort: Option<String>,
    pub facts: LeanPromptAssemblyClaudeFieldFacts,
    pub request: LeanPromptAssemblyClaudeFieldRequest,
    pub max_tokens: u64,
    pub thinking: LeanPromptAssemblyClaudeFieldThinking,
    pub temperature: Option<u64>,
    pub context_management: bool,
    pub body_effort: Option<String>,
    pub diagnostics: LeanPromptAssemblyClaudeFieldDiagnostics,
    pub per_turn_effort: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyClaudeFieldFacts {
    pub adaptive: bool,
    pub always_thinks: bool,
    pub thinking_supported: bool,
    pub rejects_disabled: bool,
    pub accepts_temperature: bool,
    pub effort_capable: bool,
    pub max_effort: bool,
    pub xhigh_effort: bool,
    pub caps_effort_when_disabled: bool,
    pub per_turn_effort: bool,
    pub output_default: u64,
    pub output_upper: u64,
    pub default_effort: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyClaudeFieldThinkingConfig {
    pub kind: String,
    pub mechanical: bool,
    pub budget: Option<u64>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyClaudeFieldRequest {
    pub thinking: LeanPromptAssemblyClaudeFieldThinkingConfig,
    pub max_tokens_override: Option<u64>,
    pub requested_effort: Option<String>,
    pub first_party: bool,
    pub first_party_default_url: bool,
    pub experimental_betas_disabled: bool,
    pub agentic: bool,
    pub fork: bool,
    pub display_updates: bool,
    pub context_management_beta: bool,
    pub per_turn_control_beta: bool,
    pub previous_message_id: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyClaudeFieldThinking {
    pub kind: String,
    pub budget: Option<u64>,
    pub updates: bool,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyClaudeFieldDiagnostics {
    pub present: bool,
    pub previous_message_id: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanPromptAssemblyClaudeStreamCase {
    pub name: String,
    pub surface: Vec<String>,
    pub events: Vec<String>,
    pub outcome: String,
    pub calls: Vec<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanPromptAssemblyClaudeThinkingStreamCase {
    pub name: String,
    pub surface: Vec<String>,
    pub events: Vec<LeanClaudeStreamEvent>,
    pub outcome: String,
    pub steps: Vec<LeanClaudeContentStep>,
    pub content: Vec<LeanClaudeStreamBlock>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanPromptAssemblyServiceTierCase {
    pub family: String,
    #[serde(deserialize_with = "required_nullable")]
    pub requested: Option<String>,
    pub allowed: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub wire: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanPromptAssemblyResponsesStorageCase {
    pub name: String,
    pub family: String,
    pub wire: String,
    pub endpoint: String,
    #[serde(deserialize_with = "required_nullable")]
    pub store: Option<bool>,
    pub encrypted_include: bool,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanPromptAssemblyResponsesEffortCase {
    pub name: String,
    pub endpoint: String,
    #[serde(deserialize_with = "required_nullable")]
    pub advertised: Option<Vec<String>>,
    #[serde(deserialize_with = "required_nullable")]
    pub requested: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub expected: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanClaudeStreamEvent {
    pub kind: String,
    pub value: Option<String>,
    pub id: Option<u64>,
    pub name: Option<String>,
    pub input: Option<String>,
    pub index: Option<u64>,
    pub fragment: Option<String>,
    pub data: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanClaudeReasoningPart {
    pub kind: String,
    pub payload: String,
    pub signature: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanClaudeStreamBlock {
    pub kind: String,
    pub value: Option<String>,
    pub parts: Option<Vec<LeanClaudeReasoningPart>>,
    pub id: Option<u64>,
    pub name: Option<String>,
    pub arguments: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanClaudeContentStep {
    #[serde(deserialize_with = "required_nullable")]
    pub provisional_thinking: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub provisional_signature: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub provisional_redacted: Option<String>,
    pub sealed: Vec<LeanClaudeStreamBlock>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanPromptAssemblyClaudeReplayCase {
    pub name: String,
    pub blocks: Vec<LeanClaudeReplayInputBlock>,
    pub outcome: String,
    pub replay: Vec<LeanClaudeReplayBlock>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LeanClaudeReplayOrigin {
    ClaudeSubscription,
    AcceptedProvider,
    RecoveredProvider,
    Foreign,
    Missing,
    Ambiguous,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanClaudeReasoningWitness {
    pub block_index: usize,
    pub parts: Vec<LeanClaudeReplayInputPart>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyClaudeCheckpointCase {
    pub name: String,
    pub required: Vec<LeanCanonicalCoordinate>,
    pub rows: Vec<LeanClaudeTaggedReplayRow>,
    pub split: usize,
    pub carrier_ids: Vec<String>,
    pub resolutions: Vec<LeanClaudeCheckpointResolution>,
    pub outcome: String,
    pub prefix_rows: Vec<LeanClaudeTaggedReplayRow>,
    pub retained: Vec<LeanClaudeTaggedReplayRow>,
    pub replay: Vec<Vec<LeanClaudeReplayBlock>>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct LeanProtectedReplayCompactionCase {
    pub name: String,
    pub message_count: usize,
    pub raw_index: usize,
    pub required: Vec<LeanCanonicalCoordinate>,
    pub rows: Vec<LeanClaudeTaggedReplayRow>,
    pub ceiling_error: String,
    #[serde(deserialize_with = "required_nullable")]
    pub ceiling: Option<usize>,
    #[serde(deserialize_with = "required_nullable")]
    pub selected_split: Option<usize>,
    pub outcome: String,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanClaudeTaggedReplayRow {
    #[serde(deserialize_with = "required_nullable")]
    pub source: Option<LeanCanonicalCoordinate>,
    #[serde(deserialize_with = "required_nullable")]
    pub physical_header: Option<String>,
    pub block_indices: Vec<usize>,
    pub blocks: Vec<LeanClaudeReplayInputBlock>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanClaudeCheckpointResolution {
    pub tag: LeanCanonicalCoordinate,
    pub origin: LeanClaudeReplayOrigin,
    pub issuer_family: String,
    pub issuer_endpoint: String,
    pub wire: LeanReasoningReplayWire,
    pub physical_header: String,
    pub complete: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub captured: Option<Vec<LeanReplayFlatItem>>,
    pub expected_reasoning: Vec<LeanClaudeReasoningWitness>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LeanReplayFlatKind {
    Ordinary,
    Reasoning,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanReplayFlatItem {
    pub kind: LeanReplayFlatKind,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LeanReasoningReplayWire {
    ClaudeMessages,
    Responses,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyReasoningSuffixCase {
    pub name: String,
    pub issuer_family: String,
    pub issuer_endpoint: String,
    pub wire: LeanReasoningReplayWire,
    pub rows: Vec<LeanClaudeTaggedReplayRow>,
    pub resolutions: Vec<LeanClaudeCheckpointResolution>,
    pub stage: Vec<LeanClaudeTaggedReplayRow>,
    pub assembled: Vec<LeanReplayFlatItem>,
    pub replay: Vec<LeanClaudeTaggedReplayRow>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyReplayShapeCase {
    pub name: String,
    pub source: LeanClaudeTaggedReplayRow,
    pub retained_indices: Vec<usize>,
    pub outcome: String,
    pub shaped: Option<LeanClaudeTaggedReplayRow>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanPromptAssemblyReplayPrefixCase {
    pub name: String,
    pub captured: Vec<LeanReplayFlatItem>,
    pub current: Vec<LeanReplayFlatItem>,
    pub ordinary_equal: bool,
    pub reasoning_suffix: bool,
    pub leading_removal: bool,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanClaudeReplayInputBlock {
    pub kind: String,
    pub payload: Option<Vec<u8>>,
    pub parts: Option<Vec<LeanClaudeReplayInputPart>>,
    pub id: Option<String>,
    pub doc_id: Option<u64>,
    pub call_id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<Vec<u8>>,
    pub signature: Option<String>,
    pub additional_params: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct LeanClaudeReplayInputPart {
    pub kind: String,
    pub payload: Vec<u8>,
    pub signature: Option<String>,
}

pub fn native_claude_reasoning_parts(
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

pub fn native_claude_replay_content(
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
                    raw_arguments: None,
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
pub struct LeanClaudeReplayBlock {
    pub kind: String,
    pub payload: Option<Vec<u8>>,
    pub signature: Option<String>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<Vec<u8>>,
}

impl LeanClaudeReplayBlock {
    pub fn native_json(&self) -> serde_json::Value {
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
pub struct LeanAggregateTokenBudgetCase {
    pub request_doc_id: String,
    pub prior_request_doc_ids: Vec<String>,
    pub prior_call_kinds: Vec<String>,
    pub name: String,
    pub limit: u64,
    pub used: u64,
    pub prior_prompt_tokens: Vec<u64>,
    pub prior_completion_tokens: Vec<u64>,
    pub input_tokens: u64,
    pub configured_max_output_tokens: u64,
    pub reported_input_tokens: u64,
    pub reported_output_tokens: u64,
    pub reported_total_tokens: u64,
    pub usage_present: bool,
    pub terminal_valid: bool,
    pub effective_output_tokens: u64,
    pub can_dispatch: bool,
    pub charged_tokens: u64,
    pub charge_result: String,
    pub next_used: Option<u64>,
    pub post_charge_action: String,
}
