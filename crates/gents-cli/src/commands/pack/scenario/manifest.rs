use std::collections::BTreeMap;

use serde::Deserialize;

use super::prepare::ScenarioPrepareStep;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ScenarioManifest {
    pub(super) name: String,
    #[serde(default)]
    pub(super) description: String,
    pub(super) init: PackInit,
    pub(super) seed: PackSeed,
    #[serde(default)]
    pub(super) default_prompt: String,
    pub(super) expect: PackExpect,
    #[serde(default = "default_timeout")]
    pub(super) await_timeout_secs: u64,
    #[serde(default)]
    pub(super) prepare: Vec<ScenarioPrepareStep>,
    #[serde(default)]
    pub(super) allowed_folders: Vec<ScenarioAllowedFolder>,
    #[serde(default)]
    pub(super) dependency_environment: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(skip)]
    pub(super) dependencies: Vec<String>,
    #[serde(skip)]
    pub(super) plugins: Vec<gents::pack::PackPlugin>,

    #[serde(skip)]
    pub(super) config: Option<gents::document_config::PackConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ScenarioAllowedFolder {
    pub(super) path: String,
    #[serde(default)]
    pub(super) access: gents::pack::BindAccess,
}

fn default_timeout() -> u64 {
    240
}

#[derive(Debug, Deserialize)]
pub(super) struct PackInit {
    pub(super) inference_url: String,
    pub(super) model_name: String,
    #[serde(default = "default_tool_package")]
    pub(super) tool_package: String,
    #[serde(default)]
    pub(super) api_key_env_var: Option<String>,
    #[serde(default)]
    pub(super) backend_preset: Option<String>,
    #[serde(default)]
    pub(super) openai_wire_api: Option<String>,
    #[serde(default)]
    pub(super) max_concurrent: Option<i64>,
    #[serde(default)]
    pub(super) tool_root: Option<String>,
    #[serde(default)]
    pub(super) tool_root_env_var: Option<String>,
    #[serde(default)]
    pub(super) tool_root_markers: Vec<String>,
}

fn default_tool_package() -> String {
    "minimal".to_string()
}

#[derive(Debug, Deserialize)]
pub(super) struct PackSeed {
    pub(super) collection: String,
    pub(super) job_id_field: String,
    pub(super) prompt_field: String,
    #[serde(default)]
    pub(super) fields: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct PackExpect {
    pub(super) trigger_ids: Vec<String>,
    #[serde(default)]
    pub(super) trigger_request_counts: BTreeMap<String, usize>,
    #[serde(default)]
    pub(super) trigger_request_count_sources: BTreeMap<String, TriggerRequestCountSource>,
    #[serde(default)]
    pub(super) collection_counts: BTreeMap<String, u64>,
    #[serde(default)]
    pub(super) projections: Vec<String>,
    #[serde(default)]
    pub(super) signed_provenance: bool,
    #[serde(default)]
    pub(super) required_tool_call_trigger_ids: Vec<String>,
    #[serde(default)]
    pub(super) source_edges: Vec<SourceEdgeExpectation>,
    #[serde(default)]
    pub(super) fan_in: Option<FanInExpectation>,
    #[serde(default)]
    pub(super) prompt_tool_contracts: Vec<PromptToolContract>,
    #[serde(default)]
    pub(super) background_completion: Option<BackgroundCompletionExpectation>,
    #[serde(default)]
    pub(super) tool_calls: Vec<ToolCallExpectation>,
    #[serde(default)]
    pub(super) stage_tool_sequences: Vec<StageToolSequenceExpectation>,
    #[serde(default)]
    pub(super) workspace_receipt_paths: Option<WorkspaceReceiptPathExpectation>,
    #[serde(default)]
    pub(super) result_documents: Vec<ResultDocumentExpectation>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WorkspaceReceiptPathExpectation {
    pub(super) work_unit_collection: String,
    pub(super) correlation_field: String,
    pub(super) work_unit_id_field: String,
    pub(super) owned_paths_field: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct TriggerRequestCountSource {
    pub(super) collection: String,
    pub(super) correlation_field: String,
    #[serde(default)]
    pub(super) expected_count_field: String,
    #[serde(default)]
    pub(super) match_field: Option<String>,
    #[serde(default)]
    pub(super) match_value: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ResultDocumentExpectation {
    pub(super) collection: String,
    pub(super) correlation_field: String,
    pub(super) fields: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ToolCallExpectation {
    pub(super) trigger_id: String,
    pub(super) tool_name: String,
    #[serde(default)]
    pub(super) action: Option<String>,
    #[serde(default)]
    pub(super) file: Option<String>,
    #[serde(default)]
    pub(super) symbol: Option<String>,
    #[serde(default)]
    pub(super) result_contains: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct StageToolSequenceExpectation {
    pub(super) trigger_id: String,
    pub(super) boundary_tool_name: String,
    pub(super) max_calls_before_boundary: usize,
    pub(super) max_calls_per_message_before_boundary: usize,
    pub(super) exact_boundary_calls: usize,
    pub(super) allowed_at_or_after_boundary: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct PromptToolContract {
    pub(super) task_id: String,
    pub(super) required_tool_names: Vec<String>,
    #[serde(default)]
    pub(super) required_query_collections: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct FanInExpectation {
    pub(super) member_collection: String,
    pub(super) result_collection: String,
    pub(super) report_collection: String,
    pub(super) correlation_field: String,
    pub(super) expected_count_field: String,
    #[serde(default)]
    pub(super) min_expected_count: Option<usize>,
    #[serde(default)]
    pub(super) max_expected_count: Option<usize>,
    pub(super) consumer_trigger_id: String,
    #[serde(default)]
    pub(super) member_required_fields: Vec<String>,
    #[serde(default)]
    pub(super) verification: Option<VerificationExpectation>,
}

#[derive(Debug, Deserialize)]
pub(super) struct VerificationExpectation {
    pub(super) candidate_collection: String,
    pub(super) decision_collection: String,
    pub(super) summary_collection: String,
    pub(super) confirmed_collection: String,
    pub(super) final_consumer_trigger_id: String,
    pub(super) finding_id_field: String,
    pub(super) verdict_field: String,
    pub(super) evidence_field: String,
    pub(super) confirmed_count_field: String,
    pub(super) refuted_count_field: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct BackgroundCompletionExpectation {
    pub(super) min_completed_caused_requests: usize,
    pub(super) min_completed_wakes: usize,
    pub(super) min_acknowledged_notifications: usize,
    #[serde(default)]
    pub(super) max_pending_notifications: usize,
    #[serde(default)]
    pub(super) max_stranded_notifications: usize,
}

#[derive(Debug, Deserialize)]
pub(super) struct SourceEdgeExpectation {
    pub(super) producer_trigger_id: String,
    pub(super) producer_tool_name: String,
    pub(super) consumer_trigger_id: String,
    pub(super) source_collection: String,
}
