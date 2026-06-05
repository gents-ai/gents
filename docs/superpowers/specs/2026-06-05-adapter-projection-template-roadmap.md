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

The projection layer should be adapter-driven, not designed in isolation. The
shared layer is justified only by real interoperability work. Build the first
three customer-facing adapter projections first, then keep only the common
projection machinery those adapters actually need.

This roadmap treats the following as required product capabilities:

- projections for run timelines, trace/event export, training/eval extraction,
  catalogs, provenance, and shared memory;
- interoperability adapters for OpenAI/Codex-style traces, LangGraph-style
  state/history, and multi-agent task frameworks such as AutoGen or CrewAI;
- protocol adapters for A2A, external ACP, MCP, and ANP-style discovery;
- pattern templates for common orchestration shapes customers know from
  LangGraph, CrewAI, OpenAI Agents SDK, and Microsoft Agent Framework.

## Prioritization principle

Build the lowest-risk surfaces first, but make them prove real interop:

1. Reconstruct Defra-native run timelines from persisted runtime documents.
2. Project the same timeline into the first three external adapter shapes:
   OpenAI/Codex-style run traces, LangGraph-style state/history, and
   multi-agent task/delegation views.
3. Extract the shared projection layer only from those concrete adapters.
4. Add protocol adapters and pattern templates over the proven projection
   layer.

This avoids creating protocol-specific business logic. The runtime remains
Defra-native; adapters translate edge shapes into the same `AgentRequest`,
`AgentMessage`, `AgentToolCall`, `AgentResponse`, session, task, and behavior
documents.

## Projection framework acceptance criteria

The first shared projection framework should exist only when the three initial
adapters need the same behavior. Required shared pieces:

- projection identity and versioning;
- request/session/conversation selectors;
- common runtime row bundle loading;
- projection context with actor identity;
- redaction modes for full, training-safe, and public outputs;
- provenance metadata linking exported rows back to Defra runtime documents;
- JSON and JSONL writers;
- schema or snapshot validation for each adapter output;
- binary E2E coverage using the real `defra-agent` executable;
- optional live-inference coverage that validates persisted state and adapter
  output without depending on exact model prose.

## Adapter-driven first three

### 1. OpenAI/Codex-style run trace adapter

Purpose:

- customer-familiar trace/event output;
- training/eval trajectory extraction;
- proof that messages, tool calls, responses, reasoning-ish fields, errors,
  and JSON/JSONL exports can be reconstructed from Defra documents.

Definition of done:

- output is generated from the run timeline projection;
- full and redacted modes are tested;
- tool calls preserve arguments, outputs, status, and child-run linkage where
  the actor is allowed to see them;
- binary E2E test runs the real CLI against embedded persisted rows;
- optional live-inference test persists a real run and verifies the adapter
  shape without asserting exact model wording.

### 2. LangGraph-style state/history adapter

Purpose:

- test graph/task/state projection;
- prove Defra documents can represent node execution, transitions,
  checkpoints, retries, and child runs without importing LangGraph as the core
  runtime model.

Definition of done:

- request/message/tool/response rows become graph nodes or state history
  entries;
- parent/child request lineage becomes graph edges;
- final state/checkpoint values are generated from persisted rows;
- binary E2E validates the shape against seeded runtime rows;
- external-system integration is dependency-light by default and only uses
  Docker or Python package installs behind an ignored/env-gated test when that
  catches compatibility bugs a fixture cannot.

### 3. Multi-agent task adapter

Purpose:

- test distributed coordination, handoffs, delegation, roles, task ownership,
  and access boundaries;
- give CrewAI/AutoGen/Microsoft-style users a familiar task/team view without
  making Defra core framework-specific.

Definition of done:

- participants derive from `agent_did` and `behavior_id`;
- delegations derive from parent/child requests and child-request tool calls;
- messages and tool events retain provenance and redaction behavior;
- binary E2E validates task, participant, delegation, and tool-event output;
- external-system integration uses lightweight contract fixtures first, with
  Docker/live-framework gates only where they provide real interoperability
  signal.

## Integration test strategy

Default CI should not require Docker, Python environments, or external cloud
credentials. The base proof should stay close to the product runtime:

- pure Rust projection unit tests for deterministic mapping behavior;
- real-binary E2E tests that create a temporary `agent-home`, boot an embedded
  Defra node, install runtime schemas, persist rows, run `defra-agent`, and
  validate adapter JSON;
- fixture/contract tests for external adapter shapes using minimal checked-in
  JSON examples or schema validators where practical;
- ignored/env-gated tests for Dockerized or live external frameworks when a
  real framework runtime catches issues fixtures cannot;
- ignored/env-gated live-inference tests that assert persisted runtime rows and
  projection invariants, not exact generated prose.

External-system tests should be additive proof, not required local setup. If a
framework has a stable CLI/server image, use Docker only in the gated suite. If
the framework is primarily a library, prefer a tiny fixture generator or schema
validator over pulling the full stack into normal Rust tests.

The current gated Docker suite is `Adapter Interop`, an on-demand GitHub
Actions workflow available through `workflow_dispatch` or the
`adapter-interop` PR label. It builds and runs the LangGraph, AutoGen, CrewAI,
and Microsoft Agent Framework fixture generators with Docker, then points the
ignored Rust adapter harness at the combined capture directory and uploads the
generated JSON fixtures as run artifacts. The same path can use fake, auto, or
live LangGraph provider mode without changing default PR CI.

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
2. OpenAI/Codex-style run trace projection.
3. LangGraph-style state/history projection.
4. Multi-agent task/delegation projection.
5. Extract the common projection framework only from the behavior shared by
   items 1-4.
6. A2A Agent Card projection.
7. A2A task lifecycle adapter.
8. Approval/review record shape.
9. Handoff specialist and manager-as-tools templates.

This slice gives customers the clearest proof of the Defra thesis:

- one persisted Defra runtime can produce familiar outputs for three different
  external ecosystems;
- runtime state remains Defra-native while adapter views stay customer
  familiar;
- redaction and provenance are part of projection behavior, not downstream
  cleanup scripts;
- delegation and lineage are visible;
- agents can later be discovered and invoked through standard protocols;
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

Started after the adapter-driven reframing:

- `crates/defra-agent/src/adapter_projection.rs` defines the first common
  adapter projection envelope, projection kind/version metadata, provenance,
  projection context, redaction modes, and three initial adapter DTOs.
- The first adapter DTOs are OpenAI/Codex-style run trace,
  LangGraph-style state/history, and multi-agent task/delegation.
- `defra-agent trace project --projection ... --request-id REQUEST_ID` exports
  those adapter views from the same run timeline reconstruction path.
- Adapter projections have lightweight contract validation for shared envelope
  fields and adapter-specific required fields before CLI output is written.
- `trace project` supports both JSON envelopes and JSONL record streams through
  `--format json|jsonl`.
- `trace project-schema --projection ... --format json|jsonl` prints
  dependency-light JSON Schema documents for adapter envelope and JSONL record
  shapes without requiring a live runtime.
- Checked-in adapter projection schema snapshots live under
  `docs/superpowers/contracts/adapter-projections/v1/`, covering all three
  initial adapter projections and their JSONL record schemas.
- Dependency-light adapter conformance fixtures live under
  `docs/superpowers/fixtures/adapter-projections/v1/`, covering representative
  OpenAI/Codex, LangGraph, and multi-agent envelopes without Docker or live
  service startup.
- Core adapter tests compile the generated JSON Schemas with a dev-only
  validator and validate emitted adapter envelopes, emitted JSONL records, and
  checked-in conformance fixture envelopes against those schemas.
- `trace project --format training-jsonl` emits normalized training/eval
  records derived from each adapter projection, and
  `trace project-schema --format training-jsonl` exports the matching schema
  snapshots.
- `trace project` accepts explicit document-scope gates for agent DID,
  behavior id, and session id. Scoped projections deny out-of-scope root
  requests and filter content-bearing child-agent events while preserving
  delegation topology.
- Binary E2E coverage validates all three adapter views from embedded
  persisted runtime rows, checks public redaction behavior, and exercises the
  JSONL, training/eval JSONL, and document-scope export paths. Tests also
  verify schema export against the checked-in snapshots and validate
  conformance fixtures without booting DefraDB.
- An ignored/env-gated external fixture harness validates upstream-captured or
  Docker-generated adapter captures through the same DTO contract, envelope
  schema, adapter JSONL schema, and training/eval JSONL schema path used by the
  checked-in fixtures.
- `docs/superpowers/fixtures/adapter-projections/run_docker_interop.sh` builds
  and runs every Dockerized upstream fixture generator into one output
  directory, then invokes the ignored Rust external adapter harness against the
  combined captures. This makes the LangGraph, AutoGen, CrewAI, and Microsoft
  Agent Framework interoperability proof repeatable as one binary/Docker
  command.
- A Docker-backed LangGraph fixture generator runs a real LangGraph
  `StateGraph`, captures `get_state_history`, and emits a wrapped
  `langgraph_state_history` adapter fixture for the external harness.
- The LangGraph fixture generator also emits a compiled-subgraph capture that
  projects parent/subgraph nodes, nested transitions, subgraph tasks, and the
  child request boundary through the same external harness.
- The LangGraph fixture generator also emits a provider-shaped capture that
  invokes a LangChain chat model, checkpoints `HumanMessage`, `AIMessage`, and
  `ToolMessage` state, projects the model/tool boundary as a child task, and
  can run either deterministically with `FakeListChatModel` or against a live
  OpenAI-compatible endpoint when credentials are supplied.
- A Docker-backed AutoGen AgentChat fixture generator runs a real
  `RoundRobinGroupChat` with deterministic custom agents, captures the native
  `TaskResult`, and emits a wrapped `multi_agent_task` adapter fixture for the
  external harness.
- The AutoGen fixture generator also emits a real `Swarm` capture with native
  `HandoffMessage` routing from planner to researcher to reviewer, projecting
  the resulting delegation chain and child request boundaries through the same
  multi-agent adapter contract.
- A Docker-backed CrewAI fixture generator runs a real sequential `Crew` with
  deterministic custom `BaseLLM` agents, captures native `Agent`, `Task`,
  `Crew`, `Process.sequential`, and `Crew.kickoff` evidence, and emits a
  wrapped `multi_agent_task` adapter fixture with task context handoff
  boundaries for the external harness.
- The CrewAI fixture generator also emits a hierarchical `Crew` capture where a
  manager uses CrewAI's `delegate_work_to_coworker` tool to delegate research
  and review work to child agents. The fixture records native manager/worker
  LLM calls, repeated manager-to-worker child request boundaries, and context
  flow from research into review through the same external harness.
- A Docker-backed Microsoft Agent Framework fixture generator runs a real
  `GroupChatBuilder` workflow with deterministic custom `BaseChatClient`
  agents, captures native workflow and group-chat request/response events, and
  emits a wrapped `multi_agent_task` adapter fixture for centralized
  orchestrator-selected turns, shared conversation flow, and child request
  boundaries.
- `trace project --acp-policy-id POLICY --graphql ENDPOINT --actor-did DID`
  enforces DefraDB Document ACP read decisions before projection. The loader
  asks `/acp/document/decide` for each runtime row `_docID`, denies the root
  request fail-closed, and filters denied child/content rows before building
  adapter output.
- `ProjectionAcpBinding` documents let `trace project` discover a projection
  ACP policy and collection-to-resource-name map when `--acp-policy-id` is not
  passed. Bindings can be scoped by agent DID, behavior id, and projection id;
  a matching binding only wins when its scope is a strict superset of other
  matches, ambiguous or incomparable matches fail closed, and explicit CLI
  policy ids remain the override path.
- Agent manifest roots can now own `ProjectionAcpBinding` documents under
  `projection-acp-bindings/<binding_id>/object.json`. Desired-state
  validate/load/write/diff/apply/export paths treat the binding as a closed
  config collection with `binding_id` upserts, agent-scope validation,
  resource-map validation, timestamp stripping, prune safety for
  `behavior_id`, Lean apply-order parity, and real-binary export/apply
  roundtrip coverage.
- `ProjectionAcpBinding` documents now carry policy lifecycle metadata:
  `staged_policy_id`, `previous_policy_id`, `publication_status`, and
  `published_at`. Manifest validation rejects collapsed active/staged/previous
  policy ids, invalid publication states, and rotating bindings without a
  staged policy; real-binary apply/export coverage proves those fields survive
  config publication and re-export.
- Projection ACP lifecycle coverage now includes the pinned DefraDB ACP policy
  path at the library boundary: a test builds policy YAML for the runtime
  projection resources, stores it with validation and DPI enforcement, validates
  the policy/resource interfaces, registers a projection row document, grants
  and revokes a reader relationship, exports actor relationships, and checks
  `DocumentACP::check_doc_access` decisions for owner, reader, and unrelated
  actors.
- An ignored live-inference binary E2E test runs a real tool-backed request
  against the configured OpenAI-compatible endpoint, then exports the persisted
  run through `trace project` for OpenAI/Codex JSON, OpenAI/Codex JSONL,
  OpenAI/Codex training JSONL, LangGraph training JSONL, and multi-agent
  training JSONL. The assertions check persisted tool evidence, projection
  provenance, redaction, and adapter invariants without depending on exact
  model prose.

Still pending for the adapter-driven slice:

- Broader native ACP lifecycle coverage beyond projection binding
  validate/apply/export/lifecycle metadata, explicit `--acp-policy-id` GraphQL
  paths, and in-memory DefraDB ACP policy/resource lifecycle coverage,
  especially HTTP/SourceHub policy publication workflows against a live DefraDB
  node.
- Broader multi-agent captures for additional Microsoft Agent Framework shapes
  or CrewAI edge cases, if those expose compatibility issues beyond the current
  AutoGen round-robin, AutoGen Swarm handoff, CrewAI sequential task-context,
  CrewAI hierarchical manager delegation, and Microsoft Agent Framework
  group-chat fixtures.
- Broader LangGraph external captures only for provider-specific edge cases
  beyond the current linear, compiled-subgraph, and provider-shaped chat-model
  fixtures.
