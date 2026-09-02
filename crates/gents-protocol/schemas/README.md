# Schema Data Model

This directory contains the non-agent DefraDB GraphQL schemas used by
`gents`. The agent collection schemas live in the dependency-free
`gents-schemas` crate so external document-peer consumers can depend on
the same collection contract without pulling in runtime/protocol dependencies.

The schema is intentionally document-oriented. The runtime resolves behavior and
execution state from documents, then publishes operational state back into
documents for inspection and debugging.

This file is the quick map of what each collection means, how collections relate
to each other, and which subsystem writes them.

## High-Level Shape

```text
AgentPrincipal
  -> default_behavior_id -> AgentBehavior

AgentBehavior
  -> backend_id -> InferenceBackend
  -> tool_selection_id -> ToolSelection
  -> inference_profile_id -> InferenceProfile

AgentPrincipal / AgentBehavior
  -> authoritative admission -> AgentBehaviorReadiness
  -> reconcile diagnostics -> AgentRuntime

Interactive execution:
  AgentRequest -> AgentResponse
               -> InferenceCall
               -> AgentSession
               -> AgentConversation
               -> AgentMessage
               -> AgentToolCall
               -> AgentToolApproval
               -> AgentToolResult
               -> CompactionEntry
               -> RenderedRequest

Scheduled and event-driven execution:
  Task         -> behavior_id -> AgentBehavior
  Schedule     -> task_id     -> Task
  EventTrigger -> task_id     -> Task

Remote tool discovery:
  ToolServiceRegistry
```

## Collection Groups

### Agent Configuration

These documents describe what the agent is and how it should run.

| Collection | Key fields | References | Written by | Read by |
|------------|------------|------------|------------|---------|
| `AgentPrincipal` | `agent_did`, `default_behavior_id`, `enabled` | `default_behavior_id -> AgentBehavior.behavior_id` | `init`, config/bootstrap code | document boot, reconcile, scheduler/task defaulting |
| `AgentBehavior` | `behavior_id`, `agent_did`, `backend_id`, `model_name`, `tool_selection_id`, `inference_profile_id`, `enabled` | `backend_id -> InferenceBackend.backend_id`, `tool_selection_id -> ToolSelection.selection_id`, `inference_profile_id -> InferenceProfile.profile_id` | `init`, `config behavior set`, library builder/document bootstrap | runtime resolution, request routing, scheduler |
| `ToolSelection` | `selection_id`, `agent_did`, file/bash/meta/delegate fields, MCP service allowlist, command execution policy fields (`command_allowed_argv_prefixes` extends via argv prefixes; `read_only_command_allowlist` replaces/narrows the ReadOnly base — see `docs/macos-bash-sandbox.md`) | selected by `AgentBehavior.tool_selection_id` | `init`, `config tools set` | tool-surface resolution |
| `InferenceBackend` | `backend_id`, `provider_kind`, `endpoint`, `api_key`, `api_key_env_var`, `max_concurrent`, `max_queue_depth`, `enabled`, `models`, `probe_status` | selected by `AgentBehavior.backend_id` | `init`, `config backend set`, desired-state manifests, health/probe updates | startup readiness, runtime execution, scheduler execution |
| `InferenceProfile` | `profile_id`, context/output/temperature/deadline fields | selected by `AgentBehavior.inference_profile_id` | `config profile set` | runtime resolution |

### Runtime Observability

These documents expose what the runtime is doing right now.

| Collection | Key fields | Meaning | Written by | Read by |
|------------|------------|---------|------------|---------|
| `AgentRuntime` | `agent_did`, `reconcile_phase`, executor capacity/queue fields, `last_reconcile_result`, `last_reconcile_error` | reconcile and executor diagnostics for one agent principal | runtime reconcile/executor instrumentation | diagnostics and metrics |
| `AgentBehaviorReadiness` | `agent_did`, `snapshot_json`, `updated_at` | authoritative process, generation, default-behavior, and per-behavior admission state | ordered runtime readiness owner | routing, health, status, clients |

### Interactive Conversation State

These documents record user requests, assistant output, and conversation history.

| Collection | Key fields | References | Written by | Read by |
|------------|------------|------------|------------|---------|
| `AgentRequest` | `request_id`, `agent_did`, `behavior_id`, `session_id`, sampling overrides, `metadata`, `status`, `lifecycle_state`, `backend_id`, `failure_reason`, `interrupt_requested_at`, `valid_until` | `_docID` is the authoritative request identity; logical retry, supersession, and subagent fields have matching physical request/tool-call document edges | `chat`, `request submit`, lifecycle transitions | router, CLI inspection, recovery |
| `InferenceCall` | `call_id`, `request_id`, `request_doc_id`, `backend_id`, `call_kind`, `call_state`, queue/timing/token fields | belongs to an exact physical request document/backend; `request_id` remains correlation-only | admission controller at terminal call state | benchmarking, RL reward shaping, debugging |
| `AgentResponse` | `request_id`, `request_doc_id`, `agent_did`, `behavior_id`, `session_id`, `status`, `content`, `reasoning`, `error_message`, `progress_seq`, `materialized_message_sequence`, `interrupted_at` | latest response for an exact physical request document; also the in-flight streaming overlay until committed into transcript | streaming/runtime code | `chat`, `response show`, `response wait`, TUI, rich clients |
| `AgentSession` | `session_id`, `behavior_id`, `status`, `started`, `ended` | ties a sequence of requests to one behavior | session manager | `chat`, inspection, recovery |
| `AgentConversation` | `session_id`, `agent_did`, `behavior_id`, `title`, `preview_text`, `status`, `latest_request_id` | high-level conversation summary per session | session/conversation layer | UI and inspection |
| `AgentMessage` | `message_key`, `session_id`, `request_id`, `request_doc_id`, `sequence`, `role`, `content`, `timestamp` | ordered transcript facts bound to the exact request document that produced them | session/history layer | chat history, timeline, adapters, debugging |
| `AgentToolCall` | `tool_call_key`, `request_id`, `request_doc_id`, `session_id`, `tool_name`, `tool_call_id`, `args`, `result`, `status`, trace enrichment fields | concrete tool invocation facts bound to their exact request document | runtime/tool persistence | chat progress, timeline, adapters, diagnostics |
| `AgentToolApproval` | `approval_id`, `tool_call_doc_id`, `tool_call_id`, `request_id`, `decision`, `approver_did` | decision bound to the exact tool-call document; logical IDs remain correlation fields | approval lifecycle | tool execution and audit projections |
| `AgentToolResult` | `tool_call_doc_id`, `agent_did`, `session_id`, `tool_name`, `tool_input`, `output_text`, `truncated`, `discarded_because_interrupted` | normalized or spilled result bound to the exact tool-call document | tool persistence hook | transcript/tool-output loading and inspection |
| `CompactionEntry` | `compaction_key`, `session_id`, `request_id`, `request_doc_id`, `summary`, `messages_compacted`, token counts | transcript-reduction fact bound to the exact request document active during compaction | compaction layer | session reconstruction, timeline, and debugging |
| `RenderedRequest` | `capture_key`, `request_doc_id`, `request_commit_cid`, `request_id`, `session_id`, `capture_scope`, `turn_index`, `attempt`, `request_json`, `provenance_json` | one durable fact per provider attempt: the exact HTTP request body and exact DefraDB request version current before send; DefraDB CIDs provide integrity without shadow payload hashes | `rendered_request::transport::RenderedRequestCapturingHttpClient`, the innermost transport in every provider stack, through `rendered_request::sink::DefraRenderedRequestSink` (installed by default) | trace projections, capture-verified reconstruction |

### Tasks, Schedules, and Event Triggers

| Collection | Key fields | References | Written by | Read by |
|------------|------------|------------|------------|---------|
| `Task` | `task_id`, `name`, `behavior_id`, `prompt_template`, optional `goal_objective_template` / `goal_token_budget`, `enabled`, `output_schema_ref` | `behavior_id -> AgentBehavior.behavior_id` | desired-state apply | trigger engine |
| `Schedule` | `schedule_id`, `task_id`, `interval_secs`, `enabled`, `concurrency`, `next_run_at`, `last_attempt_at`, `last_status`, `fire_count` | `task_id -> Task.task_id` | desired-state apply, trigger engine status updates | trigger engine |
| `EventTrigger` | `trigger_id`, `task_id`, `source_collection`, `event_kind`, `filter`, `enabled`, `concurrency`, `last_attempt_at`, `last_fired_source_doc_id`, `last_status`, `fire_count` | `task_id -> Task.task_id` | desired-state apply, trigger engine status updates | event source / trigger engine |
| `EventTriggerGroupState` | `group_key`, `trigger_id`, `correlation`, `trigger_config_key`, `first_seen_at` | internal durable timeout clock; successful resolution remains an `AgentRequest` lineage query | event source | event source |

`Task.behavior_id` is concrete and mandatory. A `Schedule` references the `Task`
it fires; the trigger engine materializes `AgentRequest` rows from due
`Schedule`s.

An `EventTrigger` also references a `Task`, but fires from document events on a
declared `source_collection`. Desired-state validation probes the live DefraDB
schema for the source collection, validates the trigger filter, and resolves
`doc.*` template references in the target task before apply succeeds.

### Tool Service Discovery

| Collection | Key fields | Meaning | Written by | Read by |
|------------|------------|---------|------------|---------|
| `ToolServiceRegistry` | `service_id`, `hostname`, `tailscale_ip`, `lan_ip`, `mcp_port`, `mcp_path`, `status`, `version`, `updated_at` | registry entries for discoverable MCP-style tool services | desired-state apply, service registry writers | meta-tools and discovery flows |

`ToolServiceRegistry` desired-state owns the identity and endpoint fields:
`service_id`, `display_name`, `description`, `hostname`, `tailscale_ip`,
`lan_ip`, `mcp_port`, and `mcp_path`. Desired-state apply normalizes missing or
null address fields to empty strings, defaults missing or empty `mcp_path` to
`/mcp`, and creates rows with `status: "online"` so they are discoverable.
`version` and `updated_at` are runtime-owned and may be null on rows created by
apply. Tool lists are discovered from the MCP service at runtime; the schema does
not expose a persisted `tools` relation.

## Operational Relationships

### Configuration Resolution

The runtime resolves a runnable behavior by following this chain:

1. Load `AgentPrincipal` for `agent_did`
2. Choose the principal’s `default_behavior_id` or an explicit `behavior_id`
3. Load `AgentBehavior`
4. Load `InferenceBackend`
5. Load `ToolSelection`
6. Optionally load `InferenceProfile`
7. Intersect behavior-selected tools with the operator `ToolCeiling`
8. Publish `AgentBehaviorReadiness` and reconcile diagnostics

If the backend is missing, disabled, or unhealthy, the behavior is unrunnable.

### Interactive Request Flow

The normal CLI path is:

1. `chat` or `request submit` writes `AgentRequest`
2. runtime claims and executes the request
3. the default rendered-request capture sink persists each exact provider-bound request body and the current request commit before streaming starts
4. streaming writes `AgentResponse`
5. transcript/session layers write `AgentSession`, `AgentConversation`, `AgentMessage`
6. once the final assistant message is committed, `AgentResponse.materialized_message_sequence`
   points at the committed `AgentMessage.sequence`
7. tool activity writes `AgentToolCall` and `AgentToolResult`

### Reconcile

Live reconcile is driven by changes to configuration documents:

- `AgentPrincipal`
- `AgentBehavior`
- `ToolSelection`
- `InferenceProfile`
- referenced `InferenceBackend`

The runtime publishes authoritative lifecycle and generation state through
`AgentBehaviorReadiness`; `AgentRuntime` records only reconcile and executor
diagnostics.

## Branchable vs Non-Branchable

Shared configuration and durable fact collections are marked `@branchable`,
including:

- `AgentConversation`
- `AgentPrincipal`
- `AgentBehavior`
- `AgentMemory`
- `AgentMessage`
- `AgentRequest`
- `AgentResponse`
- `AgentSession`
- `AgentToolApproval`
- `AgentToolCall`
- `AgentToolResult`
- `CompactionEntry`
- `RenderedRequest`
- `InferenceCall`
- `InferenceProfile`
- `ToolSelection`
- `Skill`
- `DatastoreToolSurface`
- `ToolServiceRegistry`
- `ProjectionAcpBinding`
- `Task`
- `Schedule`
- `EventTrigger`
- `EventTriggerGroupState`
- the enrollment authorization, applied-route, peer-registry, and remote
  configuration ledgers

These are the documents where preserving observable history matters most.

`@branchable` is not what gives a field its content address — DefraDB writes
per-field and composite commit blocks unconditionally, and `is_branchable` gates
only the extra collection-level block. It is irreversible (DefraDB rejects every
patch that flips it, and a populated collection cannot be dropped and
recreated), and it is the precondition for branchable collection sync and for
collection-scoped ACP read decisions. Choose it when the collection is created
or never.

Host-local state and secrets are not branchable:

- `InferenceBackend`
- `OAuthCredential`
- `WorkspaceRoot`
- `AgentRuntime`
- `ToolServiceHealthState`
- local pairing and replication intents

Those documents describe one host's endpoint, credential, filesystem, process,
health, or reconciliation state rather than portable agent facts.

## Source of Truth Boundaries

Some boundaries are deliberate:

- `ToolSelection` is the behavior-selected tool surface.
- `ToolSelection.allowed_mcp_service_ids` optionally narrows meta-tools to a
  behavior-specific set of MCP service IDs. Missing or empty means all online
  `ToolServiceRegistry` services remain visible for backward compatibility.
- `ToolSelection.required_mcp_service_ids` declares services that must be
  measured available before a behavior using the selection is runnable.
- `ToolCeiling` is not stored here; it is an operator safety cap applied at
  runtime.
- Command execution policy lives on `ToolSelection`: `command_execution_policy`
  accepts `read_only`, `workspace_write`/`managed_write`, or `unrestricted`;
  `command_allowed_argv_prefixes` and `command_forbidden_argv_prefixes` refine
  argv-level allow/deny behavior; `command_network_mode` is an optional network
  policy hint. In `read_only` mode, an allowed argv prefix can authorize an
  operator-configured diagnostic command outside the built-in read-only
  allowlist. When the allowed-prefix list is non-empty, it remains a global argv
  gate for all commands. Runtime enforcement still depends on the selected bash
  mode and host platform. On macOS, `workspace_write` uses the seatbelt sandbox
  and only permits same-sandbox process introspection; `unrestricted` is
  unsandboxed and is the policy to use for host-diagnostics stewards that need
  `ps` or broad `lsof`.
- backend credentials may currently be stored either directly in
  `InferenceBackend.api_key` or indirectly via `InferenceBackend.api_key_env_var`.
- backend capability metadata is not stored in `InferenceBackend`; provider
  behavior is delegated to rig and deprecated manifest/import fields are
  ignored during config migration.
- `AgentBehaviorReadiness` is the sole runtime lifecycle/admission authority.
  `AgentRuntime` is diagnostic observability only, not desired configuration.

## Where Schemas Are Registered

The runtime registers schemas from `crates/gents/src/schema.rs`, via
`gents_protocol::schemas`.

That file is the authoritative list of which SDL files are loaded into the
embedded DefraDB node.
