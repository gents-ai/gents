# Schema Data Model

Gents stores configuration, requests, sessions, tools, automation, and runtime
observations as DefraDB documents. Agent collection schemas live in the
dependency-free `gents-schemas` crate. This directory contains the remaining
protocol schemas.

The canonical configuration graph is:

```text
AgentPrincipal
  -> default_behavior_id -> AgentBehavior

AgentBehavior
  -> context_id           -> AgentContext
  -> inference_profile_id -> InferenceProfile

AgentContext
  -> tools_id      -> Tools
  -> compaction_id -> CompactionConfig
  -> skill_ids     -> Skill

Tools
  -> subagents.target_ids                 -> SubagentTarget
  -> remote.services[].mcp_service_id     -> ToolServiceRegistry
  -> datastore.datastore_tool_surface_ids -> DatastoreToolSurface
  -> integrations.eth_tool_ids            -> EthTool

InferenceProfile
  -> backend_id   -> InferenceBackend
  -> sampling_id  -> InferenceSampling
  -> execution_id -> InferenceExecution

InferenceExecution
  -> retry_policy_id -> InferenceRetryPolicy
```

Configuration references are owner scoped unless the target document explicitly
models a foreign principal, as `SubagentTarget.target_agent_did` does. Desired
state publishes a complete retained owner graph atomically and validates every
reference before commit. Tags are discovery metadata and never references,
permissions, or execution selectors.

`AgentBehavior` has one context selection and one inference selection. Literal
instructions, skills, tools, and compaction belong to `AgentContext` and its
referenced documents. Provider, model, sampling, execution bounds, and retries
belong to the inference documents. Backend authentication is the tagged
`InferenceBackend.auth` value; competing raw-key and environment-key fields do
not exist.

The runtime resolves a behavior in this order:

1. Load the `AgentPrincipal` for the actor DID.
2. Select its default behavior or the request's explicit `behavior_id`.
3. Resolve the behavior's `AgentContext` graph.
4. Resolve its `InferenceProfile` graph.
5. Intersect resolved tools with the host's `ToolCeiling`.
6. Publish readiness through `AgentBehaviorReadiness`.

`AgentRuntime` carries reconcile and executor diagnostics. It is not desired
configuration or admission authority.

## Requests and sessions

```text
AgentRequest -> AgentResponse
             -> InferenceCall
             -> AgentMessage
             -> AgentToolCall -> AgentToolResult
             -> CompactionEntry
             -> RenderedRequest

AgentSession -> ordered request and transcript observations
```

`AgentRequest.lifecycle_state` is the only request lifecycle state. Its
`input` JSON is the strict `RequestInput` value for selected skills, cwd,
initial title, queue facts, and authenticated goal-continuation facts. Sampling,
provider selection, and arbitrary metadata are not request inputs. Physical
request document IDs bind responses, messages, tool calls, retries, and
subagent lineage to the exact request instance.

`AgentSession` is the single durable session document. It owns session identity,
requester scope, behavior selection, creation and close times, title, tags,
provenance, and a compact observation. Request execution state remains on
`AgentRequest`.

`AgentMessage` is the ordered durable transcript. `AgentResponse` is the latest
response and streaming overlay until its final message is materialized.
`InferenceCall` records exact backend admission, timing, and token facts.
`RenderedRequest` captures the exact provider-bound payload and current request
commit before send.

## Tasks and automation

```text
Task     -> behavior_id -> AgentBehavior
Schedule -> task_id     -> Task
Trigger  -> task_id     -> Task
          source.schedule_id     -> Schedule
          source.event_source_id -> EventSource
```

`Task` owns its prompt template and optional goal/output contract. `Schedule`
owns cadence. `EventSource` owns event observation and grouping configuration;
`Trigger` binds a source to a task and owns delivery/concurrency policy. Runtime
attempt and delivery observations remain separate from desired configuration.

## Authorization and storage

DefraDB authenticates actors as DIDs and enforces document access through ACP.
Gents does not introduce a second principal or authorization namespace.
Collections that replicate as durable configuration or facts are branchable;
host-local endpoints, credentials, filesystem placement, health, and process
observations remain local.

Empty nillable lists must be written as `null`, never `[]`. GraphQL strings must
be escaped with the shared GraphQL helpers. The runtime schema registration path
and `gents-schemas` constants are authoritative for the installed catalog; this
guide describes ownership and relationships rather than duplicating field lists.
