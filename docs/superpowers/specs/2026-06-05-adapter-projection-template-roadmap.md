# Adapter, projection, and pattern-template roadmap

Date: 2026-06-05
Status: implementation roadmap
Tracking issue: https://github.com/sourcenetwork/defra-agent/issues/407
Related:

- `docs/superpowers/specs/2026-06-05-protocol-product-positioning-map.md`
- `docs/superpowers/audits/2026-06-05-protocol-product-positioning-verification.md`

## Goal

Turn Defra Agent's positioning into concrete product work.

The product promise is that customers can use familiar protocol and framework
surfaces while Defra Agent remains the durable, permissioned, document-native
source of truth for identity, work, tools, state, lineage, and audit.

This roadmap treats the following as required product capabilities:

- protocol adapters for A2A, external ACP, MCP, and ANP-style discovery;
- projections for run timelines, trace/event export, catalogs, provenance, and
  shared memory;
- pattern templates for common orchestration shapes customers know from
  LangGraph, CrewAI, OpenAI Agents SDK, and Microsoft Agent Framework.

## Prioritization principle

Build the lowest-risk surfaces first: projections over existing Defra documents,
then protocol adapters over those projections, then higher-level templates.

This avoids creating protocol-specific business logic. The runtime remains
Defra-native; adapters translate edge shapes into the same `AgentRequest`,
`AgentMessage`, `AgentToolCall`, `AgentResponse`, session, task, and behavior
documents.

## Phase 0: mapping contracts and conformance

### Deliverables

- A mapping contract for each external surface:
  - A2A task/session/event fields to Defra request/session/message/tool-call
    fields.
  - ACP run/session/lifecycle fields to Defra request/session/lifecycle fields.
  - MCP tool/resource/prompt surfaces to Defra tool-service and policy fields.
  - ANP/DID capability documents to Defra principal/behavior/task metadata.
- State-mapping conformance tests:
  - A2A task states to `AgentRequest.lifecycle_state`.
  - ACP Await/cancel to Defra interrupt/cancel/lifecycle fields.
  - tool-call denial to protocol error/event surfaces.
  - child request/subagent lineage to protocol artifacts or events.

### Definition of done

- Every adapter has a small table of field mappings.
- Every lifecycle transition used by an adapter has a conformance test.
- Unsupported protocol states are explicitly rejected or mapped to a known
  Defra-native pending/error state.

## Phase 1: core projections

### 1. Run timeline projection

Reconstruct a single run from:

- `AgentRequest`
- `AgentMessage`
- `AgentToolCall`
- `AgentResponse`
- policy denial fields
- child requests and subagent lineage
- session/conversation metadata

Definition of done:

- One stable internal run-timeline DTO.
- CLI/API endpoint to fetch a timeline by request id.
- Includes parent/child lineage and policy-denial events.
- Serves as the source for trace export, A2A events, and ACP events.

### 2. Agent catalog projection

Expose an ACL-filtered catalog of:

- principals
- behaviors
- tasks
- skills
- tool selections
- MCP services
- schedules and triggers

Definition of done:

- Public/private filtering is explicit.
- Sensitive fields such as internal URLs, private skill text, or restricted
  tools can be hidden by DefraDB ACP.
- Catalog output can feed A2A Agent Cards and ACP manifests.

### 3. Tool provenance projection

For every tool call, expose:

- calling `agent_did`
- behavior id
- request id/session id
- selected service id
- selected tool name
- policy decision and denial reason
- output/error summary
- child request id when the call spawned a subagent

Definition of done:

- Tool provenance appears in run timelines.
- Tool provenance can be exported independently for audit.
- Cross-deployment tool/subagent calls preserve remote identity metadata.

### 4. Shared-memory provenance projection

For agent-written shared documents, expose:

- writer DID
- behavior id
- originating request id
- originating tool-call id when applicable
- timestamp
- review/trust/poisoning status

Definition of done:

- New or existing agent-written documents can be traced back to runtime origin.
- A reviewer can distinguish user-written, operator-written, and agent-written
  documents.
- Poisoning/review status is represented as document metadata or a related
  review document.

## Phase 2: first protocol adapters

### 1. A2A Agent Card projection

Generate A2A Agent Cards from:

- `AgentPrincipal`
- `AgentBehavior`
- `Task`
- `ToolSelection`
- `ToolServiceRegistry`
- skills

Definition of done:

- Public Agent Card endpoint.
- Private/internal card view for authorized operators.
- Supported interfaces/endpoints are generated from deployment configuration.
- A2A Agent Card JWS support is available where signed cards are needed.

### 2. A2A task lifecycle adapter

Map A2A `SendMessage` into Defra work:

- new `SendMessage` work creates an `AgentRequest`;
- generated Defra `request_id` is exposed as returned `taskId`;
- `contextId` maps to `session_id`;
- incoming `taskId` resumes/refines existing request/task work;
- lifecycle states map to A2A task states;
- messages, responses, tool calls, and child requests stream as task
  events/artifacts.

Definition of done:

- A2A clients can discover an agent and send work.
- A2A task state is derived from Defra lifecycle state.
- A2A event streams are generated from the run timeline projection.
- Creation-vs-continuation semantics are tested.

### 3. Shared ACP/A2A run mapper

Build one internal mapper for task/run/session concepts, then expose
protocol-specific HTTP/JSON edge shapes.

Definition of done:

- ACP run creation maps to `AgentRequest`.
- ACP sessions map to `AgentSession`.
- ACP Await maps to interrupt/input-required flows.
- ACP cancel maps to cancel fields.
- ACP run events are generated from the same run timeline projection used by
  A2A.

## Phase 3: identity and discovery adapters

### 1. Protocol auth to Defra identity bridge

Translate external protocol auth into a Defra actor/principal.

Definition of done:

- A2A/ACP/ANP request identity is resolved before DB work.
- DefraDB ACP remains the final permission decider.
- Adapter-local auth decides endpoint access only; document/capability access is
  still DefraDB ACP.
- All protocol-created requests record the actor identity used for DB work.

### 2. ANP/DID discovery adapter

Publish DID-bound agent description/capability metadata from Defra documents.

Definition of done:

- Agent DID verification is defined.
- Capability documents can be signed or otherwise verified.
- Public/private publication rules are enforced through DefraDB ACP.
- Metadata publication is minimized by default.
- The adapter does not invent a second authorization model.

## Phase 4: MCP projections

### 1. Defra-backed MCP server

Expose Defra-backed resources/tools/prompts through MCP.

Candidate MCP resources:

- agent catalog
- task catalog
- run timeline
- tool provenance
- shared-memory provenance
- allowed document collections

Candidate MCP tools:

- `defra_query`
- `get_agent_status`
- `get_run_timeline`
- `request_agent_work`
- `cancel_agent_work`
- `list_allowed_tools`

Candidate MCP prompts:

- behavior prompts where the actor is authorized to inspect them
- task-specific operating instructions
- reviewer prompts for human-in-the-loop workflows

Definition of done:

- MCP exposure is ACL-filtered.
- MCP tools execute as the resolved actor/principal.
- Tool calls are persisted as `AgentToolCall` rows when invoked through Defra.
- The MCP server can be used by external frameworks without bypassing DefraDB
  ACP.

## Phase 5: missing document shapes

### 1. Approval/review records

Current Defra-native state has interrupt/cancel/policy-denial fields, but
OpenAI/Microsoft/A2A-style approval flows need a first-class review surface.

Proposed document shape:

- approval id
- request id
- tool-call id when applicable
- requesting agent DID
- behavior id
- requested action
- risk/policy reason
- approver identity
- decision: pending/approved/rejected/expired
- decision timestamp
- resume token or continuation reference

Definition of done:

- risky tool calls can pause before side effects.
- approval/rejection resumes or terminates the same request lineage.
- approval decisions appear in run timelines and protocol event streams.

### 2. Shared-memory review/trust records

Agent-written shared documents need review and poisoning controls.

Definition of done:

- agent-written docs have origin metadata.
- reviewers can mark trust/rejection/quarantine status.
- downstream agents can filter shared memory by trust status and provenance.

## Phase 6: pattern templates

Templates should be declarative Defra manifests, not cloned framework APIs.

### 1. Handoff specialist

One agent delegates ownership to another agent/behavior.

Maps to:

- OpenAI handoffs
- A2A delegation
- Microsoft handoff orchestration

Definition of done:

- parent request records child request id;
- ownership transfer is visible in run timeline;
- final response attribution is clear.

### 2. Manager-as-tools

A supervisor agent calls specialist agents as bounded capabilities while
keeping final response ownership.

Maps to:

- OpenAI agents-as-tools
- supervisor/worker workflows

Definition of done:

- specialist calls are persisted as tool calls or child requests;
- supervisor remains final response owner;
- each specialist call has isolated tool policy.

### 3. Sequential pipeline

Step A -> B -> C with persisted intermediate documents and lifecycle gates.

Definition of done:

- each step is a task/behavior invocation;
- intermediate outputs are documents;
- retries and cancellation are per step and visible in the parent timeline.

### 4. Concurrent fanout/fanin

Spawn multiple child requests and aggregate responses under a parent.

Definition of done:

- child requests share parent lineage;
- aggregation waits for configured terminal states;
- partial failure behavior is explicit.

### 5. Group chat / Magentic-style coordination

Multiple agents share a session with turn policy, manager selection, and
termination criteria.

Definition of done:

- each turn is persisted as a message/request update;
- manager/selection policy is declarative;
- termination condition is explicit and auditable.

### 6. CrewAI-style Flow/Crew

Flow template for structured state transitions; Crew template for
role-specialized behaviors and delegation.

Definition of done:

- flow state is a document;
- transitions are driven by task completion, schedule, or event triggers;
- crew roles are `AgentBehavior` plus `ToolSelection` plus skills.

### 7. LangGraph-style durable graph

Nodes are behaviors/tasks; edges are document-triggered transitions; checkpoints
are Defra documents.

Definition of done:

- graph state can be resumed from persisted documents;
- each node execution is an `AgentRequest`;
- edge decisions are recorded in the timeline.

### 8. Human approval

Interrupt before risky tool calls, persist approval request, then resume or
deny.

Definition of done:

- approval record exists;
- protocol adapters expose input-required/Await/review states;
- approval appears in timeline and trace export.

### 9. Scheduled agent

`Schedule -> AgentRequest -> run timeline`.

Definition of done:

- schedule fire creates a request with schedule provenance;
- retries/deadlines are explicit;
- timeline includes the schedule source.

### 10. Event-triggered agent

`EventTrigger -> AgentRequest`, with source event provenance and idempotency.

Definition of done:

- source event id is recorded;
- duplicate event behavior is defined;
- timeline includes trigger source and causality.

### 11. Cross-deployment subagent

Parent request delegates to a remote deployment.

Definition of done:

- remote identity and request id are recorded;
- parent/child lifecycle is visible;
- cancellation and terminal-state propagation are tested.

## Recommended first implementation slice

1. Run timeline projection.
2. A2A Agent Card projection.
3. A2A task lifecycle adapter.
4. Approval/review record shape.
5. Handoff specialist and manager-as-tools templates.

This slice gives customers the clearest proof of the Defra thesis:

- agents can be discovered;
- work can enter through a standard protocol;
- runtime state remains Defra-native;
- approvals and lineage are visible;
- familiar orchestration patterns exist without replacing Defra's document
  substrate.

## Implementation status

Started 2026-06-05:

- Run timeline projection now has a reusable library DTO/builder in
  `crates/defra-agent/src/run_timeline.rs`.
- The projection is exposed publicly from `defra-agent` as
  `build_run_timeline`, `RunTimeline`, `RunTimelineRows`, and row/event DTOs.
- The CLI has an initial fetch surface:
  `defra-agent trace timeline --request-id REQUEST_ID`.
- The CLI timeline loader reads `AgentRequest`, `AgentSession`,
  `AgentConversation`, `AgentMessage`, `AgentToolCall`, and `AgentResponse`
  rows, then emits a stable timeline JSON document.
- Focused tests cover the pure builder and embedded-DefraDB CLI path.

Still pending for item 1:

- HTTP/API endpoint for runtime consumers that should not shell out to the CLI.
- Explicit adapter-facing event views for A2A task events and ACP run events.
- Stricter multi-request session scoping rules if future adapters need a
  narrower view than the current request/session reconstruction.
