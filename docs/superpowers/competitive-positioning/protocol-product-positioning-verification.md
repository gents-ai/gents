# Protocol/product positioning verification

Date: 2026-06-05
Subject: `docs/superpowers/competitive-positioning/protocol-product-positioning-map.md`
Result: verified with wording corrections applied
Tracking issue: https://github.com/sourcenetwork/defra-agent/issues/407
Moved: 2026-06-08 into `docs/superpowers/competitive-positioning/` with the
rest of the competitive positioning material.

## Scope

This audit verifies the claims in the protocol/product positioning map against:

- the current Defra Agent working tree;
- shallow upstream checkouts under `/Users/johnzampolin/go/src/github.com`;
- current primary documentation for OpenAI, LangGraph, CrewAI, Microsoft Agent
  Framework, MCP, A2A, external Agent Communication Protocol, and ANP.

The strategy document intentionally contains positioning arguments. Those are
marked below as product interpretation when they are not direct upstream claims.

## Upstream checkouts

| Project | Local path | Verified head |
| --- | --- | --- |
| MCP | `/Users/johnzampolin/go/src/github.com/modelcontextprotocol/modelcontextprotocol` | `6d441518de8a9d5adbab0b10a76a667a63f90665` |
| A2A | `/Users/johnzampolin/go/src/github.com/a2aproject/A2A` | `2e0a4e535e738ae12af2a757c7013cf60283fa71` |
| external ACP | `/Users/johnzampolin/go/src/github.com/i-am-bee/acp` | `e5265ca9fa06c55cd011b1e81ee927f6d80af8f6` |
| ANP | `/Users/johnzampolin/go/src/github.com/agent-network-protocol/AgentNetworkProtocol` | `a0a7d2f2ee88464a1ecbcb1e3f8f63c730b846c0` |
| LangGraph | `/Users/johnzampolin/go/src/github.com/langchain-ai/langgraph` | `2b1abc8` |
| CrewAI | `/Users/johnzampolin/go/src/github.com/crewAIInc/crewAI` | `cab3319` |
| Microsoft Agent Framework | `/Users/johnzampolin/go/src/github.com/microsoft/agent-framework` | `bf4ad48` |

OpenAI claims were checked against official OpenAI developer docs only.

## Corrections applied

| Claim area | Original risk | Correction |
| --- | --- | --- |
| Defra trace/access wording | Said sessions, traces, and access decisions are first-class documents. | Narrowed to sessions/messages/responses as documents; trace/audit views are reconstructed from persisted runtime rows; access decisions are enforced by DefraDB ACP. |
| MCP scope | Framed MCP mostly as a tool boundary. | Expanded to tools, resources, prompts, and contextual services through MCP servers over JSON-RPC. |
| A2A task id mapping | Said `request_id` maps to `taskId` without task creation nuance. | Clarified that generated Defra `request_id` is exposed as `taskId` for new A2A tasks, while incoming `taskId` resumes/refines existing work. |
| external ACP status | Said ACP is converging with A2A. | Updated to ACP is merging into A2A under the Linux Foundation and active ACP development is winding down. |
| signed discovery | Suggested signed cards/manifests together. | Split A2A Agent Card JWS from ACP manifest signing, which should be treated as a Defra extension unless standardized. |
| ANP status/disclosure | Implied a mature standard and concrete selective-disclosure task metadata protocol. | Marked ANP as draft/active-development and narrowed to signed/verified capability docs plus permissioned/minimized publication of metadata. |
| LangGraph integration | Could read as LangGraph already supports Defra storage. | Marked Defra-backed checkpoint/run storage as a Defra integration design, not an upstream LangGraph claim. |
| CrewAI mapping | Said the Flow/Crew split maps cleanly and implied CrewAI is only Python runtime objects. | Narrowed to capability-level mapping and removed stale "only Python runtime objects" language. |
| OpenAI trace wording | Referred to trace docs. | Changed to trace export plus `AgentToolCall` rows. |
| human-in-the-loop records | Could imply dedicated approval records already exist. | Changed to current interrupt/cancel/policy-denial flows plus explicit approval records if needed. |
| trace exporter compatibility | Suggested OpenTelemetry/LangSmith-compatible traces as a direct recommendation. | Marked compatibility as future integration work if customers need it. |
| pattern templates | Over-attributed all listed patterns to every framework. | Split patterns by source: LangGraph durable graphs, CrewAI Flows/Crews, OpenAI handoffs/manager-as-tools, Microsoft sequential/concurrent/handoff/group-chat/Magentic. |

## Claim verification

| Claim | Verdict | Evidence |
| --- | --- | --- |
| Defra Agent is document-native and stores agent identity/config/work as documents. | Verified. | `AgentPrincipal`, `AgentBehavior`, `AgentRequest`, `AgentMessage`, `AgentResponse`, `AgentSession`, `AgentToolCall`, `Task`, `Schedule`, and `EventTrigger` schemas exist under `crates/defra-agent-schemas/schemas/agent/`. |
| Defra Agent uses DID-keyed principals as identity/permission boundaries. | Verified. | `agent_principal.graphql` defines `agent_did`; `crates/defra-agent/src/identity.rs` and `crates/defra-agent/src/config.rs` route behavior work through principal identity. |
| DefraDB ACP is the production permission decider. | Verified. | `docs/superpowers/specs/2026-05-19-identity-permission-runtime-design.md` records DefraDB ACP as the production decider and avoids a separate Rust permission decider. |
| Trace export exists, but traces are not a first-class schema document. | Verified and corrected. | `crates/defra-agent/src/trace_export.rs` reconstructs/export traces from persisted runtime rows; no separate trace GraphQL document schema was found. |
| Declarative deployment supports desired-state validate/diff/apply and prune routed through reconcile modeling. | Verified. | `crates/defra-agent-cli/src/desired_state/` includes validate/diff/apply/prune flows; prune routes through `apply_model::diff_prune`, with Lean reconcile model references in the repo. |
| Defra has human-in-the-loop adjacent interrupt/cancel and policy-denial state, but no dedicated approval-record schema was verified. | Verified and corrected. | `agent_request.graphql` and `agent_tool_call.graphql` include interrupt/cancel/lifecycle/policy-denial fields; search did not find a first-class approval record schema in the agent schemas. |
| MCP defines host/client/server roles, JSON-RPC messages, and tools/resources/prompts. | Verified. | MCP architecture docs and checked-out repo at `modelcontextprotocol/modelcontextprotocol` describe the architecture and primitives. |
| MCP should remain Defra's external context/tool boundary. | Product recommendation. | Supported by MCP scope, but it is Defra positioning rather than an MCP requirement. Wording was broadened beyond tools. |
| A2A is for agent collaboration/interoperability and uses Agent Cards for discovery. | Verified. | A2A docs describe Agent Cards, discovery, supported interfaces/endpoints, capabilities, security, and skills. |
| A2A `contextId` groups related work and `taskId` identifies task work. | Verified with correction. | A2A spec/docs support `contextId` grouping and task ids; new task ids are server-generated, so Defra adapters must expose generated `request_id` rather than accept arbitrary client task ids for creation. |
| A2A `SendMessage` can map to Defra `AgentRequest`. | Product integration recommendation. | A2A has `SendMessage`; Defra has durable `AgentRequest`. The mapping is a design recommendation, not an upstream claim. |
| external ACP is REST-based and supports sync/async/streaming runs, manifests, discovery, sessions, Await, cancel, and lifecycle states. | Verified. | ACP welcome, discovery, run lifecycle, and distributed sessions docs support the feature list. |
| external ACP is merging into A2A under the Linux Foundation. | Verified and corrected. | ACP README/docs and i-am-bee announcement state ACP is part of/merging into A2A and active ACP development is winding down. |
| ANP centers DID identity, secure messaging/E2EE, agent description, discovery, and meta-protocol negotiation. | Verified with status caveat. | ANP specs and repo describe DID/WBA identity, secure communication, description, discovery, and meta-protocol layers; specs are draft/active-development material. |
| ANP projection should use signed/verified capabilities and minimized metadata publication. | Product recommendation with support in spirit. | DID documents and optional proof fields support verification concepts; ANP privacy/discovery docs support minimized/public-private publication, but not a fully mandatory signing/selective-disclosure protocol. |
| LangGraph provides durable graph runtime behavior with checkpoints, HITL, memory, replay/fork, fault tolerance, and observability. | Verified. | LangGraph persistence, time-travel, memory, and product docs cover checkpoints, HITL, memory, replay/fork, fault tolerance, and LangSmith observability. |
| Defra could serve as LangGraph checkpoint/run storage. | Product integration recommendation. | Defra has durable documents; LangGraph supports persistence/checkpointing. No upstream LangGraph claim says Defra is a supported backend. |
| CrewAI Flows provide structured event-driven workflow control with state; Crews provide role-oriented collaborative agent teams. | Verified. | CrewAI introduction, Flows, Crews, Processes, and Agents docs support this split. |
| Defra task/trigger/schedule plus behavior/tool-selection/subagent primitives match CrewAI at capability level. | Product interpretation. | Local Defra schemas support the primitives; mapping is capability-level, not one-to-one API equivalence. |
| OpenAI Agents SDK is a code-first SDK for tools, handoffs, guardrails/human review, state/continuation, MCP wiring, and tracing. | Verified. | Official OpenAI Agents SDK docs cover SDK-owned orchestration, tools/MCP, handoffs, guardrails/human review, results/state, and tracing. |
| Microsoft Agent Framework is successor to AutoGen and Semantic Kernel and provides graph workflows plus orchestration patterns. | Verified. | Microsoft docs and repo README describe the successor positioning, graph workflows, sequential/concurrent/handoff/group-chat/Magentic patterns, and human-in-the-loop flows. |
| Defra's differentiator is a permissioned document graph underneath these adapters/frameworks. | Product positioning, locally supported. | Defra schemas, DID identity routing, durable runtime rows, and DefraDB ACP support the argument; external sources do not prove comparative statements about other frameworks' permission models. |

## Primary external sources

- MCP architecture: https://modelcontextprotocol.io/docs/learn/architecture
- A2A discovery: https://a2a-protocol.org/latest/topics/agent-discovery/
- A2A task lifecycle: https://a2a-protocol.org/latest/topics/life-of-a-task/
- A2A definitions: https://a2a-protocol.org/latest/definitions/
- ACP welcome: https://agentcommunicationprotocol.dev/introduction/welcome
- ACP run lifecycle: https://agentcommunicationprotocol.dev/core-concepts/agent-run-lifecycle
- ACP discovery: https://agentcommunicationprotocol.dev/core-concepts/agent-discovery
- ACP distributed sessions: https://agentcommunicationprotocol.dev/core-concepts/distributed-sessions
- ACP/A2A merge announcement: https://github.com/orgs/i-am-bee/discussions/5
- ANP specs: https://agentnetworkprotocol.com/en/specs/
- LangGraph persistence: https://docs.langchain.com/oss/python/langgraph/persistence
- LangGraph time travel: https://docs.langchain.com/oss/python/langgraph/use-time-travel
- CrewAI Flows: https://docs.crewai.com/en/concepts/flows
- CrewAI Crews: https://docs.crewai.com/en/concepts/crews
- OpenAI Agents SDK: https://developers.openai.com/api/docs/guides/agents
- OpenAI Agents SDK orchestration: https://developers.openai.com/api/docs/guides/agents/orchestration
- OpenAI Agents SDK guardrails/human review: https://developers.openai.com/api/docs/guides/agents/guardrails-approvals
- OpenAI Agents SDK results/state: https://developers.openai.com/api/docs/guides/agents/results
- OpenAI Agents SDK integrations/observability: https://developers.openai.com/api/docs/guides/agents/integrations-observability
- Microsoft Agent Framework overview: https://learn.microsoft.com/en-us/agent-framework/overview/
- Microsoft Agent Framework orchestrations: https://learn.microsoft.com/en-us/agent-framework/workflows/orchestrations/
