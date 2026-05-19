use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "witness", deny_unknown_fields)]
pub(crate) enum LeanR4cBackgroundWorkCase {
    #[serde(rename = "r4c.list_subagents.lineage_rejects")]
    ListSubagentsLineageRejects {
        caller_request_id: String,
        sibling_request_id: String,
        sibling_child_id: String,
        caller_sees_sibling_child: bool,
    },
    #[serde(rename = "r4c.read_subagent_transcript.cursor_advances")]
    ReadTranscriptCursorAdvances {
        child_session_id: String,
        first_since_sequence: usize,
        first_through_sequence: usize,
        first_next_sequence: usize,
        second_since_sequence: usize,
        second_through_sequence: usize,
        no_gap: bool,
        no_overlap: bool,
    },
    #[serde(rename = "r4c.read_subagent_transcript.hides_bridge_rows")]
    ReadTranscriptHidesBridgeRows {
        child_session_id: String,
        bridge_call_id: String,
        rendered_transcript: String,
    },
    #[serde(rename = "r4c.read_tool_output.dispatch_by_state")]
    ReadToolOutputDispatchesByState {
        tool_call_id: String,
        running_source: String,
        terminal_source: String,
        running_payload: String,
        stale_running_payload: String,
        terminal_payload: String,
    },
    #[serde(rename = "r4c.steer_subagent.append_preserves_lineage")]
    SteerAppendPreservesLineage {
        caller_request_id: String,
        child_session_id: String,
        queued_request_id: String,
        caused_by_parent_request_id: String,
        queue_source: String,
        queue_policy: String,
    },
    #[serde(rename = "r4c.steer_subagent.interrupt_composes")]
    SteerInterruptComposes {
        caller_request_id: String,
        child_session_id: String,
        interrupted_active_request_id: String,
        drained_wake_up_request_ids: Vec<String>,
        drained_wake_up_queue_key: String,
        queued_request_id: String,
        queue_interrupted_request_id: String,
    },
}

impl LeanR4cBackgroundWorkCase {
    pub(crate) fn witness(&self) -> &'static str {
        match self {
            Self::ListSubagentsLineageRejects { .. } => "r4c.list_subagents.lineage_rejects",
            Self::ReadTranscriptCursorAdvances { .. } => {
                "r4c.read_subagent_transcript.cursor_advances"
            }
            Self::ReadTranscriptHidesBridgeRows { .. } => {
                "r4c.read_subagent_transcript.hides_bridge_rows"
            }
            Self::ReadToolOutputDispatchesByState { .. } => {
                "r4c.read_tool_output.dispatch_by_state"
            }
            Self::SteerAppendPreservesLineage { .. } => {
                "r4c.steer_subagent.append_preserves_lineage"
            }
            Self::SteerInterruptComposes { .. } => "r4c.steer_subagent.interrupt_composes",
        }
    }
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
    pub(crate) cancel_policy: String,
    pub(crate) child_request_id: Option<String>,
    pub(crate) terminal_state: String,
    pub(crate) result: Option<String>,
    pub(crate) reason: Option<String>,
    pub(crate) error_code: Option<String>,
    pub(crate) queue_source: Option<String>,
    pub(crate) queue_key: Option<String>,
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
pub(crate) struct LeanResponseTransitionCase {
    pub(crate) name: String,
    pub(crate) group: String,
    pub(crate) action: String,
    pub(crate) legal: bool,
    pub(crate) pre_status: String,
    pub(crate) post_status: String,
    pub(crate) pre_live_tail: String,
    pub(crate) post_live_tail: String,
    pub(crate) pre_token_count: usize,
    pub(crate) post_token_count: usize,
    pub(crate) error_reason: Option<String>,
    pub(crate) pre_materialized_seq: Option<usize>,
    pub(crate) post_materialized_seq: Option<usize>,
    pub(crate) expected_request_state: Option<String>,
    pub(crate) expected_request_persistence: Option<String>,
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
    pub(crate) gate_open: bool,
    pub(crate) safe_to_reduce: bool,
    pub(crate) reducer_is_identity: bool,
}
