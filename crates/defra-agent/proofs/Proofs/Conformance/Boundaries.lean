/-!
# Conformance Boundaries and Product Policies

This file records intentional product semantics and external assumptions at the
Rust/Lean boundary. These are not active spec deviations.

## Current Request Lifecycle Product Semantics

The persisted `AgentRequest.lifecycle_state` vocabulary has nine strings:
`pending`, `claimed`, `processing`, `inputRequired`, `completed`, `failed`,
`superseded`, `dead`, and `interrupted`.

The current core request transition machine uses these current-product paths:

* `pending -> claimed` for successful claim while `valid_until` is open.
* `pending -> superseded` for losing latest-only/deduplication arbitration.
* `claimed -> processing` when inference execution begins.
* `processing -> processing` for progress.
* `processing -> completed` for successful terminal completion.
* `processing -> failed` for provider failure, retry exhaustion, tool failure,
  or post-claim deadline expiry.
* `claimed -> failed` for failure before streaming starts.
* `pending -> dead` only for stale pre-claim TTL expiry.
* `pending | claimed | processing -> interrupted` for cancellation.

`inputRequired` is reserved persisted/client protocol vocabulary. Rust does not
currently emit it because there is no first-class approval or human-input loop;
autonomous tool calls run inline. Future approval work should add an explicit
extension module or widen the core transition relation together with Rust writer
tests.

`dead` is a real terminal persisted state only for stale pre-claim work. Once a
request is claimed, provider failures, retry exhaustion, tool failures, and
deadline expiry remain terminal `failed`.

## Intentional Product Policies

Tool-call failures are classified as permanent until tool metadata can prove
retry safety. Retrying tool calls without health, idempotency, and side-effect
metadata can repeat side effects, so the request transition system does not
model tool retries and Rust treats `StreamingError::Tool(_)` as a permanent
failure.

`Proofs.ToolExecution` is the initial local model for future MCP/tool execution
semantics. It currently proves only the service-local boundary Rust enforces:
unreachable services and invalid preflight schemas block dispatch, `list_tools`
transport retries are safe-read retries, and `call_tool` transport retries
require explicit idempotency evidence. Rust does not currently persist or
consume idempotency metadata for MCP tools, so `McpPool::call_tool` must not
implicitly retry after dispatch failure. Future tool retries should first extend
`ToolExecution.IdempotencyEvidence`, add a Rust contract for the advertised
metadata, and only then widen `McpPool::call_tool` retry behavior.

The scheduler's aggregate fleet slot state is reconstructed from
`InferenceCall` rows. A backend's held slot count is the derived count of rows
with `call_state = "running"`; queued rows are waiting for a semaphore permit
and terminal rows (`cancelled`, `completed`, `failed`) have released any permit.
There is intentionally no denormalized persisted `FleetState` document that
must carry the aggregate invariant.

The command-policy model covers local validation and selection logic for
`CommandExecutionMode`, `CommandNetworkMode`, argv allowed/forbidden prefixes,
read-only command allowlisting, sandbox labels, and filtered shell environment
keys. It does not prove that an invoked external binary is semantically
read-only, nor does it prove the host kernel's sandbox implementation. Rust
tests cover the parser/validator boundary and command metadata emitted by
`toolset/shared/command.rs`; the Lean model covers the fail-closed policy
ordering and sandbox/env invariants that those tests exercise.

## External Assumptions

The `PersistenceState` model abstracts the storage commit boundary. Rust uses
`StreamBuffer`, `DefraSessionHook`, and hook failure policy around DefraDB
mutations; it does not persist a per-token `PersistenceState` document. The
assumption is: DefraDB mutations that return success are durable for the modeled
stream/session writes. Storage-engine crash windows, transport delivery, and
CRDT/event-delivery guarantees are outside the core request proof.

Backend health and availability observations are only as fresh as the backend
documents visible at admission time. Endpoint freshness and network/provider
behavior are environmental assumptions, not service-local state-machine facts.
The service-local proof and tests cover the consequence of an observed backend
configuration: reconstructed running call rows do not exceed that backend's
`max_concurrent`.

The generated `SessionRecovery` conformance contract currently covers the
finite failed-latest-request reissue witness (`failed -> pending`) instead of
the full request lifecycle vocabulary. It is a smoke contract for the executable
session boundary, not a complete request-state coverage claim. Future
session-recovery executable witnesses should widen that contract before Rust
depends on broader transition coverage from it.

## Closed Historical Items

These were previous conformance gaps and are now closed product/spec behavior:

* `recovering` is an explicit persisted process state. Startup publishes it
  before recovery work and only starts request watchers/routers after
  `recover_all()` and startup runtime publication complete.
* Claim and inference retry waits are bounded by submitter TTL and claimed
  request deadlines; stale pre-claim rows become `dead/Stale`.
* Interrupting a request has an end-to-end path to cancelling queued/running
  linked `InferenceCall` rows.
-/
