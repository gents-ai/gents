use super::*;

fn patch_contract(target: SelfConfigTarget, field_shapes: Value) -> Value {
    json!({
        "collection": target.collection_name(),
        "writable_fields": target.writable_fields(),
        "protected_fields": target.protected_fields(),
        "field_shapes": field_shapes,
        "contract_source": "Top-level fields come from canonical serde configuration metadata. Nested field inventories and advertised enum values are conformance-tested against the canonical Rust types; every preview/apply performs full typed decode and validation.",
        "semantics": "--set replaces one top-level field with the supplied JSON value; nested objects are complete typed values, not recursive merge patches. --clear removes an optional top-level field. Omitted fields are preserved.",
    })
}

pub(in crate::tools::builtin::self_config) fn help_patch_contracts(
    resource: Option<&str>,
) -> Value {
    if let Some(name) = resource {
        let family = match name {
            "context" => Some("agent"),
            "sampling" | "retry-policy" | "compaction" => Some("profile"),
            "task" | "trigger" | "schedule" | "event-source" => Some("automation"),
            _ => None,
        };
        if let Some(family) = family {
            let target = crud::resource_target(name).expect("resource metadata");
            return Value::Array(
                help_patch_contracts(Some(family))
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|contract| contract["collection"] == target.collection_name())
                    .cloned()
                    .collect(),
            );
        }
    }
    let contracts = match resource {
        Some("skill") => vec![patch_contract(
            SelfConfigTarget::Skill,
            json!({
                "name":"string|null; skill name",
                "description":"string|null; when to use this skill",
                "instructions":"string|null; Markdown instructions",
                "source_directory":"string|null; resolved local SKILL.md parent; supporting-file base, never an access grant",
                "tool_refs":"array<string>; tool dependencies, default []; never tool grants",
                "display_name":"string|null; display label",
                "interface_json":"string|null; serialized interface metadata",
                "enabled":"boolean; default true",
                "tags":"array<string>; default []"
            }),
        )],
        Some("datastore") => vec![patch_contract(
            SelfConfigTarget::DatastoreToolSurface,
            json!({
                "display_name":"string|null",
                "enabled":"boolean; default true",
                "entries":"array<SurfaceToolDecl>|null (reads use {entries:[...]} envelope). Create: {tool_name:string,collection:string,description?:string,fields:[{name:string,required?:boolean,fill?:correlation|{source_field:string}}],output_obligation?:{scope:request|trigger,minimum_writes?:positive integer,expected_count_field?:string}}. Query: {kind:query,tool_name:string,collection:string,description?:string,fields:[string],filter_fields?:[same field objects]}. fill fields are runtime-filled and never model arguments (what fill means: [\"help\",\"datastore\"]); other fields are model arguments. output_obligation is for workflows that must produce stored output, not a guarantee about one tool call. It gates request completion: request covers every request, including automated handlers; trigger covers automated work only. Requiring writes to a watched input collection can retrigger the handler. Omit for inbox writers and general-purpose tools. Surface writes validate declaration syntax. Selecting the surface in Tools checks tool-name collisions; runtime invocation validates the target collection/fields. Register schemas first, then bind and exercise the tools to establish readiness.",
                "tags":"array<string>; default []"
            }),
        )],
        Some("agent") => vec![
            patch_contract(
                SelfConfigTarget::Agent,
                json!({
                    "display_name": "string|null",
                    "description": "string|null",
                    "context_id": "existing same-node AgentContext ID|null",
                    "inference_profile_id": "existing same-node InferenceProfile ID (required); never another account of the current provider",
                    "enabled": "boolean; default true",
                    "tags": "array<string>; default [] (UI/discovery labels only)",
                }),
            ),
            patch_contract(
                SelfConfigTarget::AgentContext,
                json!({
                    "display_name": "string|null",
                    "description": "string|null",
                    "system_prompt": "literal string|null; never template-evaluated",
                    "tools_id": "existing same-node Tools ID|null; null grants no tools",
                    "compaction_id": "existing same-node CompactionConfig ID|null; null uses runtime compaction defaults",
                    "skill_ids": "array<existing same-node Skill ID>; default []",
                    "tags": "array<string>; default []",
                }),
            ),
        ],
        Some("tools") => vec![patch_contract(
            SelfConfigTarget::Tools,
            json!({
                "display_name": "string|null",
                "host": {"root":"string|null; absent uses runtime cwd", "files":{"mode":"Off|ReadOnly|ReadWrite (default Off)","max_read_chars":"integer 1..1000000|null; default 32000; read_file bytes, default and maximum","max_list_entries":"integer 1..5000|null; default 200","max_matches":"integer 1..5000|null; default 200; glob and grep"}, "bash":{"mode":"Off|ReadOnly|Unrestricted; default Off; ReadOnly still runs shell commands","execution_mode":"read_only|workspace_write|artifact_write|unrestricted|null","network_mode":"inherit|disabled|enabled|null","allowed_argv_prefixes":"array<array<string>>|null","forbidden_argv_prefixes":"array<array<string>>|null","read_only_commands":"array<string>|null","background_enabled":"boolean; default false","timeout_secs":"positive integer|null; default host --command-timeout-secs (120); clamped to the host maximum","max_timeout_secs":"positive integer|null; default timeout_secs if set, else host maximum; clamped to host --command-timeout-max-secs","background_timeout_secs":"positive integer|null; default 36000; clamped to 36000","wait_timeout_secs":"positive integer|null; default 30","max_wait_timeout_secs":"positive integer|null; default 600; clamped to 600","max_output_chars":"integer 1..1000000|null; default 16000; stdout and stderr each"}, "cli":[{"name":"string; host-registered CLI tool name (cli default [])","timeout_secs":"positive integer|null; default host registration (10); clamped to host --command-timeout-max-secs","max_output_chars":"integer 1..1000000|null; default host registration (16000); stdout and stderr each"}]},
                "remote": {"services":"array<{mcp_service_id:string,tool_names:array<string>,style:flat|discovery(default),required:boolean(default false),background_tool_names:array<string>,connect_timeout_secs?:integer,discovery_timeout_secs?:integer,timeout_secs?:integer,stale_timeout_secs?:integer,background_timeout_secs?:integer (default 36000; clamped to 36000),wait_timeout_secs?:integer (default 30),max_wait_timeout_secs?:integer (default 600; clamped to 600)}>; default []"},
                "agents": {"target_ids":"array<existing same-node AgentTarget ID>; default []; the agents agent_new may address (create them with agent-target)","enabled":"boolean|null; enables the agents tools: agent_message, agent_interrupt, agent_stop, agent_resume, agent_list, agent_session_list and agent_session_read, plus agent_new when target_ids selects a target"},
                "built_ins": {"enable_goal_tools":"boolean|null","enable_goal_creation":"boolean|null","enable_memory":"boolean|null","enable_session_history_tool":"boolean|null","enable_schema_tool":"boolean|null","enable_p2p_tool":"boolean|null","enable_p2p_mutations":"boolean|null","p2p_collections":"array<string>|null","enable_context_budget":"boolean|null"},
                "datastore": {"enable_defra_query":"boolean|null","defra_query_collections":"array<string>|null","write_collections":"array<exact application collection>|null (empty denies all)","datastore_tool_surface_ids":"array<existing same-node DatastoreToolSurface ID>|null"},
                "integrations": {"lsp":{"config":"JSON encoded as a string|null","timeout_secs":"positive integer|null; default 20","max_timeout_secs":"positive integer|null; default 300; clamped to 300"},"plugins":"array<{plugin: installed namespace/name, digest: sha256:<hex>|null, job: {agent_targets: non-empty array<existing AgentTarget ID>, max_live: 1..64; default 8, max_spawns: 1..1024; default 128, max_messages_per_slot: 0..16; default 4, result_bytes: 1024..1048576; default 65536, max_wall_secs: 60..86400; default 7200, policy: {dynamic: off|check|run; default check, dynamic_targets: array<ID> within agent_targets unless dynamic is off, trusted_packs: array<pack coordinate>}}|null}>|null; the plugin must declare job: true; job is operator-only, so a Tools write cannot add one or widen it"},
                "self_config": {"enable_self_config":"boolean|null; absent is disabled","self_config_categories":"array<node|agent|tools|profile|backend|mcp_service|automation>|null; absent selects agent, tools, profile; node grants the agent catalog; agent permits only current-agent configuration","self_config_no_lockout":"boolean|null","self_config_preview":"boolean|null; grants the preview verb","enable_pack_install":"boolean|null; raise it only if you hold it; keep a stored true when rewriting self_config"},
                "tags": "array<string>; default []",
            }),
        )],
        Some("profile") => vec![
            patch_contract(
                SelfConfigTarget::InferenceProfile,
                json!({
                    "display_name":"string|null","description":"string|null","backend_id":"existing same-node backend ID; never another account of the current provider","model_name":"string","reasoning_effort":"none|minimal|low|medium|high|xhigh|max|ultra|null","service_tier":"default|fast|null; OpenAI-compatible and ChatGPT backends only","context_window":"positive integer <= the model's advertised maximum context window, when the backend advertises one|null","max_output_tokens":"positive integer|null","sampling_id":"existing same-node sampling ID|null","execution_id":"existing same-node execution ID|null","tags":"array<string>; default []"
                }),
            ),
            patch_contract(
                SelfConfigTarget::InferenceSampling,
                json!({
                    "display_name":"string|null","temperature":"number >= 0|null","top_p":"number 0..1|null","top_k":"positive integer|null","seed":"integer >= 0|null","min_p":"number 0..1|null","frequency_penalty":"number -2..2|null","presence_penalty":"number -2..2|null","repetition_penalty":"number > 0|null","tags":"array<string>; default []"
                }),
            ),
            patch_contract(
                SelfConfigTarget::InferenceExecution,
                json!({
                    "display_name":"string|null","max_turns":format!("positive integer|null; default {}", crate::inference::resolved::DEFAULT_MAX_TURNS),"max_total_tokens":"positive integer|null; null is unlimited","stream_batch_ms":format!("positive integer|null; persistence batching interval, {}x for reasoning-only output; default 1000 ms", crate::request::streaming::REASONING_BATCH_FACTOR),"stream_liveness_timeout_secs":format!("positive integer|null; renewed execution lease, independent of provider output; default {} seconds and less than deadline", crate::inference::resolved::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS),"provider_idle_timeout_secs":format!("positive integer|null; maximum provider transport silence, not total request duration; default {} seconds", crate::inference::resolved::DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS),"deadline_duration_secs":format!("positive integer|null; total wall time for one request across model/tool turns, not the whole session; default 86400; at most {}", crate::documents::MAX_DEADLINE_DURATION_SECS),"retry_policy_id":"existing same-node retry policy ID|null; null uses request-origin retry defaults","tags":"array<string>; default []"
                }),
            ),
            patch_contract(
                SelfConfigTarget::InferenceRetryPolicy,
                json!({
                    "display_name":"string|null","max_transport_retries":"integer >= 0|null","backoff_ms":"array<positive integer>|null","max_resample_retries":"integer >= 0|null","allow_repair":"boolean|null","interactive_max_retries":"integer >= 0|null","tags":"array<string>; default []"
                }),
            ),
            patch_contract(
                SelfConfigTarget::Compaction,
                json!({
                    "display_name":"string|null","strategy":"StripToolResults|StripThenSummarize|null; absent uses StripThenSummarize","threshold":"number 0..1|null; default 0.75","keep_recent_tokens":"non-negative integer|null","tool_result_max_chars":"positive integer|null","summary_max_output_tokens":"positive integer|null","summary_file_list_max":"non-negative integer|null","inference_profile_id":"existing same-node profile ID|null; never another account of the current provider","tags":"array<string>; default []"
                }),
            ),
        ],
        Some("backend") => vec![patch_contract(
            SelfConfigTarget::InferenceBackend,
            json!({
                "name":"string","provider_kind":"OpenAiCompatible|OpenRouter|ChatGptCodex|XaiGrokOAuth|ClaudeCliSubscription|AnthropicApiKey","openai_wire_api":"chat_completions|responses|null","endpoint":"URL string","auth":"{kind:unauthenticated}|{kind:environment,variable:string}|{kind:node_oauth}; raw api_key values and node_oauth account_ref are operator-managed","connect_timeout_secs":"positive integer|null; default 10","discovery_timeout_secs":"positive integer|null; default 10","max_concurrent":"positive integer|null; default 1","max_queue_depth":"integer >= 0|null; default 100","enabled":"boolean; default true","tags":"array<string>; default []"
            }),
        )],
        Some("mcp-service") => vec![patch_contract(
            SelfConfigTarget::ToolServiceRegistry,
            json!({
                "display_name":"string|null","description":"string|null","hostname":"string|null","tailscale_ip":"string|null","lan_ip":"string|null","mcp_port":"integer|null","mcp_path":"string|null; absent uses endpoint root","send_node_did":"boolean; default false","enabled":"boolean; default true","tags":"array<string>; default []"
            }),
        )],
        Some("automation") => vec![
            patch_contract(
                SelfConfigTarget::Task,
                json!({
                    "display_name":"string|null","description":"string|null","prompt_template":"string; rendered per invocation; state what to return or write and where. Writing to the watched collection triggers it again","emit_outcome":"boolean; true writes the standard per-input success/failure record (FireOutcome) at request or Goal termination. Default false writes no completion record; leave false for outcome consumers","goal_objective_template":"string|null; starts a durable goal that continues until update_goal completes it; requires built_ins.enable_goal_tools on the agent. Omit for ordinary one-request tasks","goal_token_budget":"positive integer|null; requires goal_objective_template; absent means unlimited","hooks":"array<{hook_id:string,phase:before|after_success|after_failure|finally,command:nonempty array<string>,timeout_secs?:positive integer}>; default []","enabled":"boolean; default true","output_schema_ref":"string|null","tags":"array<string>; default []"
                }),
            ),
            patch_contract(
                SelfConfigTarget::Schedule,
                json!({
                    "display_name":"string|null","cadence":"{kind:interval,interval_secs:positive integer}|{kind:cron,expression:string,timezone:string,missed_run_policy?:latest_only}","tags":"array<string>; default []"
                }),
            ),
            patch_contract(
                SelfConfigTarget::Trigger,
                json!({
                    "display_name":"string|null","description":"string|null","task_id":"existing task ID owned by selected agent","source":"{kind:schedule,schedule_id:string}|{kind:event,event_source_id:string}","enabled":"boolean; default true","concurrency":"parallel|queued_serial|serial|latest_only|null; default parallel; controls overlap, not session destination; queued_serial needs an event source or an explicit scheduled session destination","session_id_template":"string|null; template rendering an existing session ID; event or schedule sources; target must be open and use the Task agent. Null creates a session per fire. Independent of concurrency","tags":"array<string>; default []"
                }),
            ),
            patch_contract(
                SelfConfigTarget::EventSource,
                json!({
                    "display_name":"string|null","source_collection":"installed collection name","event_kind":"created|null; default created","filter":"string|null; a GraphQL filter object literal with unquoted keys","correlation_field":"GraphQL field name|null","group":"{expected_count?:positive integer|{source_field:string},timeout_secs?:positive integer,min_count?:positive integer}|null","workspace_authority":"canonical workspace authority object|null","tags":"array<string>; default []"
                }),
            ),
        ],
        Some("agent-target") => vec![patch_contract(
            SelfConfigTarget::AgentTarget,
            json!({
                "name":"string; the agent name agent_new exposes","target_node_did":"node DID; omit on create for a local helper (authenticated node). Set explicitly for remote targets. Omitted on update preserves the destination; never infer identity from a document ID","agent_id":"agent ID on target_node_did; must exist when local","description":"string|null","tags":"array<string>; default []"
            }),
        )],
        Some("execution") => help_patch_contracts(Some("profile"))
            .as_array()
            .into_iter()
            .flatten()
            .filter(|contract| contract["collection"] == "InferenceExecution")
            .cloned()
            .collect(),
        _ => Vec::new(),
    };
    Value::Array(contracts)
}

pub(in crate::tools::builtin::self_config) fn field_shape(
    collection: &str,
    path: &str,
) -> Option<Value> {
    let contract = [
        "agent",
        "tools",
        "datastore",
        "skill",
        "profile",
        "backend",
        "mcp-service",
        "automation",
        "agent-target",
    ]
    .into_iter()
    .flat_map(|resource| match help_patch_contracts(Some(resource)) {
        Value::Array(contracts) => contracts,
        _ => Vec::new(),
    })
    .find(|contract| contract["collection"] == collection)?;
    let mut shape = &contract["field_shapes"];
    let path = path.replace('[', ".[");
    for segment in path.split('.').filter(|segment| !segment.is_empty()) {
        shape = if segment.starts_with('[') {
            match shape {
                Value::Array(items) => items.first()?,
                other => other,
            }
        } else {
            shape.get(segment)?
        };
    }
    Some(shape.clone())
}
