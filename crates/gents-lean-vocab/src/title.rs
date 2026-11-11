use super::required_nullable;
use serde::Deserialize;

pub use gents_protocol::request_input::QueueDelivery as LeanTitleQueueDelivery;
pub use gents_protocol::request_input::QueuePosition as LeanTitleQueuePosition;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum LeanRequestPurpose {
    #[serde(rename = "normal")]
    Normal,
    #[serde(rename = "title-audit")]
    TitleAudit,
    #[serde(rename = "access-review")]
    AccessReview,
}

impl From<LeanRequestPurpose> for gents_protocol::request_admission::RequestPurpose {
    fn from(purpose: LeanRequestPurpose) -> Self {
        match purpose {
            LeanRequestPurpose::Normal => Self::Normal,
            LeanRequestPurpose::TitleAudit => Self::TitleAudit,
            LeanRequestPurpose::AccessReview => Self::AccessReview,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleRequestPurposeWireCase {
    pub name: String,
    #[serde(deserialize_with = "required_nullable")]
    pub wire: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub expected_decoded: Option<LeanRequestPurpose>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleRequestAdmissionCase {
    pub name: String,
    pub parent_observed_state: String,
    pub observation_available: bool,
    pub branch_fields_exact: bool,
    pub pending_deadline_absent: bool,
    pub request: LeanTitleRequest,
    pub admission: LeanTitleAdmission,
    #[serde(deserialize_with = "required_nullable")]
    pub runtime_evidence: Option<LeanTitleRuntimeEvidence>,
    pub session_agent: String,
    pub expected_admitted: bool,
    pub expected_claimable: bool,
    pub expected_disposition: String,
    #[serde(deserialize_with = "required_nullable")]
    pub expected_pending_state: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleRequest {
    pub hop: u32,
    pub request_id: String,
    pub purpose: LeanRequestPurpose,
    pub target_node: String,
    pub requester_did: String,
    pub agent_did: String,
    pub session_id: String,
    pub content: String,
    pub input: LeanTitleRequestInput,
    pub model_input_fields_hex: Vec<String>,
    pub created_at: String,
    pub trigger_config_document_id: String,
    pub model_retry_fields_hex: Vec<String>,
    pub model_trigger_fields_hex: Vec<String>,
    pub model_parent_fields_hex: Vec<String>,
    pub model_workspace_fields_hex: Vec<String>,
    pub model_semantic_fields_hex: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleRequestInput {
    pub selected_skill_ids: Vec<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub cwd: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub initial_title: Option<LeanTitleInitialTitle>,
    #[serde(deserialize_with = "required_nullable")]
    pub queue: Option<LeanTitleQueueInput>,
    #[serde(deserialize_with = "required_nullable")]
    pub goal_continuation: Option<LeanTitleGoalContinuation>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleInitialTitle {
    pub text: String,
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleQueueInput {
    pub delivery: LeanTitleQueueDelivery,
    #[serde(deserialize_with = "required_nullable")]
    pub position: Option<LeanTitleQueuePosition>,
    pub source: String,
    pub policy: String,
    #[serde(deserialize_with = "required_nullable")]
    pub key: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub queued_after_request_id: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub interrupted_request_id: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub background_completion_wake_version: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleGoalContinuation {
    pub sequence: u64,
    pub wrapup: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleAdmission {
    pub kind: String,
    pub signer_did: String,
    pub issuer_did: String,
    pub source_request_id: String,
    pub runtime_source_kind: String,
    pub signature_valid: bool,
    pub model_signed_fields_hex: Vec<String>,
    pub model_expected_fields_hex: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleRuntimeEvidence {
    pub source_kind: String,
    pub issuer_did: String,
    pub source_request_id: String,
    pub target_node: String,
    pub target_runtime_attestation_valid: bool,
    pub source_binding_current: bool,
    pub source_document_binding_current: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub title_parent: Option<LeanTitleParentEvidence>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleParentEvidence {
    pub request_id: String,
    pub document_id: String,
    pub node_did: String,
    pub session_id: String,
    pub agent_did: String,
    pub logical_binding_current: bool,
    pub physical_binding_current: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleUsageCase {
    pub name: String,
    pub parent: LeanTitleUsageParent,
    pub rows: Vec<LeanTitleUsageRow>,
    pub signed_purposes: Vec<LeanTitleSignedPurpose>,
    pub title_bindings: Vec<LeanTitleUsageBindingFact>,
    #[serde(deserialize_with = "required_nullable")]
    pub usage_action: Option<LeanTitleUsageAction>,
    pub expected_before: LeanTitleUsageResult,
    pub expected_after: LeanTitleUsageResult,
    pub budget_before: LeanUsageBudgetResult,
    pub budget_after: LeanUsageBudgetResult,
    pub goal_before: LeanUsageBudgetResult,
    pub goal_after: LeanUsageBudgetResult,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleUsageParent {
    pub physical: u64,
    pub logical: u64,
    pub node: u64,
    pub session: u64,
    pub state: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleUsageRow {
    pub key: u64,
    pub call_id: u64,
    pub request_physical: u64,
    pub request_logical: u64,
    pub backend: String,
    pub state: String,
    #[serde(deserialize_with = "required_nullable")]
    pub terminal_stamp: Option<u64>,
    #[serde(deserialize_with = "required_nullable")]
    pub usage: Option<LeanTitleUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleSignedPurpose {
    pub physical: u64,
    pub purpose: LeanRequestPurpose,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleUsageBindingFact {
    pub physical: u64,
    pub binding: LeanTitleUsageBinding,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleUsageBinding {
    pub purpose: LeanRequestPurpose,
    pub physical: u64,
    pub logical: u64,
    pub parent_physical: u64,
    pub parent_logical: u64,
    pub node: u64,
    pub session: u64,
    pub authenticated: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleUsageAction {
    pub call_id: u64,
    pub usage: LeanTitleUsage,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanTitleUsageResult {
    Ok {
        normal_public: LeanTitleUsage,
        parent_inclusive_audit: LeanTitleUsage,
    },
    Error {
        error: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanUsageBudgetResult {
    Ok { used: u64 },
    Error { error: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleAdmissionJoinCase {
    pub name: String,
    pub admission_case: String,
    pub observation_available: bool,
    pub row: LeanTitleJoinRow,
    pub world: LeanTitleJoinWorld,
    pub generation: u64,
    pub duration: u64,
    pub deadline: u64,
    pub now: u64,
    #[serde(deserialize_with = "required_nullable")]
    pub expected_activation: Option<LeanTitleJoinBinding>,
    pub expected_claimed: bool,
    pub expected_lease_active: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub expected_claimed_binding: Option<LeanTitleJoinBinding>,
    #[serde(deserialize_with = "required_nullable")]
    pub expected_queue_active: Option<String>,
    pub expected_queue_unchanged: bool,
    pub expected_own_physical: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleJoinRow {
    pub physical_request: String,
    pub logical_request: String,
    pub model_signed_fields_hex: Vec<String>,
    pub physical_binding_current: bool,
    pub branch_fields_exact: bool,
    pub pending_deadline_absent: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleJoinWorld {
    pub own_physical: String,
    pub model_own_physical: String,
    pub purpose: LeanRequestPurpose,
    pub session: String,
    pub node_did: String,
    pub queue_scope_node: String,
    pub queue_scope_session: String,
    #[serde(deserialize_with = "required_nullable")]
    pub queue_active: Option<String>,
    pub lease_pending: bool,
    pub lease_vacant: bool,
    pub lease_now: u64,
    pub claimed_absent: bool,
    pub terminal_selection_absent: bool,
    pub gate_actor: u64,
    pub gate_phase: String,
    pub gate_independent: bool,
    pub gate_sibling_waiting: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTitleJoinBinding {
    pub purpose: LeanRequestPurpose,
    pub physical_request: String,
    pub logical_request: String,
    pub parent_physical: String,
    pub parent_logical: String,
    pub node: String,
    pub session: String,
    pub authenticated: bool,
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::{required_nullable, LeanRequestPurpose};

    #[test]
    fn generated_request_purpose_wire_cases_match_native_owner() {
        use gents_protocol::request_admission::RequestPurpose;

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct PurposeField {
            purpose: RequestPurpose,
        }

        let cases = super::super::lean_title_request_purpose_wire_cases();
        assert!(!cases.is_empty());
        for case in cases {
            let expected = case.expected_decoded.map(RequestPurpose::from);
            let parsed = case
                .wire
                .as_deref()
                .and_then(|wire| RequestPurpose::try_from(wire).ok());
            assert_eq!(parsed, expected, "{}: native wire decoder", case.name);
            let input = match &case.wire {
                Some(wire) => serde_json::json!({ "purpose": wire }),
                None => serde_json::json!({}),
            };
            let decoded = serde_json::from_value::<PurposeField>(input)
                .ok()
                .map(|row| row.purpose);
            assert_eq!(
                decoded, expected,
                "{}: required signed field shape",
                case.name
            );
            if let Some(purpose) = decoded {
                assert_eq!(
                    Some(purpose.as_str()),
                    case.wire.as_deref(),
                    "{}",
                    case.name
                );
            }
        }
    }

    #[test]
    fn generated_title_groups_decode_without_native_binding_claim() {
        use super::super::{
            lean_title_admission_join_cases, lean_title_request_admission_cases,
            lean_title_request_purpose_wire_cases, lean_title_usage_cases,
        };

        assert!(!lean_title_request_admission_cases().is_empty());
        assert!(!lean_title_request_purpose_wire_cases().is_empty());
        assert!(!lean_title_usage_cases().is_empty());
        assert!(!lean_title_admission_join_cases().is_empty());
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RequiredNullableControl {
        purpose: LeanRequestPurpose,
        #[serde(deserialize_with = "required_nullable")]
        value: Option<String>,
    }

    #[test]
    fn required_nullable_and_purpose_are_not_missing_field_defaults() {
        assert!(
            serde_json::from_str::<RequiredNullableControl>(r#"{"purpose":"normal"}"#).is_err()
        );
        assert!(serde_json::from_str::<RequiredNullableControl>(r#"{"value":null}"#).is_err());
        let decoded: RequiredNullableControl =
            serde_json::from_str(r#"{"purpose":"title-audit","value":null}"#).unwrap();
        assert_eq!(decoded.purpose, LeanRequestPurpose::TitleAudit);
        assert!(decoded.value.is_none());
        assert!(serde_json::from_str::<RequiredNullableControl>(
            r#"{"purpose":"titleAudit","value":null}"#
        )
        .is_err());
    }
}
