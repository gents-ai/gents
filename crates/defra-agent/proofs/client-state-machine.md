# Client Turn Observation Protocol

Formal derivation reference: `Proofs/Client.lean`

Shell workflow reference: `Proofs/ClientShell.lean`

Rust reference implementation:
`crates/defra-agent-protocol/src/client_protocol.rs`

This document explains the formal model for client implementers building CLI,
web, mobile, or desktop applications against the `defra-agent` document
surface.

The Lean file captures the client-state derivation rules and their
monotonicity, terminality, totality, and retry-replacement properties. The Rust
reference implementation additionally resolves the retry tip from `request_id`
/ `retry_parent_request` links before applying those rules.

## Turn State

A client observing an agent turn derives one of 6 states:

| State | Rank | Meaning |
|---|---:|---|
| `waitingForClaim` | 0 | Request observed, no response content yet |
| `streaming` | 1 | Response observed with partial content |
| `completed` | 2 | Response terminal success |
| `failed` | 2 | Latest attempt terminal failure, no successor |
| `superseded` | 2 | A later request replaced this turn |
| `interrupted` | 2 | The operator interrupted this turn |

Rank is monotonic under valid server transitions with one exception: retry
restart, where a failed attempt (rank 2) is followed by a new pending attempt
(rank 0).

## Turn Identity

A turn is identified by `retry_root_request`, the `request_id` of the first
request in a retry chain. All retries share the same root and collapse into one
logical user turn.

The tip of the chain is the attempt the client renders. It is the most recent
attempt: the one whose `request_id` is not referenced as
`retry_parent_request` by any other observed attempt.

The Rust reference implementation derives this tip from request-link metadata
rather than trusting input slice order.

## Derivation Rules

Given the tip attempt's `AgentRequest` and its associated `AgentResponse` if
present, derive the client state using this priority:

### 1. Supersession

Highest priority:

- if `AgentRequest.superseded_by_request` is set, derive `superseded`
- if `AgentRequest.lifecycle_state` is `superseded`, derive `superseded`

### 2. Server Terminal Lifecycle States

These override any response because terminal lifecycle states are irreversible.

| `lifecycle_state` | Client state |
|---|---|
| `completed` | `completed` |
| `failed` | `failed` |
| `dead` | `failed` |
| `interrupted` | `interrupted` |

### 3. Non-Terminal Lifecycle Plus Response

If the request is in a non-terminal state (`pending`, `claimed`, `processing`,
`inputRequired`), the response may be more current than the request under P2P
replication lag. Trust the response.

| `AgentResponse.status` | Client state |
|---|---|
| `complete` | `completed` |
| `error` | `failed` |
| `streaming` | `streaming` |

### 4. No Response

Non-terminal request, no response observed: `waitingForClaim`.

In Rust, `lifecycle_state` is parsed into the closed
`RequestLifecycleState` enum before derivation so persisted request lifecycle
values cannot be confused with request or response `status` strings.

## Implementation Deviations Inherited From The Server Model

See `Proofs/Conformance/Deviations.lean` for the server-side gaps that affect
what clients may observe.

| Server state | Client mapping | Note |
|---|---|---|
| `inputRequired` | `waitingForClaim` unless a response exists | The ideal model has `inputRequired`, but Rust does not yet expose a first-class human-input flow |
| `dead` | `failed` | Rust currently persists retry exhaustion as failure-shaped state; clients can derive exhaustion externally |

`interrupted` is no longer a client-doc gap: both Lean and Rust model it as a
terminal client state.

## Stall Detection

The server liveness proofs (L1, L3) guarantee every modeled request terminates.
If a client perceives a stall, that is a transport, replication, provider, or UI
materialization problem, not a turn-state problem.

Stall detection is a per-client UI affordance, not part of the turn projection.
A reasonable heuristic: if no observation update has arrived for N seconds and
the derived state is non-terminal, show a transport health indicator. This is
not a turn state and does not affect derivation.

## Parallel Observation Surfaces

These are observed alongside the turn but do not affect turn-state derivation.
They are rendered as supplementary UI content.

### AgentToolCall

Filter: `session_id = <current session>`

Order by: `message_sequence` ascending

Key fields: `tool_name`, `args`, `result`, `status`

Rendering: inline timeline cards during streaming, showing tool invocations as
they execute. `status` tracks individual tool lifecycle
(`pending`/`running`/`completed`/`failed`).

### AgentToolResult

Filter: `session_id = <current session>`

Key fields: `tool_name`, `tool_input`, `output_text`, `truncated`,
`discarded_because_interrupted`

Rendering: full tool output for completed tools. Useful for debug views and
tool output inspection. `truncated` indicates whether output was truncated for
context-window management.

### AgentMessage

Filter: `session_id = <current session>`

Order by: `sequence` ascending

Key fields: `role`, `content`, `timestamp`

Rendering: ordered transcript for scroll-back history. The streaming bubble
reads from `AgentResponse.content`; `AgentMessage` is the persisted transcript
surface.

### InferenceCall

Filter: `request_id = <active request>`

Key fields: `call_kind`, `call_state`, `queued_at`, `started_at`, `ended_at`,
`failure_reason`

Rendering: debug or operations surfaces can show backend-admission progress.
The formal call-state vocabulary is `queued`, `running`, `cancelled`,
`completed`, and `failed` in `Proofs/InferenceCall.lean`. For interrupted
requests, queued or running linked calls have a model path to `cancelled`.
Backend-gone/controller-drain paths also use `cancelled` as a terminal call
state, independent of request interruption. Rust currently checks the admission
and permit-drop paths, while full daemon-stream interruption remains an
integration-fixture boundary.

## Subscription Model

A compliant client should observe these collections with these filters:

| Collection | Filter | Purpose |
|---|---|---|
| `AgentRequest` | `session_id = <session>` | Turn state derivation |
| `AgentResponse` | `request_id = <active request>` | Streaming content and status |
| `AgentToolCall` | `session_id = <session>` | Inline tool cards |
| `AgentToolResult` | `session_id = <session>` | Full tool output |
| `AgentMessage` | `session_id = <session>` | Scroll-back transcript |
| `InferenceCall` | `request_id = <active request>` | Backend-call progress/debug state |
| `AgentConversation` | `agent_did = <agent>` | Conversation list |

For turn-scoped observation, filter `AgentRequest` by
`retry_root_request = <turn root>` to see all attempts in a retry chain.

Polling interval guidance:

- 500-1000 ms for active turns
- 5-10 s for idle session monitoring

## Formal Notes

T2-T5 are proven in `Proofs/Client.lean`. T1 is documented there as a merge-layer
assumption; the current theorem only states that `deriveTurn` is deterministic
on an already normalized attempt list.

| Property | Statement |
|---|---|
| T1 Merge assumption | Equivalent merged observations are expected to converge before derivation; Lean records only `deriveTurn` determinism |
| T2 Monotonicity | Valid server transitions never decrease client rank |
| T3 Terminal coherence | Client terminal iff the server observation is effectively terminal |
| T4 Totality | Defined for every observation with at least one attempt |
| T5 Turn replacement | Chain extension derives from the new tip; supersession is monotonic; retry restart is the one allowed rank decrease |

`Proofs/ClientShell.lean` models local shell workflow above this per-turn
projection. It proves that selection changes are local and transport-independent,
that transport input does not mutate shell state, and that awaiting submission
state retires only after observing the matching request tip.

## Reference Pseudocode

### Swift

```swift
func deriveAttempt(request: AgentRequestState, response: AgentResponseState?) -> ClientTurnState {
    if request.supersededByRequest != nil { return .superseded }
    switch request.lifecycleState {
    case "superseded": return .superseded
    case "completed":  return .completed
    case "failed", "dead": return .failed
    case "interrupted": return .interrupted
    default: break
    }
    guard let resp = response else { return .waitingForClaim }
    switch resp.status {
    case "complete":  return .completed
    case "error":     return .failed
    case "streaming": return .streaming
    default:          return .waitingForClaim
    }
}
```

### TypeScript

```typescript
function deriveAttempt(
  request: { lifecycleState: string; supersededByRequest?: string },
  response?: { status: string },
): ClientTurnState {
  if (request.supersededByRequest) return "superseded";
  if (request.lifecycleState === "superseded") return "superseded";
  if (request.lifecycleState === "completed") return "completed";
  if (request.lifecycleState === "failed" || request.lifecycleState === "dead") return "failed";
  if (request.lifecycleState === "interrupted") return "interrupted";
  if (!response) return "waitingForClaim";
  if (response.status === "complete") return "completed";
  if (response.status === "error") return "failed";
  if (response.status === "streaming") return "streaming";
  return "waitingForClaim";
}
```

### Rust

See `crates/defra-agent-protocol/src/client_protocol.rs` for the full reference
implementation, including typed lifecycle parsing, the derivation function, and
metadata-based chain resolution. The conformance suite in
`crates/defra-agent-protocol/src/client_protocol/tests.rs` exercises the full
derivation table plus T2/T3/T5 spot checks against the Lean model.
