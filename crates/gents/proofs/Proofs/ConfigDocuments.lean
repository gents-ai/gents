import Mathlib.Data.List.Basic

/-! Canonical document vocabulary shared by apply and self-configuration models.
Fields describe the spec at ce3aaf12; nested groups are atomic values here, with
semantic validation owned by the resolver. This is a formal view, not another
runtime/config implementation. Observations are excluded from desired fields. -/
namespace ConfigDocuments

inductive Collection where
  | node
  | agent
  | agentContext
  | compaction
  | skill
  | datastoreToolSurface
  | chainKeyBinding
  | ethTool
  | tools
  | agentTarget
  | inferenceBackend
  | inferenceProfile
  | inferenceSampling
  | inferenceExecution
  | inferenceRetryPolicy
  | toolServiceRegistry
  | projectionAcpBinding
  | task
  | schedule
  | eventSource
  | trigger
  | callback
  | callbackBinding
  | callbackModule
  | repositoryPlacement
  | graphDefinition
  | evalDefinition
  deriving DecidableEq, Repr

def all : List Collection :=
  [.node, .agent, .agentContext, .compaction, .skill, .datastoreToolSurface, .chainKeyBinding, .ethTool, .tools, .agentTarget, .inferenceBackend, .inferenceProfile, .inferenceSampling, .inferenceExecution, .inferenceRetryPolicy, .toolServiceRegistry, .projectionAcpBinding, .task, .schedule, .eventSource, .trigger, .callback, .callbackBinding, .callbackModule, .repositoryPlacement, .graphDefinition, .evalDefinition]

/-- One formal catalog row per canonical document; projections below serve
existing consumers without parallel name/key/field tables. -/
structure DocumentSpec where
  collectionName : String
  /-- Logical key within node_did; this is not global database uniqueness. -/
  uniqueField : String
  category : String
  fields : List String

def documentSpec : Collection → DocumentSpec
  | .node => ⟨"Node", "node_did", "node", ["node_did", "display_name", "default_agent_id", "enabled", "created_at", "created_by", "tags", "max_request_hop"]⟩
  | .agent => ⟨"Agent", "agent_id", "agent", ["agent_id", "node_did", "display_name", "description", "context_id", "inference_profile_id", "enabled", "tags", "created_at"]⟩
  | .agentContext => ⟨"AgentContext", "context_id", "agent", ["context_id", "node_did", "display_name", "description", "system_prompt", "tools_id", "compaction_id", "skill_ids", "tags"]⟩
  | .compaction => ⟨"CompactionConfig", "compaction_id", "profile", ["compaction_id", "node_did", "display_name", "strategy", "threshold", "keep_recent_tokens", "tool_result_max_chars", "summary_max_output_tokens", "summary_file_list_max", "inference_profile_id", "tags"]⟩
  | .skill => ⟨"Skill", "skill_id", "tools", ["skill_id", "node_did", "name", "description", "instructions", "source_directory", "tool_refs", "display_name", "interface_json", "enabled", "created_at", "tags"]⟩
  | .datastoreToolSurface => ⟨"DatastoreToolSurface", "surface_id", "tools", ["surface_id", "node_did", "display_name", "enabled", "entries", "created_at", "tags"]⟩
  | .chainKeyBinding => ⟨"ChainKeyBinding", "binding_id", "tools", ["binding_id", "node_did", "address", "key_backend", "attestation", "created_at", "revoked_at", "tags"]⟩
  | .ethTool => ⟨"EthTool", "tool_id", "tools", ["tool_id", "node_did", "display_name", "enabled", "chain_id", "rpc_url", "rpc_timeout_secs", "query_methods", "calls", "key_binding_id", "created_at", "tags"]⟩
  | .tools => ⟨"Tools", "tools_id", "tools", ["tools_id", "node_did", "display_name", "host", "remote", "agents", "built_ins", "datastore", "integrations", "self_config", "tags"]⟩
  | .agentTarget => ⟨"AgentTarget", "target_id", "tools", ["target_id", "node_did", "target_node_did", "agent_id", "name", "description", "tags"]⟩
  | .inferenceBackend => ⟨"InferenceBackend", "backend_id", "backend", ["node_did", "backend_id", "name", "provider_kind", "openai_wire_api", "endpoint", "auth", "connect_timeout_secs", "discovery_timeout_secs", "max_concurrent", "max_queue_depth", "enabled", "tags"]⟩
  | .inferenceProfile => ⟨"InferenceProfile", "profile_id", "profile", ["node_did", "profile_id", "display_name", "description", "backend_id", "model_name", "reasoning_effort", "context_window", "max_output_tokens", "sampling_id", "execution_id", "tags"]⟩
  | .inferenceSampling => ⟨"InferenceSampling", "sampling_id", "profile", ["node_did", "sampling_id", "display_name", "temperature", "top_p", "top_k", "seed", "min_p", "frequency_penalty", "presence_penalty", "repetition_penalty", "tags"]⟩
  | .inferenceExecution => ⟨"InferenceExecution", "execution_id", "profile", ["node_did", "execution_id", "display_name", "max_turns", "max_total_tokens", "stream_batch_ms", "stream_liveness_timeout_secs", "provider_idle_timeout_secs", "deadline_duration_secs", "retry_policy_id", "tags"]⟩
  | .inferenceRetryPolicy => ⟨"InferenceRetryPolicy", "retry_policy_id", "profile", ["node_did", "retry_policy_id", "display_name", "max_transport_retries", "backoff_ms", "max_resample_retries", "allow_repair", "interactive_max_retries", "tags"]⟩
  | .toolServiceRegistry => ⟨"ToolServiceRegistry", "service_id", "mcp_service", ["service_id", "node_did", "display_name", "description", "hostname", "tailscale_ip", "lan_ip", "mcp_port", "mcp_path", "send_node_did", "enabled", "tags"]⟩
  | .projectionAcpBinding => ⟨"ProjectionAcpBinding", "binding_id", "node", ["binding_id", "node_did", "agent_id", "projection_id", "policy_id", "staged_policy_id", "previous_policy_id", "resource_map_json", "enabled", "tags"]⟩
  | .task => ⟨"Task", "task_id", "automation", ["node_did", "task_id", "display_name", "description", "agent_id", "prompt_template", "emit_outcome", "goal_objective_template", "goal_token_budget", "hooks", "enabled", "output_schema_ref", "created_at", "updated_at", "tags"]⟩
  | .schedule => ⟨"Schedule", "schedule_id", "automation", ["node_did", "schedule_id", "display_name", "cadence", "created_at", "updated_at", "tags"]⟩
  | .eventSource => ⟨"EventSource", "event_source_id", "automation", ["node_did", "event_source_id", "display_name", "source_collection", "event_kind", "filter", "correlation_field", "group", "workspace_authority", "created_at", "updated_at", "tags"]⟩
  | .trigger => ⟨"Trigger", "trigger_id", "automation", ["node_did", "trigger_id", "display_name", "description", "task_id", "source", "session_id_template", "enabled", "concurrency", "created_at", "updated_at", "tags"]⟩
  | .callback => ⟨"Callback", "callback_id", "automation", ["callback_id", "node_did", "display_name", "description", "handler", "capabilities", "enabled", "tags"]⟩
  | .callbackBinding => ⟨"CallbackBinding", "binding_id", "automation", ["binding_id", "node_did", "event_source_id", "callback_id", "input_fields", "enabled", "tags"]⟩
  | .callbackModule => ⟨"CallbackModule", "module_id", "automation", ["module_id", "node_did", "abi_version", "wasm_bytes", "canonical_args", "signer_did", "provenance", "enabled", "fuel_limit", "memory_pages", "max_input_bytes", "max_output_bytes", "tags"]⟩
  | .repositoryPlacement => ⟨"RepositoryPlacement", "repository_id", "automation", ["repository_id", "node_did", "host_path", "enabled", "tags"]⟩
  | .graphDefinition => ⟨"GraphDefinition", "graph_id", "automation", ["graph_id", "node_did", "enabled", "created_at", "updated_at", "tags"]⟩
  | .evalDefinition => ⟨"EvalDefinition", "definition_id", "automation", ["definition_id", "node_did", "comparability_version", "title", "subject", "fixtures", "cases", "updated_at", "tags"]⟩

abbrev Collection.collectionName (c : Collection) := (documentSpec c).collectionName
abbrev Collection.uniqueField (c : Collection) := (documentSpec c).uniqueField
abbrev Collection.category (c : Collection) := (documentSpec c).category
abbrev Collection.fields (c : Collection) := (documentSpec c).fields

end ConfigDocuments
