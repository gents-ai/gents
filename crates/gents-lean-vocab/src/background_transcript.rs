use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanToolOutputProjectionCase {
    pub name: String,
    pub document: u64,
    pub segments: Vec<LeanCanonicalSegment>,
    pub expected_state: Option<String>,
    pub expected_payload: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "witness", deny_unknown_fields)]
pub enum LeanR4cBackgroundWorkCase {
    #[serde(rename = "r4c.read_tool_output.canonical_source_reconstruction")]
    ReadToolOutputCanonicalSourceReconstruction {
        tool_call_id: String,
        canonical_source: String,
        cases: Vec<LeanToolOutputProjectionCase>,
    },
}

impl LeanR4cBackgroundWorkCase {
    pub fn witness(&self) -> &'static str {
        match self {
            Self::ReadToolOutputCanonicalSourceReconstruction { .. } => {
                "r4c.read_tool_output.canonical_source_reconstruction"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanInterruptDispositionCase {
    pub name: String,
    pub state: String,
    pub await_mode: String,
    pub disposition: String,
    pub post_state: String,
    pub post_await_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanToolOutputPagingCase {
    pub name: String,
    pub first_offset: u64,
    pub retained_len: u64,
    pub total_bytes: u64,
    pub offset: u64,
    pub max_bytes: u64,
    pub start: u64,
    pub slice_len: u64,
    pub next_offset: u64,
    pub first_available_offset: u64,
    pub total_bytes_out: u64,
    pub has_more: bool,
    #[allow(dead_code)]
    pub theorem: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanR6BackgroundingCase {
    pub name: String,
    pub group: String,
    pub action: String,
    pub legal: bool,
    pub pre_live_count: usize,
    pub max_backgrounded: usize,
    pub await_mode: String,
    pub terminal_state: String,
    pub result: Option<String>,
    pub reason: Option<String>,
    pub error_code: Option<String>,
    pub queue_source: Option<String>,
    pub queue_key: Option<String>,
    #[serde(default)]
    pub retry_count: Option<usize>,
    #[serde(default)]
    pub max_retries: Option<usize>,
    #[serde(default)]
    pub post_retry_count: Option<usize>,
    #[serde(default)]
    pub redrive_source_request_id: Option<u64>,
    #[serde(default)]
    pub pre_depth: Option<u64>,
    #[serde(default)]
    pub post_depth: Option<u64>,
    #[serde(default)]
    pub pre_parent_request_id: Option<u64>,
    #[serde(default)]
    pub post_parent_request_id: Option<u64>,
    #[serde(default)]
    pub pre_execution_deadline: Option<u64>,
    #[serde(default)]
    pub post_execution_deadline: Option<u64>,
    #[serde(default)]
    pub retry_delay_seconds: Option<usize>,
    #[serde(default)]
    pub is_latest: Option<bool>,
    #[serde(default)]
    pub goal_status: Option<String>,
    #[serde(default)]
    pub notification_persisted: Option<bool>,
    #[serde(default)]
    pub wake_created: Option<bool>,
    #[serde(default)]
    pub redrive_allowed: Option<bool>,
    #[serde(default)]
    pub operation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanBackgroundTheoremWitness {
    pub theorem_name: String,
    pub witness_kind: String,
    pub scenario: String,
    pub numeric_bound: usize,
    pub kind_fields: Vec<LeanBackgroundTheoremKindField>,
}

impl LeanBackgroundTheoremWitness {
    pub fn kind_field(&self, key: &str) -> &str {
        self.kind_fields
            .iter()
            .find(|field| field.key == key)
            .map(|field| field.value.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "Lean Background theorem witness {:?} omitted kind field {:?}",
                    self.theorem_name, key
                )
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanBackgroundTheoremKindField {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct LeanTranscriptCase {
    pub name: String,
    pub group: String,
    pub action: String,
    pub action_call_ids: Vec<usize>,
    pub action_logical_result_ids: Vec<usize>,
    pub action_payload_hashes: Vec<usize>,
    pub legal: bool,
    pub pre_message_count: usize,
    pub post_message_count: usize,
    pub pre_tool_call_count: usize,
    pub post_tool_call_count: usize,
    pub pre_in_flight_count: usize,
    pub post_in_flight_count: usize,
    pub assistant_sequence: usize,
    pub result_sequence: usize,
    pub logical_result_id: usize,
    pub payload_hash: usize,
    pub expected_pair_closed: bool,
    pub expected_ordered: bool,
    pub expected_duplicate_reused_sequence: bool,
    pub expected_strong_drain: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanCompactionInputRow {
    Ordinary { role: String, text: String },
    AssistantToolCalls { role: String, call_ids: Vec<String> },
    ToolResult { role: String, call_id: String },
}

#[derive(Debug, Deserialize)]
pub struct LeanCompactionReducerCase {
    pub input_rows: Vec<LeanCompactionInputRow>,
    pub projected_prefix_ready: bool,
    pub name: String,
    pub group: String,
    pub reducer: String,
    pub legal: bool,
    pub pre_message_count: usize,
    pub post_message_count: usize,
    pub preserves_pairs: bool,
    pub preserves_order: bool,
    pub gate_open: Option<bool>,
    pub publication_ready: bool,
    pub provider_fixpoint: bool,
    pub turn_boundary: bool,
    pub safe_to_reduce: bool,
    pub reducer_is_identity: bool,
    pub reducer_is_idempotent: bool,
    pub split_index: usize,
    pub safe_boundary: usize,
    pub retained_count: usize,
}

#[derive(Debug, Deserialize)]
pub struct LeanCompactionCursorCase {
    pub name: String,
    pub compacted: usize,
    pub expected_cursor: Option<usize>,
}
