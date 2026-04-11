# Schema Data Model

This directory contains the DefraDB GraphQL schemas used by `defra-agent`.

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
  -> runtime publication -> AgentRuntime

Interactive execution:
  AgentRequest -> AgentResponse
               -> AgentSession
               -> AgentConversation
               -> AgentMessage
               -> AgentToolCall
               -> AgentToolResult
               -> CompactionEntry

Scheduled execution:
  ScheduledTask -> behavior_id -> AgentBehavior

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
| `ToolSelection` | `selection_id`, `agent_did`, file/bash/meta/delegate fields | selected by `AgentBehavior.tool_selection_id` | `init`, `config tools set` | tool-surface resolution |
| `InferenceBackend` | `backend_id`, `endpoint`, `api_key_env_var`, `max_concurrent`, `enabled`, `models`, `probe_status` | selected by `AgentBehavior.backend_id` | `init`, `config backend set`, health/probe updates | startup readiness, runtime execution, scheduler execution |
| `InferenceProfile` | `profile_id`, context/output/temperature/deadline fields | selected by `AgentBehavior.inference_profile_id` | `config profile set` | runtime resolution |

### Runtime Observability

These documents expose what the runtime is doing right now.

| Collection | Key fields | Meaning | Written by | Read by |
|------------|------------|---------|------------|---------|
| `AgentRuntime` | `agent_did`, `process_state`, `reconcile_phase`, `active_generation`, `router_generation`, `last_reconcile_result` | current runtime/reconcile state for one agent principal | runtime startup/reconcile/shutdown code | `status`, `show runtime`, debugging |

### Interactive Conversation State

These documents record user requests, assistant output, and conversation history.

| Collection | Key fields | References | Written by | Read by |
|------------|------------|------------|------------|---------|
| `AgentRequest` | `request_id`, `agent_did`, `behavior_id`, `session_id`, `status`, `lifecycle_state`, `admission_state`, `backend_id`, `failure_reason` | belongs to an agent/session/behavior | `chat`, `request submit`, lifecycle transitions | router, CLI inspection, recovery |
| `AgentResponse` | `request_id`, `agent_did`, `behavior_id`, `session_id`, `status`, `content`, `reasoning`, `error_message`, `progress_seq` | latest response for a request | streaming/runtime code | `chat`, `response show`, `response wait`, TUI |
| `AgentSession` | `session_id`, `behavior_id`, `status`, `started`, `ended` | ties a sequence of requests to one behavior | session manager | `chat`, inspection, recovery |
| `AgentConversation` | `session_id`, `agent_did`, `behavior_id`, `title`, `preview_text`, `status`, `latest_request_id` | high-level conversation summary per session | session/conversation layer | UI and inspection |
| `AgentMessage` | `message_key`, `session_id`, `sequence`, `role`, `content`, `timestamp` | ordered transcript entries | session/history layer | chat history, TUI, debugging |
| `AgentToolCall` | `tool_call_key`, `session_id`, `tool_name`, `tool_call_id`, `args`, `result`, `status` | concrete tool invocation records within a session | runtime/tool persistence | chat progress, TUI, diagnostics |
| `AgentToolResult` | `agent_did`, `session_id`, `tool_name`, `tool_input`, `output_text`, `truncated` | normalized tool result persistence | tool persistence hook | compaction and later inspection |
| `CompactionEntry` | `compaction_key`, `session_id`, `summary`, `messages_compacted`, token counts | persisted compaction summaries | compaction layer | session reconstruction and debugging |

### Scheduled Execution

| Collection | Key fields | References | Written by | Read by |
|------------|------------|------------|------------|---------|
| `ScheduledTask` | `task_id`, `agent_did`, `behavior_id`, `prompt`, `interval_secs`, `next_run_at`, `last_status` | `behavior_id -> AgentBehavior.behavior_id` | `config task set`, scheduler status updates | scheduler loop |

`ScheduledTask.behavior_id` is concrete and mandatory. The scheduler does not do
legacy name fallback anymore.

### Tool Service Discovery

| Collection | Key fields | Meaning | Written by | Read by |
|------------|------------|---------|------------|---------|
| `ToolServiceRegistry` | `service_id`, `hostname`, `mcp_port`, `tools`, `status`, `updated_at` | registry entries for discoverable MCP-style tool services | service registry writers | meta-tools and discovery flows |

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
8. Publish `AgentRuntime`

If the backend is missing, disabled, or unhealthy, the behavior is unrunnable.

### Interactive Request Flow

The normal CLI path is:

1. `chat` or `request submit` writes `AgentRequest`
2. runtime claims and executes the request
3. streaming writes `AgentResponse`
4. transcript/session layers write `AgentSession`, `AgentConversation`, `AgentMessage`
5. tool activity writes `AgentToolCall` and `AgentToolResult`

### Reconcile

Live reconcile is driven by changes to configuration documents:

- `AgentPrincipal`
- `AgentBehavior`
- `ToolSelection`
- `InferenceProfile`
- referenced `InferenceBackend`

The runtime republishes `AgentRuntime` as it resolves, applies, and activates a
new generation.

## Branchable vs Non-Branchable

Several operational collections are marked `@branchable`:

- `AgentConversation`
- `AgentMessage`
- `AgentRequest`
- `AgentResponse`
- `AgentRuntime`
- `AgentSession`
- `AgentToolCall`
- `AgentToolResult`
- `CompactionEntry`
- `ScheduledTask`

These are the documents where preserving observable history matters most.

The core configuration collections are not branchable:

- `AgentPrincipal`
- `AgentBehavior`
- `ToolSelection`
- `InferenceBackend`
- `InferenceProfile`

Those are treated as current desired state rather than append-only history.

## Source of Truth Boundaries

Some boundaries are deliberate:

- `ToolSelection` is the behavior-selected tool surface.
- `ToolCeiling` is not stored here; it is an operator safety cap applied at
  runtime.
- backend credentials are not stored directly in documents; only
  `InferenceBackend.api_key_env_var` is stored.
- `AgentRuntime` is the runtime’s published observability surface, not desired
  configuration.

## Where Schemas Are Registered

The runtime registers schemas from [schema.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent/src/schema.rs).

That file is the authoritative list of which SDL files are loaded into the
embedded DefraDB node.
