use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanToolOutputProjectionCase {
    pub(crate) name: String,
    pub(crate) document: u64,
    pub(crate) segments: Vec<LeanCanonicalSegment>,
    pub(crate) expected_state: Option<String>,
    pub(crate) expected_payload: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "witness", deny_unknown_fields)]
pub(crate) enum LeanR4cBackgroundWorkCase {
    #[serde(rename = "r4c.read_tool_output.canonical_source_reconstruction")]
    ReadToolOutputCanonicalSourceReconstruction {
        tool_call_id: String,
        canonical_source: String,
        cases: Vec<LeanToolOutputProjectionCase>,
    },
}

impl LeanR4cBackgroundWorkCase {
    pub(crate) fn witness(&self) -> &'static str {
        match self {
            Self::ReadToolOutputCanonicalSourceReconstruction { .. } => {
                "r4c.read_tool_output.canonical_source_reconstruction"
            }
        }
    }
}

/// Interrupt disposition witness computed by the Lean
/// `Background.Interrupt.interruptTool` for one owned tool-call shape.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanInterruptDispositionCase {
    pub(crate) name: String,
    pub(crate) state: String,
    pub(crate) await_mode: String,
    pub(crate) disposition: String,
    pub(crate) post_state: String,
    pub(crate) post_await_mode: String,
}

/// Paging witness over the retained output window (#937): inputs plus the
/// slice outputs computed from the Lean `Background.ToolOutput.readSlice`
/// model, consumed against `read_retained_output_slice`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanToolOutputPagingCase {
    pub(crate) name: String,
    pub(crate) first_offset: u64,
    pub(crate) retained_len: u64,
    pub(crate) total_bytes: u64,
    pub(crate) offset: u64,
    pub(crate) max_bytes: u64,
    pub(crate) start: u64,
    pub(crate) slice_len: u64,
    pub(crate) next_offset: u64,
    pub(crate) first_available_offset: u64,
    pub(crate) total_bytes_out: u64,
    pub(crate) has_more: bool,
    #[allow(dead_code)]
    pub(crate) theorem: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanR6BackgroundingCase {
    pub(crate) name: String,
    pub(crate) group: String,
    pub(crate) action: String,
    pub(crate) legal: bool,
    pub(crate) pre_live_count: usize,
    pub(crate) max_backgrounded: usize,
    pub(crate) await_mode: String,
    pub(crate) terminal_state: String,
    pub(crate) result: Option<String>,
    pub(crate) reason: Option<String>,
    pub(crate) error_code: Option<String>,
    pub(crate) queue_source: Option<String>,
    pub(crate) queue_key: Option<String>,
    #[serde(default)]
    pub(crate) retry_count: Option<usize>,
    #[serde(default)]
    pub(crate) max_retries: Option<usize>,
    #[serde(default)]
    pub(crate) post_retry_count: Option<usize>,
    /// Lean's opaque numeric redrive source request id (`wake.requestId`).
    #[serde(default)]
    pub(crate) redrive_source_request_id: Option<u64>,
    /// Before/after causal hop: redrive preserves the failed source's hop.
    #[serde(default)]
    pub(crate) pre_depth: Option<u64>,
    #[serde(default)]
    pub(crate) post_depth: Option<u64>,
    /// Before/after retry parent: the successor's parent is the failed source
    /// request itself.
    #[serde(default)]
    pub(crate) pre_parent_request_id: Option<u64>,
    #[serde(default)]
    pub(crate) post_parent_request_id: Option<u64>,
    /// Before/after execution deadline: redrive creates no new deadline.
    #[serde(default)]
    pub(crate) pre_execution_deadline: Option<u64>,
    #[serde(default)]
    pub(crate) post_execution_deadline: Option<u64>,
    #[serde(default)]
    pub(crate) retry_delay_seconds: Option<usize>,
    #[serde(default)]
    pub(crate) is_latest: Option<bool>,
    #[serde(default)]
    pub(crate) goal_status: Option<String>,
    #[serde(default)]
    pub(crate) notification_persisted: Option<bool>,
    #[serde(default)]
    pub(crate) wake_created: Option<bool>,
    #[serde(default)]
    pub(crate) redrive_allowed: Option<bool>,
    /// `ToolOperation` of an executed native-lifecycle row.
    #[serde(default)]
    pub(crate) operation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanBackgroundTheoremWitness {
    pub(crate) theorem_name: String,
    pub(crate) witness_kind: String,
    pub(crate) scenario: String,
    pub(crate) numeric_bound: usize,
    pub(crate) kind_fields: Vec<LeanBackgroundTheoremKindField>,
}

impl LeanBackgroundTheoremWitness {
    pub(crate) fn kind_field(&self, key: &str) -> &str {
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
pub(crate) struct LeanBackgroundTheoremKindField {
    pub(crate) key: String,
    pub(crate) value: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanTranscriptCase {
    pub(crate) name: String,
    pub(crate) group: String,
    pub(crate) action: String,
    pub(crate) action_call_ids: Vec<usize>,
    pub(crate) action_logical_result_ids: Vec<usize>,
    pub(crate) action_payload_hashes: Vec<usize>,
    pub(crate) legal: bool,
    pub(crate) pre_message_count: usize,
    pub(crate) post_message_count: usize,
    pub(crate) pre_tool_call_count: usize,
    pub(crate) post_tool_call_count: usize,
    pub(crate) pre_in_flight_count: usize,
    pub(crate) post_in_flight_count: usize,
    pub(crate) assistant_sequence: usize,
    pub(crate) result_sequence: usize,
    pub(crate) logical_result_id: usize,
    pub(crate) payload_hash: usize,
    pub(crate) expected_pair_closed: bool,
    pub(crate) expected_ordered: bool,
    pub(crate) expected_duplicate_reused_sequence: bool,
    pub(crate) expected_strong_drain: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanCompactionReducerCase {
    pub(crate) name: String,
    pub(crate) group: String,
    pub(crate) reducer: String,
    pub(crate) legal: bool,
    pub(crate) pre_message_count: usize,
    pub(crate) post_message_count: usize,
    pub(crate) preserves_pairs: bool,
    pub(crate) preserves_order: bool,
    pub(crate) gate_open: Option<bool>,
    pub(crate) publication_ready: bool,
    pub(crate) provider_fixpoint: bool,
    pub(crate) turn_boundary: bool,
    pub(crate) safe_to_reduce: bool,
    pub(crate) reducer_is_identity: bool,
    pub(crate) reducer_is_idempotent: bool,
    pub(crate) split_index: usize,
    pub(crate) safe_boundary: usize,
    pub(crate) retained_count: usize,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LeanCompactionCursorCase {
    pub(crate) name: String,
    pub(crate) compacted: usize,
    pub(crate) expected_cursor: Option<usize>,
}
