# Protocol and product positioning map

Date: 2026-06-05
Status: strategy note
Tracking issue: https://github.com/sourcenetwork/defra-agent/issues/407
Verified: 2026-06-05 against local Defra Agent code/schemas and current
upstream protocol/framework docs or repositories. Audit evidence:
https://github.com/sourcenetwork/defra-agent/issues/407#issuecomment-4672217843
Updated: 2026-06-08 to clarify which capabilities are already Defra-native
documents/projections rather than new core product areas.

## Goal

Explain how Defra Agent maps to the agent protocol and framework ecosystem,
and make the product position clear for customer conversations:

Defra Agent is a document-native, identity-aware agent runtime. It is not a
replacement for MCP, A2A, ACP, ANP, LangGraph, CrewAI, the OpenAI Agents SDK,
or Microsoft Agent Framework. It is the durable harness underneath those
surfaces: agent identity, behavior, requests, tool calls, schedules, triggers,
sessions, messages, and responses are first-class documents. Trace and audit
views are reconstructed from persisted runtime rows, and access decisions are
enforced by DefraDB ACP.

That trade-off is the important difference. In many framework-first
deployments, coordination, memory, permissions, and lineage are carried in
framework code or per-app glue. Defra Agent pushes those concerns into a shared
DefraDB document graph, with DID-keyed principals and DefraDB access control as
the permission substrate. Protocols can then be adapters or projections over
the same source of truth.

## Naming warning: two ACPs

This repo already uses "ACP" for DefraDB access-control policy surfaces such
as `DocumentACP`. The external protocol ecosystem also uses "ACP" for Agent
Communication Protocol.

In customer and engineering docs, use explicit names:

- "DefraDB ACP" for access control.
- "Agent Communication Protocol" or "external ACP" for the agent
  interoperability protocol.

This avoids confusing the permission layer with the interoperability layer.

## Core customer message

Defra Agent gives teams a shared operational substrate for agents:

- Agent identity is a document and permission boundary, not just a runtime
  object.
- Agent configuration is declarative and diff/apply managed, not scattered
  through code.
- Agent work is durable: requests, tool calls, messages, responses, schedules,
  and triggers live as documents.
- Access control is close to the data. The runtime does not need to reinvent a
  parallel permissions system in every adapter.
- Distributed coordination has a common state model. Cross-agent and
  cross-deployment workflows can refer to the same durable lineage instead of
  passing opaque blobs between framework instances.

Short version:

> Defra Agent turns agent coordination into a permissioned, peer-replicated
> document graph.
> Frameworks and protocols become ways to interact with that graph.

## Positioning principle

Defra Agent should have the important capabilities customers expect from the
ecosystem, but not by cloning every framework into core.

The product shape should be:

- Core runtime: identity, documents, lifecycle, ACL, durable execution, tool
  policy, trace export, schedules, triggers, and conformance.
- Protocol adapters: MCP, A2A, external ACP, and ANP-compatible discovery or
  transport surfaces.
- Pattern templates: LangGraph/CrewAI/OpenAI/Microsoft-style orchestration
  patterns expressed as Defra-native documents.
- Exporters: traces, run histories, task events, and audit views in formats
  customers already understand.

That is the "all features" interpretation: feature parity at the capability
level, while keeping Defra's source of truth document-native.

## Current Defra-native primitives

Local anchors:

- Agent identity: `crates/defra-agent-schemas/schemas/agent/agent_principal.graphql`
  defines `AgentPrincipal` with `agent_did`, `display_name`,
  `default_behavior_id`, and `enabled`.
- Agent behavior: `crates/defra-agent-schemas/schemas/agent/agent_behavior.graphql`
  defines prompts, model/backend selection, tool selection, compaction, and
  skills.
- Durable work item: `crates/defra-agent-schemas/schemas/agent/agent_request.graphql`
  defines request identity, agent/behavior/session linkage, lifecycle/status,
  retry lineage, trigger causality, deadlines, interrupt fields, and subagent
  lineage.
- Tool execution: `crates/defra-agent-schemas/schemas/agent/agent_tool_call.graphql`
  defines tool call identity, request/session linkage, selected MCP service,
  policy denial fields, lifecycle, deadlines, cancel state, and child request
  linkage.
- Workflow entry points: `Task`, `Schedule`, and `EventTrigger` in
  `crates/defra-agent-schemas/schemas/agent/`.
- Tool access policy: `ToolSelection` controls file/bash/meta tools, MCP
  allowlists, delegation, subagent spawning, backgrounding, and `defra_query`.
- MCP services: `crates/defra-agent-protocol/schemas/services/tool_service_registry.graphql`
  defines service identity, network address, MCP path/port, `send_agent_did`,
  and online status.
- Permission stance: DefraDB ACP is the production permission decider, and
  runtime routing signs DB work with the behavior principal (design recorded
  in the 2026-05-19 identity-permission spec, now in git history).
- Proof-first runtime contracts: `CLAUDE.md` requires Lean models,
  conformance tests, and Rust implementation to move together for state
  machine behavior.

## Protocol and framework map

| Ecosystem item | Job customers recognize | Defra-native mapping | Recommendation |
| --- | --- | --- | --- |
| MCP | AI application or agent host access to external tools, resources, prompts, and contextual services through MCP servers. MCP official docs define host/client/server roles and a JSON-RPC data layer. | `ToolServiceRegistry`, `ToolSelection.allowed_mcp_service_ids`, MCP pool/health, `AgentToolCall.selected_service_id`, `send_agent_did`. | Keep MCP as the external context/tool boundary. Defra should add stronger policy, identity, health, provenance, and trace guarantees around MCP calls rather than inventing a new context protocol. |
| A2A | Agent-to-agent discovery and task delegation. A2A Agent Cards describe identity, supported interfaces/endpoints, capabilities, security requirements, and skills; `contextId` groups related interactions and `taskId` identifies task work. | `AgentPrincipal` + `AgentBehavior` + `Task` become Agent Card source data. In the simple case, an Agent Card is a projection of one or more Defra agent behaviors and their allowed tasks/capabilities. `session_id` maps to `contextId`. For new A2A tasks, generated Defra `request_id` is exposed as `taskId`; incoming `taskId` should resume or refine an existing request/task. `AgentRequest.lifecycle_state` maps to task state. `AgentResponse`/messages/tool calls become artifacts/events. | Build an A2A projection and task adapter. The card is mostly catalog/behavior projection; the task lifecycle adapter maps protocol work to Defra `AgentRequest` rows. |
| Agent Communication Protocol | REST-based interoperability for agents, applications, and humans, with sync/async/streaming runs, manifests, discovery, sessions, Await, cancellation, and run lifecycle states. ACP docs now state ACP is merging into A2A under the Linux Foundation and active ACP development is winding down. | External ACP `Run` maps to `AgentRequest`. `session_id` maps to ACP session. Agent manifest maps from principal/behavior/task metadata. Await/cancel states map to interrupt and lifecycle fields. External ACP auth/resource semantics should resolve to a Defra actor and DefraDB ACP document decisions. | Treat external ACP as another adapter/projection. Because ACP is merging into A2A, avoid building duplicate business logic; share the same request/session mapper and use DefraDB ACP as the permission decider. |
| ANP | Draft DID-based agent-network protocol stack covering DID identity, secure messaging/E2EE, agent description, discovery, and meta-protocol negotiation. | `AgentPrincipal.agent_did` is already the identity anchor. Defra already has signed documents/capabilities as the natural source for DID-bound metadata. DefraDB ACP and document permissions are the local authorization substrate. P2P/document sync is the natural distributed state layer. | Position ANP as a remote trust/discovery layer on top of Defra Agent. The gap is not inventing signed capabilities; it is publishing Defra's existing signed docs/capabilities through an ANP-shaped discovery/trust envelope with minimized, ACL-filtered metadata. |
| LangGraph | Durable graph runtime for long-running, stateful workflows: checkpoints, human-in-the-loop, memory, replay/fork, fault tolerance, and LangSmith observability. | Defra Agent already stores durable request/tool/session state as documents and has Lean-backed lifecycle contracts. A graph runner could be integrated with Defra docs as checkpoint/run storage, but that is a Defra integration design rather than a LangGraph claim. | Do not copy LangGraph's Python graph DSL into core. Offer Defra as a durable backend or execution substrate for graph-like workflows. |
| CrewAI | Flows provide structured, event-driven workflow control with state; Crews provide collaborative, role-oriented agent teams with sequential or hierarchical process control. | `Task`, `Schedule`, and `EventTrigger` cover Defra's Flow-like entry points at the capability level. `AgentBehavior`, `ToolSelection`, `skill_refs`, delegation, and subagent lineage cover the Crew/team shape at the capability level. | Provide templates and import/export guidance. Customers can model CrewAI-style patterns in Defra docs without giving up Defra-native ACL and lineage. |
| OpenAI Agents SDK | Code-first SDK for agent loops where application code owns tools, handoffs, guardrails/human review, state/continuation, MCP wiring, and tracing. | `AgentBehavior` is the specialist contract. `ToolSelection` is the tool/guardrail policy. Subagent fields and child requests model handoffs. Trace export plus `AgentToolCall` rows record execution. | Treat the SDK as an agent runner that can be hosted behind a Defra behavior or bridged into Defra traces. Keep Defra's durable state and ACL as the system of record. |
| Microsoft Agent Framework / AutoGen lineage | Microsoft Agent Framework, successor to AutoGen and Semantic Kernel, provides graph-based workflows and built-in orchestration patterns: sequential, concurrent, handoff, group chat, and Magentic, with human-in-the-loop approval/request-info support. | These are topology patterns over `AgentRequest`, sessions, subagent lineage, tool calls, and shared documents. | Use the vocabulary in sales and examples. Implement reusable Defra templates for these patterns rather than committing core runtime to one framework's API. |

## Feature parity by capability

This is the feature checklist customers are likely to expect after using other
tools.

| Capability | Present Defra shape | Gap / recommendation |
| --- | --- | --- |
| Tool integration | MCP client/pool, tool-service registry, meta tools, tool call persistence, MCP allowlists, health checks. | Keep tightening service identity, per-agent allowlists, health policy, and trace attribution. |
| Agent discovery | Principals, behaviors, tasks, tool selections, service registry. | Generate A2A Agent Cards and ACP manifests from Defra docs. This is mostly an ACL-filtered behavior/task/catalog projection, not a new core primitive. Use A2A Agent Card JWS where needed; treat ACP manifest signing as a Defra extension unless ACP/A2A migration guidance standardizes it. |
| Agent-to-agent work | Requests have agent/behavior/session IDs, parent request/tool call causality, subagent depth, child request linkage. | Add protocol adapters for A2A/ACP send/run semantics. Make state mapping explicit and tested. |
| Distributed identity | `agent_did` is central; runtime routes behavior work through principal identity; signed documents/capabilities already exist as the local capability source. | Define the ANP projection: DID verification, remote trust policy, and permissioned/minimized publication of existing signed Defra capability metadata. |
| Access control | DefraDB ACP is the permission decider; docs are the permission boundary. | Make this a first-class customer proof point. Agent catalogs, cards, manifests, and protocol outputs should be ACL-filtered projections. Avoid adapter-local permission systems except as protocol auth shims. |
| Durable execution | Requests, tool calls, messages, responses, schedules, and triggers are documents. | Expose clear resume/replay/export APIs so LangGraph-style and ACP/A2A-style clients can understand the lifecycle. |
| Shared memory / blackboard | DefraDB collections and branchable docs provide shared state; agent writes are tied to their DID. | Treat provenance as an export/query surface over existing identity-linked writes. Add request/tool-call origin and poisoning/review controls only where automated shared-memory workflows need them. |
| Human-in-the-loop | Interrupt/cancel fields, policy denial fields, lifecycle state, desktop/CLI operator surfaces. | Lower priority than automated workflows. Map ACP Await/A2A `input-required` concepts onto existing flows where needed, but do not prioritize dedicated approval records until a concrete automated use case needs them. |
| Observability | Tool calls and request lifecycle are persisted; trace export exists. | Add exporters for protocol views: A2A task events and ACP run events. Treat OpenTelemetry or LangSmith compatibility as future integration work if customers need it. |
| Declarative deployment | Desired-state manifests, validate/diff/apply, prune routed through proven reconcile model. | Position this strongly against code-first frameworks: customers can review and apply agent fleets like infrastructure. |
| Formal contracts | Lean models and Rust conformance tests cover state machine behavior. | Keep using proofs for lifecycle/ACL/reconcile invariants so those guarantees are part of the runtime contract, not only adapter tests. |

## Corrected priority read

Several ecosystem capabilities should stay small because Defra already has the
hard substrate:

- A2A Agent Cards are not a new agent model. They are mostly projections of
  `AgentBehavior`, `Task`, `ToolSelection`, and capability/catalog documents.
- External Agent Communication Protocol should map to Defra documents and
  DefraDB ACP. Do not create an external-ACP-specific permission layer.
- ANP-style discovery should use existing signed documents and signed
  capabilities. The work is an ANP-shaped publication/trust envelope, not a new
  signing system.
- ACL-filtered catalogs should be straightforward DefraDB ACP projections.
- Shared-memory provenance already starts from agent DID-linked writes. The
  useful work is surfacing request/tool-call origin for automated workflows,
  not inventing a separate provenance substrate.
- Dedicated human approval/review records are lower priority than automated
  inter-agent use cases.
- Pattern templates are the important next customer-facing bridge: map
  LangGraph, CrewAI, AutoGen, OpenAI Agents SDK, and Microsoft Agent Framework
  patterns into Defra-native documents and prove them with E2E tests.

## Customer talk tracks

| Customer starting point | Answer |
| --- | --- |
| "We already use MCP." | Good. Defra Agent should keep MCP as the external context/tool interface. The difference is that Defra adds per-agent tool policy, service health, DID propagation, persisted tool calls, and ACL-backed access for Defra-backed documents and data surfaces. |
| "We need A2A or ACP." | Defra Agent should expose those protocols as edge adapters. Internally, the run/task/session should still be a Defra document so ACL, audit, retries, cancellation, and lineage all work the same way across protocols. |
| "We are looking at ANP for distributed agents." | Defra Agent already treats the agent DID as a first-class boundary. ANP-style discovery and encrypted messaging are natural remote layers on top; Defra's differentiator is what happens after discovery: document-level permissions and durable shared state. |
| "We use LangGraph." | LangGraph is a strong graph orchestration model. Defra Agent can complement it by providing durable, permissioned state and operational records underneath graph execution, especially where multiple agents or teams need shared ACL-governed state. |
| "We use CrewAI." | CrewAI's Flow/Crew split maps at the capability level to Defra tasks/triggers/schedules plus behaviors/tool selections/subagents. Defra's value is making those roles, tools, events, and results part of a permissioned document graph. |
| "We use the OpenAI Agents SDK." | The SDK is a good runner for code-first agents, tools, handoffs, guardrails, and traces. Defra can host or wrap those agents while preserving Defra-native identity, document ACLs, durable request/tool-call history, and deployment manifests. |
| "We need Microsoft/AutoGen-style orchestration." | Sequential, concurrent, handoff, group chat, manager-led, and approval patterns should be Defra templates over the same request/session/tool-call documents. The topology is familiar; the state and ACL model are Defra-native. |

## Trade-offs to state explicitly

Defra Agent should be positioned over lightweight code-first frameworks when
the problem involves shared state, distributed agents, auditability, access
control, or repeatable fleet configuration.

The cost is that teams need to model important runtime concepts as documents:
principals, behaviors, tasks, tools, schedules, triggers, and requests. That
is more opinionated than a library where everything is just application code,
but it is what lets Defra Agent provide ACL, lineage, diff/apply, and protocol
projections without per-app glue code.

For a one-off local agent script, this may be more substrate than a team needs.
For customer systems with multiple agents, teams, tools, datasets, and
permissions, the document-native model is the product advantage.

## Recommended adapter sequence

1. A2A Agent Card projection.
   Source fields: `AgentPrincipal`, `AgentBehavior`, `Task`, `ToolSelection`,
   and `ToolServiceRegistry`. Treat this as a behavior/catalog projection, not
   a separate agent definition. Include a public-card/private-card split so
   sensitive skills and internal URLs can be hidden by DefraDB ACP.

2. A2A task lifecycle adapter.
   Map new `SendMessage` work to `AgentRequest`, `contextId` to `session_id`,
   generated `request_id` to the returned `taskId`, incoming `taskId` to an
   existing request/task for continuation, terminal task states to terminal
   `AgentRequest.lifecycle_state`, and artifacts to responses/messages/tool
   outputs.

3. Shared ACP/A2A run mapper.
   Because external ACP is merging into A2A, build a single internal mapper for
   run/task/session state and expose protocol-specific HTTP/JSON shapes at the
   edge.

4. ANP/DID discovery design.
   Publish existing signed Defra documents and capabilities through a
   DID-bound discovery/trust envelope. Define DID verification, remote trust
   policy, and permissioned/minimized publication of agent metadata. This
   should lean on `AgentPrincipal.agent_did` and DefraDB ACP instead of
   inventing a new authorization model.

5. Trace and audit exports.
   Add a customer-visible export that reconstructs a run from
   `AgentRequest`, messages, `AgentToolCall`, responses, policy denials, and
   child requests. Then project that into ACP/A2A event streams, and treat
   OpenTelemetry or LangSmith compatibility as future integration work if
   customers ask for it.

6. Pattern templates.
   Publish Defra-native templates for the patterns customers know from the
   ecosystem: LangGraph-style durable graphs; CrewAI-style Flows/Crews;
   OpenAI-style handoffs and manager-as-tools; Microsoft-style sequential,
   concurrent, handoff, group-chat, and Magentic workflows; plus Defra-specific
   scheduled flows, event-triggered flows, and cross-deployment subagents.

## What not to do

- Do not pitch Defra Agent as "our version of MCP/A2A/ACP/ANP." That makes
  Defra sound like another protocol entrant instead of the substrate that can
  host protocol surfaces.
- Do not duplicate permissions inside each adapter. Protocol auth can decide
  who reached the endpoint; DefraDB ACP should decide what documents and
  capabilities that identity can see or use.
- Do not rebuild every orchestration framework API in core. The durable
  document model should support those patterns, while adapters/templates make
  them familiar.
- Do not hide the trade-off. Defra Agent is more opinionated about state,
  identity, access control, and persistence than lightweight code-first
  libraries. That is the point.

## Source references checked

External sources checked on 2026-06-05:

- MCP architecture: https://modelcontextprotocol.io/docs/learn/architecture
- A2A agent discovery: https://a2a-protocol.org/latest/topics/agent-discovery/
- A2A life of a task: https://a2a-protocol.org/latest/topics/life-of-a-task/
- A2A definitions: https://a2a-protocol.org/latest/definitions/
- Agent Communication Protocol welcome: https://agentcommunicationprotocol.dev/introduction/welcome
- Agent Communication Protocol run lifecycle: https://agentcommunicationprotocol.dev/core-concepts/agent-run-lifecycle
- Agent Communication Protocol discovery: https://agentcommunicationprotocol.dev/core-concepts/agent-discovery
- Agent Communication Protocol distributed sessions: https://agentcommunicationprotocol.dev/core-concepts/distributed-sessions
- Agent Communication Protocol A2A merge announcement: https://github.com/orgs/i-am-bee/discussions/5
- ANP technical specifications: https://agentnetworkprotocol.com/en/specs/
- LangGraph persistence/durable execution: https://docs.langchain.com/oss/python/langgraph/persistence
- LangGraph time travel/replay: https://docs.langchain.com/oss/python/langgraph/use-time-travel
- CrewAI introduction: https://docs.crewai.com/en/introduction
- CrewAI Flows: https://docs.crewai.com/en/concepts/flows
- CrewAI Crews: https://docs.crewai.com/en/concepts/crews
- OpenAI Agents SDK: https://developers.openai.com/api/docs/guides/agents
- OpenAI Agents SDK orchestration: https://developers.openai.com/api/docs/guides/agents/orchestration
- OpenAI Agents SDK guardrails/human review: https://developers.openai.com/api/docs/guides/agents/guardrails-approvals
- OpenAI Agents SDK results/state: https://developers.openai.com/api/docs/guides/agents/results
- OpenAI Agents SDK integrations/observability: https://developers.openai.com/api/docs/guides/agents/integrations-observability
- Microsoft Agent Framework overview: https://learn.microsoft.com/en-us/agent-framework/overview/
- Microsoft Agent Framework orchestrations: https://learn.microsoft.com/en-us/agent-framework/workflows/orchestrations/
