pub const NODE_NAME: &str = "Node";
pub const NODE: &str = include_str!("../schemas/agent/node.graphql");
pub const AGENT_NAME: &str = "Agent";
pub const AGENT: &str = include_str!("../schemas/agent/agent.graphql");
pub const AGENT_CONTEXT_NAME: &str = "AgentContext";
pub const AGENT_CONTEXT: &str = include_str!("../schemas/agent/agent_context.graphql");
pub const COMPACTION_CONFIG_NAME: &str = "CompactionConfig";
pub const COMPACTION_CONFIG: &str = include_str!("../schemas/agent/compaction_config.graphql");
pub const AGENT_TARGET_NAME: &str = "AgentTarget";
pub const AGENT_TARGET: &str = include_str!("../schemas/agent/agent_target.graphql");
pub const NODE_RUNTIME_NAME: &str = "NodeRuntime";
pub const NODE_RUNTIME: &str = include_str!("../schemas/agent/node_runtime.graphql");
pub const NODE_READINESS_NAME: &str = "NodeReadiness";
pub const NODE_READINESS: &str = include_str!("../schemas/agent/node_readiness.graphql");
pub const NODE_DIRECTORY_ENTRY_NAME: &str = "NodeDirectoryEntry";
pub const NODE_DIRECTORY_ENTRY: &str =
    include_str!("../schemas/agent/node_directory_entry.graphql");
pub const NODE_MEMORY_NAME: &str = "NodeMemory";
pub const NODE_MEMORY: &str = include_str!("../schemas/agent/node_memory.graphql");
pub const AGENT_REQUEST_NAME: &str = "AgentRequest";
pub const AGENT_REQUEST: &str = include_str!("../schemas/agent/agent_request.graphql");
pub const AGENT_MESSAGE_NAME: &str = "AgentMessage";
pub const AGENT_MESSAGE: &str = include_str!("../schemas/agent/agent_message.graphql");
pub const AGENT_SESSION_NAME: &str = "AgentSession";
pub const AGENT_SESSION: &str = include_str!("../schemas/agent/agent_session.graphql");
pub const GOAL_NAME: &str = "Goal";
pub const GOAL: &str = include_str!("../schemas/agent/goal.graphql");
pub const PACK_INSTALLATION_NAME: &str = "PackInstallation";
pub const PACK_INSTALLATION: &str = include_str!("../schemas/agent/pack_installation.graphql");
pub const GOAL_CREATION_CLAIM_NAME: &str = "GoalCreationClaim";
pub const GOAL_CREATION_CLAIM: &str = include_str!("../schemas/agent/goal_creation_claim.graphql");
pub const MAILBOX_ITEM_NAME: &str = "MailboxItem";
pub const MAILBOX_ITEM: &str = include_str!("../schemas/agent/mailbox_item.graphql");
pub const AGENT_TOOL_CALL_NAME: &str = "AgentToolCall";
pub const AGENT_TOOL_CALL: &str = include_str!("../schemas/agent/agent_tool_call.graphql");
pub const TOOL_FILE_CHANGE_NAME: &str = "ToolFileChange";
pub const TOOL_FILE_CHANGE: &str = include_str!("../schemas/agent/tool_file_change.graphql");
pub const TOOL_FILE_INVENTORY_NAME: &str = "ToolFileInventory";
pub const TOOL_FILE_INVENTORY: &str = include_str!("../schemas/agent/tool_file_inventory.graphql");
pub const TOOL_FILE_CHANGE_POLICY: &str =
    include_str!("../schemas/agent/tool_file_change_policy.yaml");
pub const TOOL_FILE_CHANGE_POLICY_ID: &str =
    "2e14bb0746cf90a5e955db6b9bb5d9098f6aa0776849c8bcb68db4dd1e67ea82";
pub const TOOL_FILE_CHANGE_POLICY_RESOURCE: &str = "file_images";
pub const AGENT_OUTPUT_SEGMENT_NAME: &str = "AgentOutputSegment";
pub const AGENT_OUTPUT_SEGMENT: &str =
    include_str!("../schemas/agent/agent_output_segment.graphql");
pub const COMPACTION_ENTRY_NAME: &str = "CompactionEntry";
pub const COMPACTION_ENTRY: &str = include_str!("../schemas/agent/compaction_entry.graphql");
pub const RENDERED_REQUEST_NAME: &str = "RenderedRequest";
pub const RENDERED_REQUEST: &str = include_str!("../schemas/agent/rendered_request.graphql");
pub const RENDERED_REQUEST_BLOCK_NAME: &str = "RenderedRequestBlock";
pub const RENDERED_REQUEST_BLOCK: &str =
    include_str!("../schemas/agent/rendered_request_block.graphql");
pub const PROVIDER_CONTEXT_REDUCTION_NAME: &str = "ProviderContextReduction";
pub const PROVIDER_CONTEXT_REDUCTION: &str =
    include_str!("../schemas/agent/provider_context_reduction.graphql");
pub const PROJECTION_ACP_BINDING_NAME: &str = "ProjectionAcpBinding";
pub const PROJECTION_ACP_BINDING: &str =
    include_str!("../schemas/agent/projection_acp_binding.graphql");
pub const TOOLS_NAME: &str = "Tools";
pub const TOOLS: &str = include_str!("../schemas/agent/tools.graphql");
pub const SKILL_NAME: &str = "Skill";
pub const SKILL: &str = include_str!("../schemas/agent/skill.graphql");
pub const DATASTORE_TOOL_SURFACE_NAME: &str = "DatastoreToolSurface";
pub const DATASTORE_TOOL_SURFACE: &str =
    include_str!("../schemas/agent/datastore_tool_surface.graphql");
pub const CHAIN_KEY_BINDING_NAME: &str = "ChainKeyBinding";
pub const CHAIN_KEY_BINDING: &str = include_str!("../schemas/agent/chain_key_binding.graphql");
pub const ETH_TOOL_NAME: &str = "EthTool";
pub const ETH_TOOL: &str = include_str!("../schemas/agent/eth_tool.graphql");
pub const PENDING_CHAT_SUBMISSION_NAME: &str = "PendingChatSubmission";
pub const PENDING_CHAT_SUBMISSION: &str =
    include_str!("../schemas/agent/pending_chat_submission.graphql");
pub const ETH_SUBMISSION_NAME: &str = "EthSubmission";
pub const ETH_SUBMISSION: &str = include_str!("../schemas/agent/eth_submission.graphql");
pub const WORKSPACE_ROOT_NAME: &str = "WorkspaceRoot";
pub const WORKSPACE_ROOT: &str = include_str!("../schemas/agent/workspace_root.graphql");
pub const ISOLATED_WORKSPACE_NAME: &str = "IsolatedWorkspace";
pub const ISOLATED_WORKSPACE: &str = include_str!("../schemas/agent/isolated_workspace.graphql");
pub const WORKSPACE_PLACEMENT_NAME: &str = "WorkspacePlacement";
pub const WORKSPACE_PLACEMENT: &str = include_str!("../schemas/agent/workspace_placement.graphql");
pub const REPOSITORY_PLACEMENT_NAME: &str = "RepositoryPlacement";
pub const REPOSITORY_PLACEMENT: &str =
    include_str!("../schemas/agent/repository_placement.graphql");
pub const WORKSPACE_BINDING_NAME: &str = "WorkspaceBinding";
pub const WORKSPACE_BINDING: &str = include_str!("../schemas/agent/workspace_binding.graphql");
pub const WORKSPACE_RECEIPT_NAME: &str = "WorkspaceReceipt";
pub const WORKSPACE_RECEIPT: &str = include_str!("../schemas/agent/workspace_receipt.graphql");
pub const EVENT_SOURCE_CURSOR_NAME: &str = "EventSourceCursor";
pub const EVENT_SOURCE_CURSOR: &str = include_str!("../schemas/agent/event_source_cursor.graphql");
pub const TRIGGER_FIRE_NAME: &str = "TriggerFire";
pub const TRIGGER_FIRE: &str = include_str!("../schemas/agent/trigger_fire.graphql");
pub const FIRE_OUTCOME_NAME: &str = "FireOutcome";
pub const FIRE_OUTCOME: &str = include_str!("../schemas/agent/fire_outcome.graphql");
pub const TRIGGER_NAME: &str = "Trigger";
pub const TRIGGER: &str = include_str!("../schemas/agent/trigger.graphql");
pub const CALLBACK_NAME: &str = "Callback";
pub const CALLBACK: &str = include_str!("../schemas/agent/callback.graphql");
pub const EVENT_SOURCE_NAME: &str = "EventSource";
pub const EVENT_SOURCE: &str = include_str!("../schemas/agent/event_source.graphql");
pub const CALLBACK_MODULE_NAME: &str = "CallbackModule";
pub const CALLBACK_MODULE: &str = include_str!("../schemas/agent/callback_module.graphql");
pub const CALLBACK_BINDING_NAME: &str = "CallbackBinding";
pub const CALLBACK_BINDING: &str = include_str!("../schemas/agent/callback_binding.graphql");
pub const CALLBACK_INVOCATION_NAME: &str = "CallbackInvocation";
pub const CALLBACK_INVOCATION: &str = include_str!("../schemas/agent/callback_invocation.graphql");
pub const CALLBACK_RESULT_NAME: &str = "CallbackResult";
pub const CALLBACK_RESULT: &str = include_str!("../schemas/agent/callback_result.graphql");
pub const TASK_NAME: &str = "Task";
pub const TASK: &str = include_str!("../schemas/agent/task.graphql");
pub const SCHEDULE_NAME: &str = "Schedule";
pub const SCHEDULE: &str = include_str!("../schemas/agent/schedule.graphql");
pub const EVENT_GROUP_STATE_NAME: &str = "EventGroupState";
pub const EVENT_GROUP_STATE: &str = include_str!("../schemas/agent/event_group_state.graphql");
pub const EVAL_DEFINITION_NAME: &str = "EvalDefinition";
pub const EVAL_DEFINITION: &str = include_str!("../schemas/agent/eval_definition.graphql");
pub const EVAL_RUN_NAME: &str = "EvalRun";
pub const EVAL_RUN: &str = include_str!("../schemas/agent/eval_run.graphql");
pub const EVAL_TRIAL_NAME: &str = "EvalTrial";
pub const EVAL_TRIAL: &str = include_str!("../schemas/agent/eval_trial.graphql");
pub const EVAL_VERDICT_NAME: &str = "EvalVerdict";
pub const EVAL_VERDICT: &str = include_str!("../schemas/agent/eval_verdict.graphql");
pub const OPTIMIZATION_JOB_NAME: &str = "OptimizationJob";
pub const OPTIMIZATION_JOB: &str = include_str!("../schemas/agent/optimization_job.graphql");
pub const PEER_PAIRING_DESIRED_NAME: &str = "PeerPairingDesired";
pub const PEER_PAIRING_DESIRED: &str =
    include_str!("../schemas/agent/peer_pairing_desired.graphql");
pub const DATA_PLANE_PAIRING_DESIRED_NAME: &str = "DataPlanePairingDesired";
pub const DATA_PLANE_PAIRING_DESIRED: &str =
    include_str!("../schemas/agent/data_plane_pairing_desired.graphql");
pub const PEER_PAIRING_APPLIED_NAME: &str = "PeerPairingApplied";
pub const PEER_PAIRING_APPLIED: &str =
    include_str!("../schemas/agent/peer_pairing_applied.graphql");
pub const PEER_REGISTRY_NAME: &str = "PeerRegistry";
pub const PEER_REGISTRY: &str = include_str!("../schemas/agent/peer_registry.graphql");
pub const NETWORK_NAME: &str = "Network";
pub const NETWORK: &str = include_str!("../schemas/agent/network.graphql");
pub const PEER_ENDPOINT_NAME: &str = "PeerEndpoint";
pub const PEER_ENDPOINT: &str = include_str!("../schemas/agent/peer_endpoint.graphql");
pub const NETWORK_ADMIN_PIN_NAME: &str = "NetworkAdminPin";
pub const NETWORK_ADMIN_PIN: &str = include_str!("../schemas/agent/network_admin_pin.graphql");
pub const NETWORK_ENROLLMENT_REQUEST_NAME: &str = "NetworkEnrollmentRequest";
pub const NETWORK_ENROLLMENT_REQUEST: &str =
    include_str!("../schemas/agent/network_enrollment_request.graphql");
pub const NETWORK_ENROLLMENT_DECISION_NAME: &str = "NetworkEnrollmentDecision";
pub const NETWORK_ENROLLMENT_DECISION: &str =
    include_str!("../schemas/agent/network_enrollment_decision.graphql");
pub const NETWORK_AUTHORIZATION_REVISION_NAME: &str = "NetworkAuthorizationRevision";
pub const NETWORK_AUTHORIZATION_REVISION: &str =
    include_str!("../schemas/agent/network_authorization_revision.graphql");
pub const NETWORK_ENROLLMENT_ROUTE_RECEIPT_NAME: &str = "NetworkEnrollmentRouteReceipt";
pub const NETWORK_ENROLLMENT_ROUTE_RECEIPT: &str =
    include_str!("../schemas/agent/network_enrollment_route_receipt.graphql");
pub const ENROLLMENT_OPERATOR_NONCE_NAME: &str = "EnrollmentOperatorNonce";
pub const ENROLLMENT_OPERATOR_NONCE: &str =
    include_str!("../schemas/agent/enrollment_operator_nonce.graphql");
pub const FILE_IMAGE_HYDRATION_REQUEST_NAME: &str = "FileImageHydrationRequest";
pub const FILE_IMAGE_HYDRATION_REQUEST: &str =
    include_str!("../schemas/agent/file_image_hydration_request.graphql");
pub const FILE_INVENTORY_REQUEST_NAME: &str = "FileInventoryRequest";
pub const FILE_INVENTORY_REQUEST: &str =
    include_str!("../schemas/agent/file_inventory_request.graphql");
pub const FILE_INVENTORY_PAGE_NAME: &str = "FileInventoryPage";
pub const FILE_INVENTORY_PAGE: &str = include_str!("../schemas/agent/file_inventory_page.graphql");
pub const FILE_INVENTORY_PROGRESS_NAME: &str = "FileInventoryProgress";
pub const FILE_INVENTORY_PROGRESS: &str =
    include_str!("../schemas/agent/file_inventory_progress.graphql");
pub const FILE_INVENTORY_REPLY_NAME: &str = "FileInventoryReply";
pub const FILE_INVENTORY_REPLY: &str =
    include_str!("../schemas/agent/file_inventory_reply.graphql");
pub const FILE_IMAGE_HYDRATION_RECEIPT_NAME: &str = "FileImageHydrationReceipt";
pub const FILE_IMAGE_HYDRATION_RECEIPT: &str =
    include_str!("../schemas/agent/file_image_hydration_receipt.graphql");
pub const SESSION_HYDRATION_REQUEST_NAME: &str = "SessionHydrationRequest";
pub const SESSION_HYDRATION_REQUEST: &str =
    include_str!("../schemas/agent/session_hydration_request.graphql");

pub const AGENT_SESSION_INPUT_EDIT_NAME: &str = "AgentSessionInputEdit";
pub const AGENT_SESSION_INPUT_EDIT: &str =
    include_str!("../schemas/agent/agent_session_input_edit.graphql");

pub const AGENT_SESSION_GOAL_CONTROL_NAME: &str = "AgentSessionGoalControl";
pub const AGENT_SESSION_GOAL_CONTROL: &str =
    include_str!("../schemas/agent/agent_session_goal_control.graphql");
pub const GOAL_OBSERVATION_NAME: &str = "GoalObservation";
pub const GOAL_OBSERVATION: &str = include_str!("../schemas/agent/goal_observation.graphql");

pub const ALL: &[&str] = &[
    NODE,
    AGENT,
    COMPACTION_CONFIG,
    AGENT_CONTEXT,
    AGENT_TARGET,
    NODE_RUNTIME,
    NODE_READINESS,
    NODE_DIRECTORY_ENTRY,
    NODE_MEMORY,
    TOOLS,
    SKILL,
    DATASTORE_TOOL_SURFACE,
    CHAIN_KEY_BINDING,
    ETH_TOOL,
    ETH_SUBMISSION,
    PENDING_CHAT_SUBMISSION,
    WORKSPACE_ROOT,
    ISOLATED_WORKSPACE,
    WORKSPACE_PLACEMENT,
    REPOSITORY_PLACEMENT,
    WORKSPACE_BINDING,
    WORKSPACE_RECEIPT,
    CALLBACK,
    EVENT_SOURCE,
    TRIGGER,
    EVENT_SOURCE_CURSOR,
    TRIGGER_FIRE,
    FIRE_OUTCOME,
    CALLBACK_MODULE,
    CALLBACK_BINDING,
    CALLBACK_INVOCATION,
    CALLBACK_RESULT,
    AGENT_REQUEST,
    AGENT_OUTPUT_SEGMENT,
    AGENT_SESSION,
    GOAL,
    GOAL_CREATION_CLAIM,
    MAILBOX_ITEM,
    AGENT_MESSAGE,
    AGENT_TOOL_CALL,
    TOOL_FILE_CHANGE,
    TOOL_FILE_INVENTORY,
    COMPACTION_ENTRY,
    RENDERED_REQUEST,
    RENDERED_REQUEST_BLOCK,
    PROVIDER_CONTEXT_REDUCTION,
    PROJECTION_ACP_BINDING,
    TASK,
    SCHEDULE,
    EVENT_GROUP_STATE,
    PEER_PAIRING_DESIRED,
    DATA_PLANE_PAIRING_DESIRED,
    PEER_PAIRING_APPLIED,
    PEER_REGISTRY,
    NETWORK,
    PEER_ENDPOINT,
    NETWORK_ADMIN_PIN,
    NETWORK_ENROLLMENT_REQUEST,
    NETWORK_ENROLLMENT_DECISION,
    NETWORK_AUTHORIZATION_REVISION,
    NETWORK_ENROLLMENT_ROUTE_RECEIPT,
    ENROLLMENT_OPERATOR_NONCE,
    SESSION_HYDRATION_REQUEST,
    FILE_IMAGE_HYDRATION_REQUEST,
    FILE_INVENTORY_REQUEST,
    FILE_INVENTORY_PAGE,
    FILE_INVENTORY_REPLY,
    FILE_INVENTORY_PROGRESS,
    FILE_IMAGE_HYDRATION_RECEIPT,
    AGENT_SESSION_INPUT_EDIT,
    AGENT_SESSION_GOAL_CONTROL,
    GOAL_OBSERVATION,
    EVAL_DEFINITION,
    EVAL_RUN,
    EVAL_TRIAL,
    EVAL_VERDICT,
    OPTIMIZATION_JOB,
    PACK_INSTALLATION,
];

pub const ALL_COLLECTION_NAMES: &[&str] = &[
    NODE_NAME,
    AGENT_NAME,
    COMPACTION_CONFIG_NAME,
    AGENT_CONTEXT_NAME,
    AGENT_TARGET_NAME,
    NODE_RUNTIME_NAME,
    NODE_READINESS_NAME,
    NODE_DIRECTORY_ENTRY_NAME,
    NODE_MEMORY_NAME,
    TOOLS_NAME,
    SKILL_NAME,
    DATASTORE_TOOL_SURFACE_NAME,
    CHAIN_KEY_BINDING_NAME,
    ETH_TOOL_NAME,
    ETH_SUBMISSION_NAME,
    PENDING_CHAT_SUBMISSION_NAME,
    WORKSPACE_ROOT_NAME,
    ISOLATED_WORKSPACE_NAME,
    WORKSPACE_PLACEMENT_NAME,
    REPOSITORY_PLACEMENT_NAME,
    WORKSPACE_BINDING_NAME,
    WORKSPACE_RECEIPT_NAME,
    CALLBACK_NAME,
    EVENT_SOURCE_NAME,
    TRIGGER_NAME,
    EVENT_SOURCE_CURSOR_NAME,
    TRIGGER_FIRE_NAME,
    FIRE_OUTCOME_NAME,
    CALLBACK_MODULE_NAME,
    CALLBACK_BINDING_NAME,
    CALLBACK_INVOCATION_NAME,
    CALLBACK_RESULT_NAME,
    AGENT_REQUEST_NAME,
    AGENT_OUTPUT_SEGMENT_NAME,
    AGENT_SESSION_NAME,
    GOAL_NAME,
    GOAL_CREATION_CLAIM_NAME,
    MAILBOX_ITEM_NAME,
    AGENT_MESSAGE_NAME,
    AGENT_TOOL_CALL_NAME,
    TOOL_FILE_CHANGE_NAME,
    TOOL_FILE_INVENTORY_NAME,
    COMPACTION_ENTRY_NAME,
    RENDERED_REQUEST_NAME,
    RENDERED_REQUEST_BLOCK_NAME,
    PROVIDER_CONTEXT_REDUCTION_NAME,
    PROJECTION_ACP_BINDING_NAME,
    TASK_NAME,
    SCHEDULE_NAME,
    EVENT_GROUP_STATE_NAME,
    PEER_PAIRING_DESIRED_NAME,
    DATA_PLANE_PAIRING_DESIRED_NAME,
    PEER_PAIRING_APPLIED_NAME,
    PEER_REGISTRY_NAME,
    NETWORK_NAME,
    PEER_ENDPOINT_NAME,
    NETWORK_ADMIN_PIN_NAME,
    NETWORK_ENROLLMENT_REQUEST_NAME,
    NETWORK_ENROLLMENT_DECISION_NAME,
    NETWORK_AUTHORIZATION_REVISION_NAME,
    NETWORK_ENROLLMENT_ROUTE_RECEIPT_NAME,
    ENROLLMENT_OPERATOR_NONCE_NAME,
    SESSION_HYDRATION_REQUEST_NAME,
    FILE_IMAGE_HYDRATION_REQUEST_NAME,
    FILE_INVENTORY_REQUEST_NAME,
    FILE_INVENTORY_PAGE_NAME,
    FILE_INVENTORY_REPLY_NAME,
    FILE_INVENTORY_PROGRESS_NAME,
    FILE_IMAGE_HYDRATION_RECEIPT_NAME,
    AGENT_SESSION_INPUT_EDIT_NAME,
    AGENT_SESSION_GOAL_CONTROL_NAME,
    GOAL_OBSERVATION_NAME,
    EVAL_DEFINITION_NAME,
    EVAL_RUN_NAME,
    EVAL_TRIAL_NAME,
    EVAL_VERDICT_NAME,
    OPTIMIZATION_JOB_NAME,
    PACK_INSTALLATION_NAME,
];

pub const BRANCHABLE_COLLECTION_NAMES: &[&str] = &[
    NODE_DIRECTORY_ENTRY_NAME,
    NODE_MEMORY_NAME,
    AGENT_REQUEST_NAME,
    AGENT_OUTPUT_SEGMENT_NAME,
    AGENT_SESSION_NAME,
    GOAL_NAME,
    GOAL_CREATION_CLAIM_NAME,
    MAILBOX_ITEM_NAME,
    AGENT_MESSAGE_NAME,
    AGENT_TOOL_CALL_NAME,
    COMPACTION_ENTRY_NAME,
    TASK_NAME,
    SCHEDULE_NAME,
    EVENT_GROUP_STATE_NAME,
    ISOLATED_WORKSPACE_NAME,
    WORKSPACE_BINDING_NAME,
    WORKSPACE_RECEIPT_NAME,
    CALLBACK_RESULT_NAME,
];

/// Whole-collection subscriptions would bypass the file-review selection
/// boundary. These records use filtered control routes or document-scoped push.
pub const EXPLICIT_DELIVERY_COLLECTION_NAMES: &[&str] = &[
    FILE_INVENTORY_REQUEST_NAME,
    FILE_INVENTORY_PAGE_NAME,
    TOOL_FILE_CHANGE_NAME,
    TOOL_FILE_INVENTORY_NAME,
    FILE_IMAGE_HYDRATION_REQUEST_NAME,
];

pub fn requires_explicit_delivery(name: &str) -> bool {
    EXPLICIT_DELIVERY_COLLECTION_NAMES.contains(&name)
}

/// Sensitive runtime facts that stay on the runtime node; every broad
/// replication catalog filters these names explicitly.
pub const LOCAL_AUDIT_COLLECTION_NAMES: &[&str] = &[
    RENDERED_REQUEST_NAME,
    RENDERED_REQUEST_BLOCK_NAME,
    PROVIDER_CONTEXT_REDUCTION_NAME,
    ETH_SUBMISSION_NAME,
    EVAL_DEFINITION_NAME,
    EVAL_VERDICT_NAME,
    OPTIMIZATION_JOB_NAME,
];

/// Local trust state that must never be learned from peers.
pub const LOCAL_ONLY_COLLECTION_NAMES: &[&str] = &[
    NETWORK_ADMIN_PIN_NAME,
    ENROLLMENT_OPERATOR_NONCE_NAME,
    FILE_INVENTORY_REPLY_NAME,
    FILE_INVENTORY_PROGRESS_NAME,
    FILE_IMAGE_HYDRATION_RECEIPT_NAME,
    PENDING_CHAT_SUBMISSION_NAME,
];

pub fn is_local_audit_collection(name: &str) -> bool {
    LOCAL_AUDIT_COLLECTION_NAMES.contains(&name)
}

pub fn is_local_only_collection(name: &str) -> bool {
    LOCAL_ONLY_COLLECTION_NAMES.contains(&name)
}
