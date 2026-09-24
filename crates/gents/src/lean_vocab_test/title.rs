use super::required_nullable;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub(crate) enum LeanRequestPurpose {
    #[serde(rename = "normal")]
    Normal,
    #[serde(rename = "title-audit")]
    TitleAudit,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleRequestPurposeWireCase {
    pub(crate) name: String,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) wire: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) expected_decoded: Option<LeanRequestPurpose>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleRequestAdmissionCase {
    pub(crate) name: String,
    pub(crate) parent_observed_state: String,
    pub(crate) observation_available: bool,
    pub(crate) branch_fields_exact: bool,
    pub(crate) pending_deadline_absent: bool,
    pub(crate) request: LeanTitleRequest,
    pub(crate) admission: LeanTitleAdmission,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) runtime_evidence: Option<LeanTitleRuntimeEvidence>,
    pub(crate) session_behavior: String,
    pub(crate) expected_admitted: bool,
    pub(crate) expected_claimable: bool,
    pub(crate) expected_disposition: String,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) expected_pending_state: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleRequest {
    pub(crate) request_id: String,
    pub(crate) purpose: LeanRequestPurpose,
    pub(crate) target_agent: String,
    pub(crate) requester_did: String,
    pub(crate) behavior_id: String,
    pub(crate) session_id: String,
    pub(crate) content: String,
    pub(crate) input: LeanTitleRequestInput,
    pub(crate) model_input_fields_hex: Vec<String>,
    pub(crate) created_at: String,
    pub(crate) trigger_config_document_id: String,
    pub(crate) model_retry_fields_hex: Vec<String>,
    pub(crate) model_trigger_fields_hex: Vec<String>,
    pub(crate) model_parent_fields_hex: Vec<String>,
    pub(crate) model_workspace_fields_hex: Vec<String>,
    pub(crate) model_semantic_fields_hex: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleRequestInput {
    pub(crate) selected_skill_ids: Vec<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) cwd: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) initial_title: Option<LeanTitleInitialTitle>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) queue: Option<LeanTitleQueueInput>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) goal_continuation: Option<LeanTitleGoalContinuation>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleInitialTitle {
    pub(crate) text: String,
    pub(crate) source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleQueueInput {
    pub(crate) source: String,
    pub(crate) policy: String,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) key: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) queued_after_request_id: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) interrupted_request_id: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) background_completion_wake_version: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleGoalContinuation {
    pub(crate) sequence: u64,
    pub(crate) wrapup: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleAdmission {
    pub(crate) kind: String,
    pub(crate) signer_did: String,
    pub(crate) issuer_did: String,
    pub(crate) source_request_id: String,
    pub(crate) runtime_source_kind: String,
    pub(crate) bridge_author_did: String,
    pub(crate) signature_valid: bool,
    pub(crate) model_signed_fields_hex: Vec<String>,
    pub(crate) model_expected_fields_hex: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleRuntimeEvidence {
    pub(crate) source_kind: String,
    pub(crate) issuer_did: String,
    pub(crate) source_request_id: String,
    pub(crate) bridge_author_did: String,
    pub(crate) target_agent: String,
    pub(crate) target_runtime_attestation_valid: bool,
    pub(crate) source_binding_current: bool,
    pub(crate) source_document_binding_current: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) title_parent: Option<LeanTitleParentEvidence>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleParentEvidence {
    pub(crate) request_id: String,
    pub(crate) document_id: String,
    pub(crate) agent_did: String,
    pub(crate) session_id: String,
    pub(crate) behavior_id: String,
    pub(crate) logical_binding_current: bool,
    pub(crate) physical_binding_current: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleUsageCase {
    pub(crate) name: String,
    pub(crate) parent: LeanTitleUsageParent,
    pub(crate) rows: Vec<LeanTitleUsageRow>,
    pub(crate) signed_purposes: Vec<LeanTitleSignedPurpose>,
    pub(crate) title_bindings: Vec<LeanTitleUsageBindingFact>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) usage_action: Option<LeanTitleUsageAction>,
    pub(crate) expected_before: LeanTitleUsageResult,
    pub(crate) expected_after: LeanTitleUsageResult,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleUsageParent {
    pub(crate) physical: u64,
    pub(crate) logical: u64,
    pub(crate) agent: u64,
    pub(crate) session: u64,
    pub(crate) state: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleUsageRow {
    pub(crate) key: u64,
    pub(crate) call_id: u64,
    pub(crate) request_physical: u64,
    pub(crate) request_logical: u64,
    pub(crate) backend: String,
    pub(crate) state: String,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) terminal_stamp: Option<u64>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) usage: Option<LeanTitleUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleUsage {
    pub(crate) prompt_tokens: u64,
    pub(crate) completion_tokens: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleSignedPurpose {
    pub(crate) physical: u64,
    pub(crate) purpose: LeanRequestPurpose,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleUsageBindingFact {
    pub(crate) physical: u64,
    pub(crate) binding: LeanTitleUsageBinding,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleUsageBinding {
    pub(crate) physical: u64,
    pub(crate) logical: u64,
    pub(crate) parent_physical: u64,
    pub(crate) parent_logical: u64,
    pub(crate) agent: u64,
    pub(crate) session: u64,
    pub(crate) authenticated: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleUsageAction {
    pub(crate) call_id: u64,
    pub(crate) usage: LeanTitleUsage,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LeanTitleUsageResult {
    Ok {
        normal_public: LeanTitleUsage,
        parent_inclusive_audit: LeanTitleUsage,
    },
    Error {
        error: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleAdmissionJoinCase {
    pub(crate) name: String,
    pub(crate) admission_case: String,
    pub(crate) observation_available: bool,
    pub(crate) row: LeanTitleJoinRow,
    pub(crate) world: LeanTitleJoinWorld,
    pub(crate) generation: u64,
    pub(crate) duration: u64,
    pub(crate) deadline: u64,
    pub(crate) now: u64,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) expected_activation: Option<LeanTitleJoinBinding>,
    pub(crate) expected_claimed: bool,
    pub(crate) expected_lease_active: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) expected_claimed_binding: Option<LeanTitleJoinBinding>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) expected_queue_active: Option<String>,
    pub(crate) expected_queue_unchanged: bool,
    pub(crate) expected_own_physical: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleJoinRow {
    pub(crate) physical_request: String,
    pub(crate) logical_request: String,
    pub(crate) model_signed_fields_hex: Vec<String>,
    pub(crate) physical_binding_current: bool,
    pub(crate) branch_fields_exact: bool,
    pub(crate) pending_deadline_absent: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleJoinWorld {
    pub(crate) own_physical: String,
    pub(crate) model_own_physical: String,
    pub(crate) purpose: LeanRequestPurpose,
    pub(crate) session: String,
    pub(crate) principal: String,
    pub(crate) queue_scope_agent: String,
    pub(crate) queue_scope_session: String,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) queue_active: Option<String>,
    pub(crate) lease_pending: bool,
    pub(crate) lease_vacant: bool,
    pub(crate) lease_now: u64,
    pub(crate) claimed_absent: bool,
    pub(crate) terminal_selection_absent: bool,
    pub(crate) gate_actor: u64,
    pub(crate) gate_phase: String,
    pub(crate) gate_independent: bool,
    pub(crate) gate_sibling_waiting: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTitleJoinBinding {
    pub(crate) physical_request: String,
    pub(crate) logical_request: String,
    pub(crate) parent_physical: String,
    pub(crate) parent_logical: String,
    pub(crate) agent: String,
    pub(crate) session: String,
    pub(crate) authenticated: bool,
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
            let expected = case.expected_decoded.map(|purpose| match purpose {
                LeanRequestPurpose::Normal => RequestPurpose::Normal,
                LeanRequestPurpose::TitleAudit => RequestPurpose::TitleAudit,
            });
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
