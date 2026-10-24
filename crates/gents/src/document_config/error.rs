use gents_protocol::graphql::GraphqlError;

use crate::template::TemplateError;

use super::eval_definition::LLM_JUDGE_CHECK;
use super::task_validation::MAX_TASK_HOOK_TIMEOUT_SECS;
use crate::runtime_snapshot::MAX_EVENT_TRIGGER_GROUP_DOCS;

/// Validation messages are shown to the model when it edits configuration;
/// keep the wording stable.
#[derive(Debug, thiserror::Error)]
pub enum DocumentConfigError {
    #[error("{violations}")]
    CompactionViolations { violations: String },
    #[error("{violations}")]
    InferenceSamplingViolations { violations: String },
    #[error("{violations}")]
    InferenceExecutionViolations { violations: String },
    #[error("{violations}")]
    InferenceRetryPolicyViolations { violations: String },
    #[error("{violations}")]
    InferenceProfileViolations { violations: String },
    #[error("Tools {tools_id}: {violations}")]
    ToolsViolations {
        tools_id: String,
        violations: String,
    },

    #[error("service {service_id} must contain a positive mcp_port")]
    ServiceMcpPort { service_id: String },
    #[error("service {service_id} requires hostname, tailscale_ip, or lan_ip")]
    ServiceAddress { service_id: String },
    #[error("repository placement {repository_id} must contain a non-empty host_path")]
    RepositoryHostPath { repository_id: String },

    #[error("task {task_id} goal_objective_template must be non-empty when set")]
    TaskGoalObjectiveBlank { task_id: String },
    #[error("task {task_id} goal_token_budget requires goal_objective_template")]
    TaskGoalBudgetWithoutObjective { task_id: String },
    #[error("task {task_id} goal_token_budget must be positive")]
    TaskGoalBudgetNotPositive { task_id: String },
    #[error("task {task_id} has duplicate hook_id {hook_id:?}")]
    TaskDuplicateHook { task_id: String, hook_id: String },
    #[error("task {task_id} hook {hook_id:?} command must be nonempty")]
    TaskHookCommandEmpty { task_id: String, hook_id: String },
    #[error(
        "task {task_id} hook {hook_id:?} timeout_secs must be between 1 and {MAX_TASK_HOOK_TIMEOUT_SECS}"
    )]
    TaskHookTimeout { task_id: String, hook_id: String },
    #[error("task {task_id} {field} failed to parse")]
    TaskTemplateParse {
        task_id: String,
        field: &'static str,
        #[source]
        source: TemplateError,
    },
    #[error("task {task_id} {field} {judgement}")]
    TaskTemplateVocabulary {
        task_id: String,
        field: &'static str,
        judgement: &'static str,
        #[source]
        source: TemplateError,
    },
    #[error("task {task_id} {field} references unavailable template variable {path}")]
    TaskTemplateUnavailableVariable {
        task_id: String,
        field: &'static str,
        path: String,
    },

    #[error("projection ACP binding {binding_id} must contain a non-empty policy_id")]
    ProjectionAcpPolicyBlank { binding_id: String },
    #[error(
        "projection ACP binding {binding_id} staged_policy_id must differ from active policy_id"
    )]
    ProjectionAcpStagedIsActive { binding_id: String },
    #[error(
        "projection ACP binding {binding_id} previous_policy_id must differ from active policy_id"
    )]
    ProjectionAcpPreviousIsActive { binding_id: String },
    #[error(
        "projection ACP binding {binding_id} staged_policy_id must differ from previous_policy_id"
    )]
    ProjectionAcpStagedIsPrevious { binding_id: String },
    #[error("projection ACP binding {binding_id} has invalid projection_id")]
    ProjectionAcpInvalidProjection {
        binding_id: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("parsing ProjectionAcpBinding.resource_map_json")]
    ProjectionResourceMapParse {
        #[source]
        source: serde_json::Error,
    },
    #[error("ProjectionAcpBinding.resource_map_json must map non-empty collection names to non-empty ACP resource names")]
    ProjectionResourceMapBlankEntry,
    #[error("ProjectionAcpBinding.resource_map_json contains unknown runtime collection {collection}; expected one of {expected}")]
    ProjectionResourceMapUnknownCollection {
        collection: String,
        expected: String,
    },

    #[error("tool_name must be non-empty")]
    WriteToolNameBlank,
    #[error("invalid collection {collection:?}: {error}")]
    WriteToolInvalidCollection {
        collection: String,
        error: GraphqlError,
    },
    #[error("invalid field[{index}] name: empty name")]
    WriteToolFieldNameBlank { index: usize },
    #[error("invalid field[{index}] name {name:?}: {error}")]
    WriteToolFieldName {
        index: usize,
        name: String,
        error: GraphqlError,
    },
    #[error("field[{index}] requester_did is node identity and must be runtime-filled")]
    WriteToolRequesterDid { index: usize },
    #[error(
        "collection {collection:?} is protected and cannot be exposed through a datastore tool"
    )]
    ProtectedCollection { collection: String },
    #[error("{field}[{index}] tool_name {name:?} collides with a built-in tool; declared {role} must use a unique name")]
    ToolNameBuiltinCollision {
        field: &'static str,
        index: usize,
        name: String,
        role: &'static str,
    },
    #[error("{field}[{index}] tool_name {name:?} collides with a CLI entry in the same Tools document; each tool must have a unique name")]
    ToolNameCliCollision {
        field: &'static str,
        index: usize,
        name: String,
    },
    #[error("{field}[{index}] tool_name {name:?} collides with another runtime-provided tool; each tool must have a unique name")]
    ToolNameRuntimeCollision {
        field: &'static str,
        index: usize,
        name: String,
    },
    #[error("runtime-filled field `{field_name}` requires an active AgentRequest context")]
    RuntimeFieldNeedsRequest { field_name: String },
    #[error("runtime-filled field `{field_name}` requires a non-empty correlation")]
    RuntimeFieldNeedsCorrelation { field_name: String },
    #[error("runtime-filled field `{field_name}` requires source field `{source_field}` in trigger context")]
    RuntimeFieldNeedsSourceField {
        field_name: String,
        source_field: String,
    },

    #[error("entry {tool_name:?} output_obligation.minimum_writes must be greater than zero and output_obligation.expected_count_field, when present, must name a required model-provided field")]
    OutputObligationMalformed { tool_name: String },
    #[error("duplicate DatastoreToolSurface {surface_id}")]
    DuplicateDatastoreToolSurface { surface_id: String },
    #[error("Tools {tools_id} has an empty datastore_tool_surface_ids entry")]
    ToolsEmptySurfaceId { tools_id: String },
    #[error("Tools {tools_id} lists DatastoreToolSurface {surface_id} more than once")]
    ToolsDuplicateSurfaceLink {
        tools_id: String,
        surface_id: String,
    },
    #[error("Tools {tools_id} references missing same-agent DatastoreToolSurface {surface_id}")]
    ToolsMissingSurface {
        tools_id: String,
        surface_id: String,
    },
    #[error("Tools {tools_id} references disabled DatastoreToolSurface {surface_id}")]
    ToolsDisabledSurface {
        tools_id: String,
        surface_id: String,
    },
    #[error("DatastoreToolSurface {surface_id} has a malformed entry: {error}")]
    SurfaceEntryMalformed {
        surface_id: String,
        error: Box<DocumentConfigError>,
    },
    #[error("duplicate tool_name {tool_name:?} after expanding DatastoreToolSurface {surface_id} for Tools {tools_id}")]
    DuplicateSurfaceToolName {
        tool_name: String,
        surface_id: String,
        tools_id: String,
    },
    #[error("query_tools[{index}] is malformed (tool_name, collection, and at least one projection field are required): tool_name={tool_name:?}, collection={collection:?}, fields={fields}")]
    QueryToolMalformed {
        index: usize,
        tool_name: String,
        collection: String,
        fields: usize,
    },
    #[error(
        "query_tools[{index}] (tool {tool_name:?}) has invalid collection {collection:?}: {error}"
    )]
    QueryToolInvalidCollection {
        index: usize,
        tool_name: String,
        collection: String,
        error: GraphqlError,
    },
    #[error("query_tools[{index}] (tool {tool_name:?}) fields[{field_index}] {field:?} is not a GraphQL name: {error}")]
    QueryToolFieldNotName {
        index: usize,
        tool_name: String,
        field_index: usize,
        field: String,
        error: GraphqlError,
    },
    #[error("query_tools[{index}] (tool {tool_name:?}) has a duplicate projection field {field:?}; each field must be unique")]
    QueryToolDuplicateField {
        index: usize,
        tool_name: String,
        field: String,
    },
    #[error("query_tools[{index}] (tool {tool_name:?}) has a filter_fields[{field_index}] with an empty name")]
    QueryToolFilterBlank {
        index: usize,
        tool_name: String,
        field_index: usize,
    },
    #[error("query_tools[{index}] (tool {tool_name:?}) filter_fields[{field_index}] {name:?} is not a GraphQL name: {error}")]
    QueryToolFilterNotName {
        index: usize,
        tool_name: String,
        field_index: usize,
        name: String,
        error: GraphqlError,
    },
    #[error("query_tools[{index}] (tool {tool_name:?}) filter_fields[{field_index}] {name:?} collides with a reserved query argument")]
    QueryToolFilterReserved {
        index: usize,
        tool_name: String,
        field_index: usize,
        name: String,
    },
    #[error("query_tools[{index}] (tool {tool_name:?}) has a duplicate filter field {name:?}; each filter_fields name must be unique")]
    QueryToolDuplicateFilter {
        index: usize,
        tool_name: String,
        name: String,
    },
    #[error("query_tools[{index}] (tool {tool_name:?}) filter_fields[{field_index}] {name:?} is runtime-filled and cannot be required")]
    QueryToolFilterRequiredFill {
        index: usize,
        tool_name: String,
        field_index: usize,
        name: String,
    },
    #[error("query_tools[{index}] (tool {tool_name:?}) filter_fields[{field_index}] has invalid source_field {source_field:?}: {error}")]
    QueryToolSourceField {
        index: usize,
        tool_name: String,
        field_index: usize,
        source_field: String,
        error: GraphqlError,
    },
    #[error("query_tools has a duplicate tool_name {tool_name:?}; each declared query tool must have a unique tool_name")]
    QueryToolDuplicateName { tool_name: String },

    #[error("event source supports only the created event kind")]
    EventSourceEventKind,
    #[error("per_group source requires correlation_field")]
    PerGroupRequiresCorrelation,
    #[error("expected_count must be a positive count")]
    ExpectedCountNotPositive,
    #[error("expected_count {count} exceeds the group bound {MAX_EVENT_TRIGGER_GROUP_DOCS}")]
    ExpectedCountOutOfBounds { count: usize },
    #[error("count field is not a GraphQL name")]
    CountFieldNotName,
    #[error("group timeout_secs must be positive")]
    GroupTimeoutNotPositive,
    #[error("group timeout_secs exceeds the supported range")]
    GroupTimeoutOutOfRange,
    #[error("group min_count must be positive")]
    GroupMinCountNotPositive,
    #[error("group min_count exceeds the supported range")]
    GroupMinCountOutOfRange,
    #[error("min_count {min_count} must be within 1..={MAX_EVENT_TRIGGER_GROUP_DOCS}")]
    GroupMinCountOutOfBounds { min_count: usize },
    #[error("min_count requires a positive group timeout_secs")]
    MinCountRequiresTimeout,
    #[error("min_count {min_count} exceeds expected_count {expected}")]
    MinCountExceedsExpected { min_count: usize, expected: usize },
    #[error("per_group without a fixed or source-field count requires a positive timeout_secs")]
    PerGroupRequiresCount,
    #[error("expected_count cannot combine a fixed count and a source field")]
    ExpectedCountAndSourceField,
    #[error(transparent)]
    Graphql(#[from] GraphqlError),

    #[error("eval definition requires a definition_id")]
    EvalDefinitionIdBlank,
    #[error("eval definition {id} comparability_version must be at least 1")]
    EvalComparabilityVersion { id: String },
    #[error("eval definition {id} requires at least one case")]
    EvalNoCases { id: String },
    #[error(
        "{owner} fixture file path {path:?} must be a relative path inside the trial workspace"
    )]
    EvalFixturePath { owner: String, path: String },
    #[error("eval definition {id} has a case with no case_id")]
    EvalCaseIdBlank { id: String },
    #[error("eval definition {id} has duplicate case_id {case_id}")]
    EvalDuplicateCase { id: String, case_id: String },
    #[error("eval definition {id} case {case_id} requires at least one stage")]
    EvalCaseNoStages { id: String, case_id: String },
    #[error("eval definition {id} case {case_id} has a goal with an empty collection name")]
    EvalGoalCollectionBlank { id: String, case_id: String },
    #[error("eval definition {id} case {case_id} has a stage with no stage_id")]
    EvalStageIdBlank { id: String, case_id: String },
    #[error("eval definition {id} case {case_id} has duplicate stage_id {stage_id}")]
    EvalDuplicateStage {
        id: String,
        case_id: String,
        stage_id: String,
    },
    #[error("eval definition {id} case {case_id} stage {stage_id} deadline_secs must be positive")]
    EvalStageDeadline {
        id: String,
        case_id: String,
        stage_id: String,
    },
    #[error(
        "eval definition {id} case {case_id} stage {stage_id} needs exactly one of seed or prompt"
    )]
    EvalStageSeedOrPrompt {
        id: String,
        case_id: String,
        stage_id: String,
    },
    #[error("eval definition {id} case {case_id} stage {stage_id} review_previous requires a prompt and a preceding checked stage")]
    EvalReviewPrevious {
        id: String,
        case_id: String,
        stage_id: String,
    },
    #[error("eval definition {id} case {case_id} stage {stage_id} seed collection must be named")]
    EvalSeedCollection {
        id: String,
        case_id: String,
        stage_id: String,
    },
    #[error("eval definition {id} case {case_id} stage {stage_id} continuation requires settle")]
    EvalContinuationRequiresSettle {
        id: String,
        case_id: String,
        stage_id: String,
    },
    #[error("eval definition {id} case {case_id} stage {stage_id} continuation needs a prompt and a max of at least 1")]
    EvalContinuationInvalid {
        id: String,
        case_id: String,
        stage_id: String,
    },
    #[error("eval definition {id} case {case_id} stage {stage_id} continuation until names no documents capture {until:?}")]
    EvalContinuationUntil {
        id: String,
        case_id: String,
        stage_id: String,
        until: String,
    },
    #[error("eval definition {id} case {case_id} stage {stage_id} has an empty check name")]
    EvalCheckNameBlank {
        id: String,
        case_id: String,
        stage_id: String,
    },
    #[error("eval definition {id} case {case_id} stage {stage_id} has duplicate check {check}")]
    EvalDuplicateCheck {
        id: String,
        case_id: String,
        stage_id: String,
        check: String,
    },
    #[error("eval definition {id} case {case_id} check {check} weight must be at least 1")]
    EvalCheckWeight {
        id: String,
        case_id: String,
        check: String,
    },
    #[error("eval definition {id} case {case_id}: {LLM_JUDGE_CHECK} is development-tier only")]
    EvalJudgeAcceptance { id: String, case_id: String },
    #[error("eval definition {id} case {case_id} stage {stage_id} has an empty capture name")]
    EvalCaptureNameBlank {
        id: String,
        case_id: String,
        stage_id: String,
    },
    #[error(
        "eval definition {id} case {case_id} stage {stage_id} has duplicate capture name {name}"
    )]
    EvalDuplicateCapture {
        id: String,
        case_id: String,
        stage_id: String,
        name: String,
    },
    #[error("schema capture {name} needs collection names")]
    EvalSchemaCaptureEmpty { name: String },
    #[error("schema capture {name} repeats {collection}")]
    EvalSchemaCaptureRepeats { name: String, collection: String },
    #[error("eval definition {id} case {case_id} stage {stage_id} capture {name} has an empty collection")]
    EvalDocumentsCaptureBlank {
        id: String,
        case_id: String,
        stage_id: String,
        name: String,
    },
    #[error(
        "eval definition {id} case {case_id} stage {stage_id} capture {name} has an empty glob"
    )]
    EvalFileCaptureBlank {
        id: String,
        case_id: String,
        stage_id: String,
        name: String,
    },
    #[error("eval definition {id} case {case_id} requires at least one acceptance-tier check")]
    EvalAcceptanceRequired { id: String, case_id: String },

    #[error("ChainKeyBinding requires binding_id")]
    ChainKeyBindingIdBlank,
    #[error("ChainKeyBinding requires node_did")]
    ChainKeyBindingNodeBlank,
    #[error("ChainKeyBinding has an invalid Ethereum address")]
    ChainKeyBindingAddress,
    #[error("ChainKeyBinding has unsupported key_backend")]
    ChainKeyBindingKeyBackend,
    #[error("ChainKeyBinding has no principal attestation")]
    ChainKeyBindingAttestation,
    #[error("ChainKeyBinding has no created_at timestamp")]
    ChainKeyBindingCreatedAt,
    #[error("chain key binding update must be an object")]
    ChainKeyBindingUpdateNotObject,

    #[error("{field} response has no rows array")]
    RowsArrayMissing { field: String },
    #[error("document row must be an object")]
    RowNotObject,
    #[error("document row requires physical ID")]
    RowMissingPhysicalId,
    #[error("invalid {field} row")]
    InvalidRow {
        field: String,
        #[source]
        source: serde_json::Error,
    },
}
