//! Generated-case structs for the Lean `SelfConfig` model
//! (`proofs/Proofs/SelfConfig/`): per-target field tables and patch-merge
//! witness cases consumed by `tests/conformance/self_config.rs`. The tables
//! describe the canonical configuration collections (`ConfigDocuments`), with
//! the nested `Tools.self_config` group carried as one field value.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanSelfConfigFieldTable {
    pub(crate) collection: String,
    pub(crate) unique_field: String,
    pub(crate) category: String,
    pub(crate) all_fields: Vec<String>,
    pub(crate) writable_fields: Vec<String>,
    pub(crate) protected_fields: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanSelfConfigFieldValue {
    pub(crate) field: String,
    pub(crate) value: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanSelfConfigPatchEntry {
    pub(crate) field: String,
    /// `"set"` or `"clear"`.
    pub(crate) action: String,
    pub(crate) value: Option<String>,
}

/// A backend a profile row can select; `backend_id` and `auth` are canonical
/// JSON text.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanSelfConfigBackend {
    pub(crate) backend_id: String,
    pub(crate) provider_kind: String,
    pub(crate) auth: String,
}

/// Operator grants the invoking agent holds (`SelfConfig.Grants`).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanSelfConfigGrants {
    pub(crate) pack_install: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanSelfConfigCase {
    pub(crate) name: String,
    pub(crate) collection: String,
    pub(crate) category: String,
    pub(crate) guarded: bool,
    pub(crate) validates: bool,
    pub(crate) held_grants: LeanSelfConfigGrants,
    /// The Tools document the row's Context or Behavior selects before and
    /// after the patch (Lean `rowResolve`), or null when it selects none.
    #[serde(deserialize_with = "super::required_nullable")]
    pub(crate) selected_before: Option<Vec<LeanSelfConfigFieldValue>>,
    #[serde(deserialize_with = "super::required_nullable")]
    pub(crate) selected_after: Option<Vec<LeanSelfConfigFieldValue>>,
    pub(crate) doc: Vec<LeanSelfConfigFieldValue>,
    pub(crate) patch: Vec<LeanSelfConfigPatchEntry>,
    pub(crate) admissible: bool,
    pub(crate) accepted: bool,
    pub(crate) result: Vec<LeanSelfConfigFieldValue>,
    pub(crate) protected_preserved: bool,
    pub(crate) containment_holds: bool,
    pub(crate) unchanged_on_reject: bool,
    pub(crate) control_kept_after_accept: bool,
    #[serde(default)]
    pub(crate) backends: Vec<LeanSelfConfigBackend>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanAgentCatalogEntry {
    pub(crate) agent_id: String,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanAgentCreateInput {
    pub(crate) display_name: String,
    pub(crate) system_prompt: String,
    pub(crate) inference_profile_id: String,
    pub(crate) clone_from: String,
}

/// Decision inputs shared by `agent_decision_cases` and
/// `agent_materialization_cases`.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanAgentDecisionInputs {
    pub(crate) name: String,
    pub(crate) agents: Vec<LeanAgentCatalogEntry>,
    pub(crate) protected_ids: Vec<String>,
    pub(crate) default_id: Option<String>,
    pub(crate) published_profiles: Vec<String>,
    pub(crate) operation: String,
    pub(crate) target: String,
    pub(crate) make_default: bool,
    pub(crate) create_input: Option<LeanAgentCreateInput>,
    pub(crate) edit_patch: Option<Vec<LeanSelfConfigPatchEntry>>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanAgentDecisionCase {
    #[serde(flatten)]
    pub(crate) inputs: LeanAgentDecisionInputs,
    pub(crate) accepted: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanAgentResolvedSession {
    pub(crate) instructions: String,
    pub(crate) skill_ids: Vec<String>,
    pub(crate) tool_names: Vec<String>,
    pub(crate) backend_id: String,
    pub(crate) model: String,
    pub(crate) effort: Option<String>,
}

/// Candidate registry rows under the Lean `Configuration` field names.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCandidateAgent {
    pub(crate) agent_id: String,
    pub(crate) context_id: Option<String>,
    pub(crate) inference_profile_id: String,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCandidateContext {
    pub(crate) context_id: String,
    pub(crate) system_prompt: String,
    pub(crate) skill_ids: Vec<String>,
    pub(crate) tool_names: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCandidateProfile {
    pub(crate) profile_id: String,
    pub(crate) backend_id: String,
    pub(crate) model_name: String,
    pub(crate) reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCandidateBackend {
    pub(crate) backend_id: String,
    pub(crate) enabled: bool,
}

/// The serialized `CandidateFixture`: one node scope and its registry documents.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanAgentCandidate {
    pub(crate) node_did: String,
    pub(crate) agents: Vec<LeanCandidateAgent>,
    pub(crate) contexts: Vec<LeanCandidateContext>,
    pub(crate) inference_profiles: Vec<LeanCandidateProfile>,
    pub(crate) inference_backends: Vec<LeanCandidateBackend>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanToolFlagCase {
    pub(crate) existing: bool,
    pub(crate) requested: Option<bool>,
    pub(crate) selected: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanGraphPresentationCase {
    pub(crate) requested: bool,
    pub(crate) self_config: bool,
    pub(crate) pack_install: bool,
    pub(crate) presented: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanSelfConfigSelectionCases {
    pub(crate) tool_flag_cases: Vec<LeanToolFlagCase>,
    pub(crate) graph_presentation_cases: Vec<LeanGraphPresentationCase>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanAgentMaterializationCase {
    #[serde(flatten)]
    pub(crate) inputs: LeanAgentDecisionInputs,
    pub(crate) candidate: LeanAgentCandidate,
    pub(crate) session: Option<LeanAgentResolvedSession>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanSiblingToolsCase {
    pub(crate) owner_matches: bool,
    pub(crate) protected: bool,
    pub(crate) shared_context: bool,
    pub(crate) shared_tools: bool,
    pub(crate) requested_network: Option<String>,
    pub(crate) existing_network: String,
    pub(crate) admitted: bool,
    pub(crate) result_network: String,
}
