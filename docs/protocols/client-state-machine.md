# Client Turn Observation Protocol

Formal derivation reference: `crates/defra-agent/proofs/Proofs/Client.lean`

This document explains the formal model for client implementers building
CLI, web, mobile, or desktop applications against the defra-agent
document surface.

The Lean file captures the client-state derivation rules and their
monotonicity/terminality properties. The Rust reference implementation
additionally resolves the retry tip from `request_id` /
`retry_parent_request` links before applying those rules.

## Turn State

A client observing an agent turn derives one of 5 states:

| State | Rank | Meaning |
|---|---|---|
| `waitingForClaim` | 0 | Request observed, no response content yet |
| `streaming` | 1 | Response observed with partial content |
| `completed` | 2 | Response terminal success |
| `failed` | 2 | Latest attempt terminal failure, no successor |
| `superseded` | 2 | A later request replaced this turn |

Rank is monotonic under valid server transitions (rank never decreases)
with one exception: retry restart, where a failed attempt (rank 2) is
followed by a new pending attempt (rank 0).

## Turn Identity

A turn is identified by `retry_root_request` — the `request_id` of
the first request in a retry chain. All retries share the same root and
collapse into one logical user turn.

The tip of the chain (the attempt the client renders) is the most
recent attempt: the one whose `request_id` is not referenced as
`retry_parent_request` by any other observed attempt.

The Rust reference implementation derives this tip from the request-link
metadata rather than trusting input slice order.

## Derivation Rules

Given the tip attempt's `AgentRequest` and its associated
`AgentResponse` (if any), derive the client state using this priority:

### 1. Supersession (highest priority)

If `AgentRequest.superseded_by_request` is set → `superseded`.
If `AgentRequest.lifecycle_state` is `"superseded"` → `superseded`.

### 2. Server terminal lifecycle states

These override any response — terminal states are irreversible.

| `lifecycle_state` | Client state |
|---|---|
| `"completed"` | `completed` |
| `"failed"` | `failed` |
| `"dead"` | `failed` |

### 3. Non-terminal lifecycle + response

If the request is in a non-terminal state (`pending`, `claimed`,
`processing`, `inputRequired`), the response may be more current
than the request under P2P replication lag. Trust the response:

| `AgentResponse.status` | Client state |
|---|---|
| `"complete"` | `completed` |
| `"error"` | `failed` |
| `"streaming"` | `streaming` |

### 4. No response (lowest priority)

Non-terminal request, no response observed → `waitingForClaim`.

In Rust, `lifecycle_state` is parsed into a closed
`RequestLifecycleState` enum first so persisted request lifecycle values
cannot be confused with request or response `status` strings.

## Current Deviations

See `crates/defra-agent/proofs/Proofs/Conformance/Deviations.lean`.

| Server state | Client mapping | Deviation |
|---|---|---|
| `inputRequired` | `waitingForClaim` | #2: no persisted inputRequired path |
| `dead` | `failed` | #3: clients derive exhaustion externally |

## Stall Detection

The server liveness proofs (L1, L3) guarantee every request terminates.
If a client perceives a "stall," that is a transport or replication
problem, not a turn-state problem.

Stall detection is a per-client UI affordance, not part of the turn
projection. A reasonable heuristic: if no observation update has arrived
for N seconds and the derived state is non-terminal, show a transport
health indicator. This is NOT a turn state — it does not affect the
derivation.

## Parallel Observation Surfaces

These are observed alongside the turn but do NOT affect turn state
derivation. They are rendered as supplementary UI content.

### AgentToolCall

Filter: `session_id = <current session>`
Order by: `message_sequence` (ascending)
Key fields: `tool_name`, `args`, `result`, `status`
Rendering: inline timeline cards during streaming, showing tool
invocations as they execute. `status` tracks individual tool lifecycle
(pending/running/completed/failed).

### AgentToolResult

Filter: `session_id = <current session>`
Key fields: `tool_name`, `tool_input`, `output_text`, `truncated`
Rendering: full tool output for completed tools. Useful for debug
views and tool output inspection. `truncated` indicates whether the
output was truncated for context window management.

### AgentMessage

Filter: `session_id = <current session>`
Order by: `sequence` (ascending)
Key fields: `role`, `content`, `timestamp`
Rendering: ordered transcript for scroll-back history. NOT on the
critical streaming path — the streaming bubble reads from
`AgentResponse.content`, not from AgentMessage. AgentMessage is
populated after the turn completes (or periodically during long turns).

## Subscription Model

A compliant client must observe these collections with these filters:

| Collection | Filter | Purpose |
|---|---|---|
| `AgentRequest` | `session_id = <session>` | Turn state derivation |
| `AgentResponse` | `request_id = <active request>` | Streaming content + status |
| `AgentToolCall` | `session_id = <session>` | Inline tool cards |
| `AgentToolResult` | `session_id = <session>` | Full tool output |
| `AgentMessage` | `session_id = <session>` | Scroll-back transcript |
| `AgentConversation` | `agent_did = <agent>` | Conversation list |

For turn-scoped observation, filter `AgentRequest` by
`retry_root_request = <turn root>` to see all attempts in a retry chain.

Polling interval: 500-1000ms for active turns (streaming), 5-10s for
idle session monitoring.

## Formal Notes

T2-T5 are proven in `Proofs/Client.lean`. T1 is documented there as a
merge-layer assumption; the current theorem only states that
`deriveTurn` is deterministic on an already-normalized attempt list.

| Property | Statement |
|---|---|
| T1 Merge assumption | Equivalent merged observations are expected to converge before derivation; Lean currently records only `deriveTurn` determinism |
| T2 Monotonicity | Valid server transitions never decrease client rank |
| T3 Terminal coherence | Client terminal ↔ server effectively terminal |
| T4 Totality | Defined for every observation with ≥1 attempt |
| T5 Turn replacement | Chain extension derives from new tip; supersession is monotonic; retry restart is the one allowed rank decrease |

## Reference Pseudocode

### Swift

```swift
func deriveAttempt(request: AgentRequestState, response: AgentResponseState?) -> ClientTurnState {
    if request.supersededByRequest != nil { return .superseded }
    switch request.lifecycleState {
    case "superseded": return .superseded
    case "completed":  return .completed
    case "failed", "dead": return .failed
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
  if (!response) return "waitingForClaim";
  if (response.status === "complete") return "completed";
  if (response.status === "error") return "failed";
  if (response.status === "streaming") return "streaming";
  return "waitingForClaim";
}
```

### Rust

See `crates/defra-agent/src/client_protocol.rs` for the full reference
implementation, including typed lifecycle parsing, the derivation
function, and metadata-based chain resolution. The conformance suite in
`crates/defra-agent/src/client_protocol/tests.rs` exercises the full
derivation table plus T2/T3/T5 spot checks against the Lean model.
