use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct LeanSelfConfigFieldTable {
    pub collection: String,
    pub unique_field: String,
    pub category: String,
    pub all_fields: Vec<String>,
    pub writable_fields: Vec<String>,
    pub protected_fields: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanSelfConfigFieldValue {
    pub field: String,
    pub value: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanSelfConfigPatchEntry {
    pub field: String,
    pub action: String,
    pub value: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanSelfConfigBackend {
    pub backend_id: String,
    pub provider_kind: String,
    pub auth: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanSelfConfigGrants {
    pub pack_install: bool,
    pub policy: bool,
    pub pack_authority: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanSelfConfigCase {
    pub name: String,
    pub collection: String,
    pub category: String,
    pub guarded: bool,
    pub validates: bool,
    pub held_grants: LeanSelfConfigGrants,
    #[serde(deserialize_with = "super::required_nullable")]
    pub selected_before: Option<Vec<LeanSelfConfigFieldValue>>,
    #[serde(deserialize_with = "super::required_nullable")]
    pub selected_after: Option<Vec<LeanSelfConfigFieldValue>>,
    pub doc: Vec<LeanSelfConfigFieldValue>,
    pub patch: Vec<LeanSelfConfigPatchEntry>,
    pub admissible: bool,
    pub accepted: bool,
    pub result: Vec<LeanSelfConfigFieldValue>,
    pub protected_preserved: bool,
    pub containment_holds: bool,
    pub unchanged_on_reject: bool,
    pub control_kept_after_accept: bool,
    #[serde(default)]
    pub backends: Vec<LeanSelfConfigBackend>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanAgentCatalogEntry {
    pub agent_did: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanAgentCreateInput {
    pub display_name: String,
    pub system_prompt: String,
    pub inference_profile_id: String,
    pub clone_from: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanAgentDecisionInputs {
    pub name: String,
    pub agents: Vec<LeanAgentCatalogEntry>,
    pub protected_ids: Vec<String>,
    pub default_id: Option<String>,
    pub published_profiles: Vec<String>,
    pub operation: String,
    pub target: String,
    pub make_default: bool,
    pub create_input: Option<LeanAgentCreateInput>,
    pub edit_patch: Option<Vec<LeanSelfConfigPatchEntry>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanAgentDecisionCase {
    #[serde(flatten)]
    pub inputs: LeanAgentDecisionInputs,
    pub accepted: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanAgentResolvedSession {
    pub instructions: String,
    pub skill_ids: Vec<String>,
    pub tool_names: Vec<String>,
    pub backend_id: String,
    pub model: String,
    pub effort: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCandidateAgent {
    pub agent_did: String,
    pub creation_nonce: String,
    pub context_id: Option<String>,
    pub inference_profile_id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCandidateContext {
    pub context_id: String,
    pub system_prompt: String,
    pub skill_ids: Vec<String>,
    pub tool_names: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCandidateProfile {
    pub profile_id: String,
    pub backend_id: String,
    pub model_name: String,
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCandidateBackend {
    pub backend_id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanAgentCandidate {
    pub node_did: String,
    pub agents: Vec<LeanCandidateAgent>,
    pub contexts: Vec<LeanCandidateContext>,
    pub inference_profiles: Vec<LeanCandidateProfile>,
    pub inference_backends: Vec<LeanCandidateBackend>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanToolFlagCase {
    pub existing: bool,
    pub requested: Option<bool>,
    pub selected: bool,
}

/// A self-config write of a Tools `integrations` value (Lean `JobGuardRow`):
/// `writer` is `"model"` or `"operator"`, `stored_integrations` is null when
/// the stored Tools document has no `integrations`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanJobGuardCase {
    pub name: String,
    pub writer: String,
    pub selected_before: bool,
    pub native_authorized: bool,
    pub native_source_authorized: bool,
    pub add_schema_tool: bool,
    #[serde(deserialize_with = "super::required_nullable")]
    pub stored_integrations: Option<serde_json::Value>,
    pub candidate_integrations: serde_json::Value,
    pub accepted: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSelfConfigSelectionCases {
    pub tool_flag_cases: Vec<LeanToolFlagCase>,
    pub job_guard_cases: Vec<LeanJobGuardCase>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanAgentMaterializationCase {
    #[serde(flatten)]
    pub inputs: LeanAgentDecisionInputs,
    pub candidate: LeanAgentCandidate,
    pub session: Option<LeanAgentResolvedSession>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSiblingToolsCase {
    pub owner_matches: bool,
    pub protected: bool,
    pub shared_context: bool,
    pub shared_tools: bool,
    pub requested_network: Option<String>,
    pub existing_network: String,
    pub admitted: bool,
    pub result_network: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanSelfConfigScopeEditCase {
    pub name: String,
    pub categories: Vec<String>,
    pub category: String,
    pub scope: String,
    pub allowed: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanSelfConfigGroupCase {
    pub name: String,
    pub enabled: bool,
    pub categories: Option<Vec<String>>,
    pub effective: Vec<String>,
    pub may_create_agent: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanSelfConfigScopeOfCase {
    pub name: String,
    pub invoking_agent: String,
    pub target_agent: String,
    pub shared: bool,
    pub scope: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanSelfConfigScopeCases {
    pub categories: Vec<String>,
    pub default_categories: Vec<String>,
    pub edit: Vec<LeanSelfConfigScopeEditCase>,
    pub groups: Vec<LeanSelfConfigGroupCase>,
    pub scope_of: Vec<LeanSelfConfigScopeOfCase>,
}
