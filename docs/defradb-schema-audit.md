# DefraDB schema audit matrix

This is the stable scope ledger for the schema epic tracked by #1063. It keeps
the review consumer-driven: a schema change needs a concrete ingest writer,
projection consumer, or DefraDB replication/identity requirement.

Scope legend:

- **Shared**: canonical fact or desired configuration that must survive host
  changes and support peer catch-up.
- **Local**: host-specific secret, path, lease, cursor, health, or observation.
- **Projection**: rebuildable view whose source documents remain authoritative.
- **CID at use**: execution should preserve the exact version it consumed.
- **CID at transition**: lifecycle history may need the exact transition
  version for audit/recovery.

Retention legend: `CFG` versioned configuration, `FACT` durable audit or
transcript, `SEC` security ledger, `OP` operational state, `SECRET` encrypted
or rotated secret, `PROJ` rebuildable projection, and `MEM` user-controlled
memory.

## Inference, identity, and configuration

| Collection | Scope | Branchable | Identity/version contract | Retention | Primary consumers |
| --- | --- | --- | --- | --- | --- |
| `InferenceBackend` | Local desired endpoint, secret, and probe observation | No | `backend_id`, `_docID`; local snapshot only | SECRET + OP | runtime reconcile, health, UI |
| `InferenceCall` | Shared provider-attempt lifecycle | **Yes** | `call_id`, `_docID`, immutable `request_doc_id`, CID at transitions | FACT | timeline, adapters, compaction |
| `InferenceProfile` | Shared generation and retry policy | **Yes** | `profile_id`, `_docID`, CID at use | CFG | provider assembly, directory, UI |
| `OAuthCredential` | Local provider credential | No | `credential_id`, `_docID`; do not export ordinary CID manifests | SECRET | provider authentication |
| `ToolServiceHealthState` | Local probe and backoff observation | No | `service_id`, `_docID` | OP | service reconcile, UI |
| `ToolServiceRegistry` | Shared MCP service catalog with some observations | **Yes** | `service_id`, `_docID`, config CID at use | CFG + OP | tool resolver, UI, P2P |
| `AgentPrincipal` | Shared DID-rooted identity configuration | **Yes** | `agent_did`, `_docID`, CID at use | CFG + SEC | reconcile, directory, P2P |
| `AgentBehavior` | Shared prompt/model/tool configuration | **Yes** | `behavior_id`, `_docID`, CID at use | CFG | reconcile, provider assembly |
| `ToolSelection` | Shared capability and approval surface | **Yes** | `selection_id`, `_docID`, CID at use | CFG | tool resolver, reconcile |
| `Skill` | Shared instructions and tool references | **Yes** | `skill_id`, `_docID`, CID at use | CFG | skill and prompt assembly |
| `DatastoreToolSurface` | Shared database tool surface | **Yes** | `surface_id`, `_docID`, CID at use | CFG | tool resolver, P2P |
| `ProjectionAcpBinding` | Shared desired projection-policy binding | **Yes** | `binding_id`, `_docID`, CID at use | CFG + SEC | projection and ACP reconcile |
| `WorkspaceRoot` | Local filesystem boundary | No | `root_path`, `_docID` | OP + CFG | tool/runtime reconcile |
| `AgentRuntime` | Local process capacity and reconcile observation | **No** | `_docID`; must gain host identity before any shared form | OP | runtime status, UI |

`InferenceBackend` intentionally remains local: endpoint availability, secrets,
and probe state are deployment properties, not portable agent configuration.
`ToolServiceRegistry` mixes desired catalog data and observed status; the
desired portion is shared, while ownership of observed fields needs a later,
consumer-proven decision rather than an immediate schema split.

## Conversation and execution

| Collection | Scope | Branchable | Identity/version contract | Retention | Primary consumers |
| --- | --- | --- | --- | --- | --- |
| `AgentConversation` | Shared rebuildable conversation summary | Yes | `session_id`, `_docID`; source rows authoritative | PROJ | desktop, CLI, Codex shim |
| `AgentDirectoryEntry` | Shared rebuildable availability projection | Yes | `directory_key`, `_docID`; retain config source CIDs | PROJ | discovery, UI, P2P |
| `AgentMemory` | Shared principal memory | Yes | `memory_id`, `_docID`, CID at prompt use | MEM | memory and prompt assembly |
| `AgentMessage` | Shared durable transcript event | Yes | `message_key`, `_docID`, immutable `request_doc_id`, fact CID | FACT | timeline, adapters, history |
| `AgentSession` | Shared conversation/run envelope | Yes | `session_id`, `_docID`, CID at transitions | FACT | timeline, UI, history |
| `AgentRequest` | Shared command, input, lineage, and lifecycle | Yes | `request_id` is correlation only; `_docID` authoritative; admission/transition CID | FACT | watcher, owned loop, projections |
| `AgentResponse` | Shared streaming/materialized response | Yes | `response_key`, `_docID`, immutable `request_doc_id`, CID at transitions | FACT | stream processor, timeline, UI |
| `AgentToolApproval` | Shared tool decision fact | Yes | `approval_id`, `_docID`, immutable `tool_call_doc_id`, fact CID | FACT + SEC | tool lifecycle, timeline |
| `AgentToolCall` | Shared tool invocation lifecycle | Yes | `tool_call_key`, `_docID`, immutable `request_doc_id`, CID at transitions | FACT | owned loop, timeline, adapters |
| `AgentToolResult` | Shared full or spilled tool output | Yes | `_docID`, immutable `tool_call_doc_id`, fact CID | FACT | transcript/tool-output loading |
| `CompactionEntry` | Shared transcript reduction fact | Yes | `compaction_key`, `_docID`, immutable `request_doc_id`, fact CID | FACT | history and compaction |
| `Goal` | Shared goal lifecycle | Yes | `goal_id`, `_docID`, CID at transitions | FACT | goal runtime, timeline, UI |
| `RenderedRequest` | Shared exact provider request capture | Yes | `capture_key`, `_docID`, fact CID, exact `AgentRequest` CID | FACT | provenance, replay, debugging |

Queries by `AgentRequest.request_id` must prove cardinality and fail closed on
duplicates. Request-scoped messages, responses, and inference calls retain the
logical id for operator correlation but bind directly to the authoritative
`AgentRequest._docID`. Tool approvals and spilled results likewise bind to the
exact `AgentToolCall._docID` rather than trusting a provider-supplied call id.
Retry, supersession, and subagent lineage retain their operator-facing logical
IDs, but each edge also records the exact parent, survivor, or bridge document
ID. Writers validate logical and physical halves together; readers reject
ambiguous or forged joins rather than choosing the first logical-ID match.

`RenderedRequest` is the immutable provider-bound body plus the request commit
that was current immediately before send. Timeline and adapter output is a
`CapturedOnly` projection over that capture and the current DefraDB fact graph;
it must not be described as a historically version-pinned reconstruction of
every transcript/config row until those source CIDs are captured explicitly.

## Automation

| Collection | Scope | Branchable | Identity/version contract | Retention | Primary consumers |
| --- | --- | --- | --- | --- | --- |
| `Task` | Shared automation definition | Yes | `task_id`, `_docID`, CID at use | CFG | task and trigger engine |
| `Schedule` | Shared definition with firing observations | Yes | `schedule_id`, `_docID`, definition/transition CIDs | CFG + FACT | scheduler, UI |
| `EventTrigger` | Shared subscription with cursor/status observations | Yes | `trigger_id`, `_docID`, definition/transition CIDs | CFG + FACT | trigger engine, UI |
| `PersonaConfigRequest` | Shared remote configuration command | Yes | `request_key`, `_docID`, admission/transition CID | FACT | persona/config reconcile |

`Schedule` and `EventTrigger` mix desired state with observations. Keep the
existing collections until a real multi-host ownership conflict demonstrates
that a split is necessary.

## Network, pairing, and security

| Collection | Scope | Branchable | Identity/version contract | Retention | Primary consumers |
| --- | --- | --- | --- | --- | --- |
| `AgentNetwork` | Shared signed network root | Yes | `network_id`, `_docID`, exact CID and signer | SEC | network control, P2P |
| `NetworkJoinRequest` | Shared signed admission request | Yes | `request_key`, `_docID`, admission/transition CID | SEC | network admission |
| `NetworkMembership` | Shared signed grant/revocation ledger | Yes | `membership_key`, `_docID`, exact CID and signer | SEC | replication admission |
| `PeerEndpoint` | Shared signed DID/node/address binding | Yes | logical DID/node key, `_docID`, CID at use and signer | SEC + CFG | peer discovery |
| `PairingBearerClaim` | Shared signed bearer claim | Yes | token, `_docID`, exact CID and signer | SEC | pairing protocol |
| `BearerPairingReady` | Shared signed issuer acknowledgement | Yes | `readiness_key`, `_docID`, exact CID and signer | SEC | pairing protocol |
| `ConsumedInviteNonce` | Shared replay-prevention ledger | **Yes** | nonce, `_docID`, exact CID | SEC | invite admission |
| `PeerRegistry` | Shared fleet peer registry | **Yes** | `peer_id`, `_docID`, CID at use | CFG + SEC | peer reconcile, discovery |
| `DataPlanePairingDesired` | Local replication intent | No | peer/agent key, `_docID` | OP | data-plane reconcile |
| `PeerPairingDesired` | Local pairing intent | No | `peer_id`, `_docID` | OP | peer reconcile |
| `PeerPairingApplied` | Local applied observation | No | `peer_id`, `_docID` | OP | peer reconcile |
| `ReciprocalConversationIntent` | Local reciprocal-replication intent | No | `member_did`, `_docID` | OP | conversation pairing |

## Remaining review gates

The matrix deliberately leaves only evidence-backed work:

1. Exercise signed provenance plus timeline and all adapter projections through
   the real single-node demo. DefraDB P2P/CRDT replication is a later test of
   the same document contract, not a new runtime control plane.
2. Add authenticated caller identity to the remote HTTP GraphQL envelope before
   treating remote requests as caller-signed ACP evidence. Embedded runtime
   writes and explicit local transactions already use the node signer.
3. Decide whether a future historical-replay requirement justifies capturing
   exact config and transcript source CIDs. Current projections deliberately
   advertise current-snapshot `CapturedOnly` provenance instead.
