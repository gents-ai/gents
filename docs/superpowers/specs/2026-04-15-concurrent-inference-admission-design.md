# Concurrent Inference Call Admission Design

Moves `max_concurrent` enforcement from the request level (where it doesn't
match workload reality) to the HTTP-call level, adds a bounded FIFO wait
queue that prefers waiting over failing, and records each call's stats via
a terminal `InferenceCall` document. Removes the now-redundant request-level
slot bookkeeping and simplifies the Lean model to match.

## Problem

An `AgentRequest` typically spans many HTTP calls to its backend — one per
agentic turn (model call → tool call → model call → …), plus support calls
such as compaction summaries. The current concurrency gate
(`BackendTracker::try_acquire_permit`) is per `AgentRequest`, not per HTTP
call. That is the wrong unit: a request-level slot is held while no backend
HTTP call is in flight (tool execution, streaming response processing,
retry backoff, delegate waits), and any model invocation that does not sit
under that exact request-level guard is easy to miss. Under benchmarking, RL
training, scheduled tasks, compaction, or subagent fan-out workloads the
observed backend load is therefore not the same thing as the declared
`max_concurrent`. Backends that tolerate moderate concurrent HTTP load can go
unsaturated, while backends that do not can still be stressed by ungated call
paths.

There is also no queue. When `BackendTracker` is full, today's behavior is
fast-fail; there is no way to simply wait for capacity. This blocks the "plug
into inference and go" use case for bursty workloads.

Finally, there is no per-call stats record. Operators can see per-request
state but not "call 3 of request R waited 4s before running and took 2s" —
which is the load-bearing signal for benchmarking and RL reward shaping.

The existing request-level admission machinery (`AgentRequest.admission_state`,
`BackendTracker`/`BackendPermit`, `Proofs/Scheduling.lean`'s `AdmissionState`
+ `SchedulerState` + `capacityInvariant`) was designed under the assumption
that a request maps 1:1 to a backend call. Under agentic workloads that
assumption does not hold. The fix is to move concurrency enforcement to the
unit that actually matches backend load — the HTTP call — and to remove the
request-level slot bookkeeping whose invariant is now meaningless.

## Scope

In scope:

- Enforce `max_concurrent` against **concurrent HTTP calls** to a single
  backend, via a new admission controller built on `tokio::sync::Semaphore`.
- Bounded FIFO wait queue with **wait-by-default** semantics. Queue overflow
  rejects as a safety valve against unbounded memory growth.
- A new `InferenceCall` document written exactly once per call, at the
  call's terminal moment, with the full collected stats.
- Remove `AgentRequest.admission_state`, `BackendTracker`, `BackendPermit`,
  and the `Proofs/Scheduling.lean` slot-accounting vocabulary. Update
  `Proofs/Request.lean` and downstream proofs/tests accordingly.
- New Lean module `Proofs/CallAdmission.lean` carrying the concurrency
  invariants that used to live at the request level, now stated over calls.

Out of scope (tracked separately):

- Multi-backend pools, failover, capability-based routing, external model
  router — deferred; `AgentBehavior` stays bound to one backend.
- **Cancellation mechanism — acknowledged as needing further design work.**
  A `cancelled` terminal state is reserved in the schema and state machine
  so v1 is future-proof, but the delivery path (how a parent request's
  cancellation reaches the controller, whether the HTTP future is aborted,
  what happens to token counts for a cancelled-but-completed response) is
  left for a follow-up mini-spec.
- Deadline-aware scheduling. The `priority` field exists on `InferenceCall`
  for future use; v1 ignores it.
- Distributed admission coordination across multiple agent processes.
- Live queue introspection via GraphQL — only terminal records are
  queryable.

## The refactor

The existing request-level admission model and the new call-level model
can't coexist cleanly using the same `max_concurrent` knob. The user's
resolution: `max_concurrent` describes backend HTTP-call load (what actually
matters for saturation and backend health), and the request-level slot
layer is removed.

What goes away:

Unless otherwise noted, implementation paths below are relative to
`crates/defra-agent/`.

| Thing | Where |
|---|---|
| `AgentRequest.admission_state` field | `schemas/agent/agent_request.graphql` |
| `BackendTracker`, `BackendPermit`, `try_acquire`/`try_acquire_permit`/`release`/`running_count` | `src/backend_registry.rs`, `src/scheduler/execution.rs`, `src/agent/daemon/request.rs`, `src/agent/daemon.rs`, `src/agent/runtime.rs`, `src/scheduler.rs` |
| request admission-state writes, filters, and materialization defaults | `src/lifecycle/*.rs`, `src/toolset/delegate.rs` |
| `AdmissionState`, `holdsSlot`, `SchedulerState`, `SchedulerState.capacityInvariant` | `proofs/Proofs/Scheduling.lean` |
| `RequestContext.admission`, `coherentStateAdmission`, `releaseToTerminal` admission updates, `claimed_coherent_cases` | `proofs/Proofs/Request.lean` |
| Scheduler slot conformance / deviations tied to request-level `running` counts | `proofs/Proofs/Conformance/SchedulerConformance.lean`, `proofs/Proofs/Conformance/Deviations.lean`, `proofs/Proofs/Properties/SchedulingSafety.lean`, `proofs/Proofs/Properties/SchedulingLiveness.lean` |
| `admission_state` expectations in conformance/regression tests and their helpers | `tests/state_machine_conformance.rs`, `tests/lifecycle_regression.rs`, `tests/support/mod.rs`, `tests/support/snapshots.rs` |
| `admission_state` column in `AgentRequest` CLI queries | `crates/defra-agent-cli/src/main.rs` |
| `admission_state` in the protocol-crate plan's row mirror | `docs/superpowers/plans/2026-04-13-defra-agent-protocol-crate.md` (editorial — the plan isn't implemented yet) |

What replaces it: the new call-level admission (everything below).

Request lifecycle state (`RequestState`: 8 states, `pending → claimed →
processing → …`) is unchanged. The `claimed` state used to mean "scheduler
holds a backend slot for this request"; after the refactor it simply means
"scheduler has claimed this request and is about to start inference." It
still earns its keep as a deduplication window (`dedup_lose`) and a
pre-inference failure point (`fail_before_stream`), but it reserves no
backend resource. No bookkeeping beyond the request's own status flags.
The first HTTP call from the request is what interacts with the admission
controller.

A deeper consequence: after this refactor, an `AgentRequest`'s relationship
to a backend is purely **referential** rather than reservational.
`AgentRequest.backend_id` records which backend handled (or is handling)
the request, but it does not reserve any of that backend's capacity.
Capacity is consumed strictly per HTTP call. This is the shape that makes
future pool / failover / router work clean — a behavior that wanted to
pick a backend per-call rather than per-request wouldn't need to fight
against request-level slot accounting, because there isn't any.

**No request-level queuing in v1.** A natural question is whether the
refactor should reintroduce a `max_queued_requests` knob at the request
level — e.g. to give caller-side backpressure when a subagent fanout
exceeds backend capacity by orders of magnitude. The answer for v1 is no:
the call-level queue already absorbs bursts up to `max_queue_depth`, and
beyond that the `rejected` terminal is the feedback mechanism. Callers
that need finer-grained backpressure (a subagent scheduler that wants to
throttle fanout based on backend pressure) can query the `InferenceCall`
log or add it later as a separate concern. v1 keeps the request layer
free of admission entirely.

## Decisions at a glance

| Decision | Choice | Rationale |
|---|---|---|
| Unit of concurrency | HTTP call | Matches backend load reality under agentic workloads. |
| `max_concurrent` semantics | Concurrent HTTP calls per backend | No rename — the name already implies calls in industry parlance; we are fixing the implementation to match. |
| Existing request-level slot layer | Remove | Its invariant (`running requests ≤ max_concurrent`) is meaningless for agentic traffic; keeping it alongside call-level admission would double-count the same physical resource. |
| Controller location | In-memory `AdmissionRegistry`, one controller per available backend | Admission is ephemeral runtime state; per-transition DB writes would dominate the call cost. |
| Backend admission config | Snapshot-owned `BackendAdmissionConfig` map | Capacity is a backend property; it must not be hidden in `BehaviorConfig`. |
| Runtime entrypoint | `AdmittedCompletionClient<C>` / `AdmittedCompletionModel<M>` around rig completion | The only place that reliably sees every rig provider HTTP call, including multi-turn, scheduled, and compaction calls. |
| Agent builder shape | New admitted builder for runtime/scheduler; existing raw builder remains for oneshot | `rig::CompletionModel` requires `make`, so the admitted wrapper needs a small client wrapper; public oneshot calls have no `AgentRequest` and stay outside v1 admission. |
| Per-request call metadata | Async-scoped `AdmissionCallContext` | Keeps long-lived rig agents and tool setup reusable while making request attribution explicit at call sites. |
| `InferenceCall` write strategy | Exactly one write, at terminal state | Preserves stats-for-observability without paying for a per-transition write log. |
| Terminal-write idempotency | Stable `call_id` from runtime instance + per-request call sequence; duplicate terminal insert is treated as already written | Drop-spawned writes must be harmless if shutdown/retry races surface the same terminal record twice, while restarted processes must not collide with already-recorded calls. |
| Queue default behavior | **Wait, don't fail** | The primary use case is saturating inference bandwidth, not rejecting load. A call would rather wait than fail. |
| Queue overflow policy | Bounded FIFO; reject only on overflow | Overflow is a memory safety valve, not the normal failure mode. |
| `max_queue_depth` default | 100 (`0` ⇒ no waiting; immediate permit still runs) | Absorbs realistic bursts; bounded memory footprint; revisit with real data. |
| Queue overflow parent error | Retryable backpressure | `QueueFull` records `AdmissionRejected` and maps to retryable inference backoff, not permanent request failure by default. |
| Cancellation from `running` | Placeholder state reserved; mechanism deferred | User flagged cancellation as needing more design. State exists so schema and state machine are stable; the delivery mechanism is a follow-up. |
| Capacity decreases | Drain, do not preempt | Existing HTTP calls keep running; no new controller generation is installed until the old generation drains, so v1 avoids aborting calls while preventing new oversubscription. |
| Total call deadline | None in v1 | Running calls rely on existing stream liveness timeout and caller drop/shutdown; no wall-clock cap is added here. |
| Semaphore primitive | `tokio::sync::Semaphore` (owned-permit variant) | Battle-tested; built-in FIFO-fair acquire; RAII `OwnedSemaphorePermit`. |
| Field name on `InferenceCall` | `call_state` | Avoids any residual collision with the soon-to-be-deleted `admission_state`; keeps the new vocabulary cleanly namespaced. |
| Priority | Field added, not wired | Cheap to reserve; prevents schema churn when agentic-starvation becomes a real concern. |
| Source of truth for the state machine | Lean 4 | Project norm. |

## Architecture

### Placement

```
AgentRequest  (Pending → Claimed → Processing → Completed/Failed/...)
   │
   └── performs 1..N HTTP calls per turn/tool cycle
          │
          └── InferenceCall  (queued → running → released | rejected | cancelled*)
              call-level admission
              acquired via BackendAdmissionController
              (* cancelled transitions reserved in v1, mechanism deferred)
```

The request lifecycle no longer tracks backend-slot holding; it reflects
only the request's own progress. Every HTTP call the request makes passes
through `BackendAdmissionController::acquire` and carries its own
`InferenceCall` lifecycle, independent of request state.

### `AdmissionRegistry`

`AdmissionRegistry` is the runtime owner of admission state. It is created in
`agent/runtime.rs`, stored on `RuntimeContext`, and shared with behavior
daemons, the scheduler, and any code that builds backend-bound rig agents.
`backend_registry.rs` remains the DefraDB document parser / lookup layer; it
does not become a live controller supervisor.

The registry maintains one `Arc<BackendAdmissionController>` per available
backend (`enabled && probe_status == "healthy"`). Runtime reconcile updates
the registry from a snapshot-owned backend admission map, not from
`BehaviorConfig`.

```rust
pub struct BackendAdmissionConfig {
    pub backend_id: String,
    pub max_concurrent: usize,
    pub max_queue_depth: usize,
    pub enabled: bool,
    pub probe_status: String,
    pub config_fingerprint: String,
}
```

`ResolvedRuntimeSnapshot` and `ActiveRuntimeSnapshot` carry
`HashMap<String, BackendAdmissionConfig>`. `BehaviorConfig` keeps the fields
needed to build a behavior's provider client, but does not own admission
capacity. The apply/snapshot layer rejects `max_concurrent < 1` and
`max_queue_depth < 0`; `max_queue_depth = 0` remains valid.

The registry also generates a `runtime_instance_id` UUID at startup, used in
`InferenceCall.call_id`.

Unchanged backend admission configs are reused. Removed, disabled, unhealthy,
or capacity-changed backends close their old controller and move it to a
draining set. While a backend has a draining controller, no replacement
controller for that backend is installed and new acquire attempts return
`BackendGone` as retryable transient admission failure. When the last in-flight
permit from the draining controller drops, the registry installs the latest
pending healthy config, if one exists. v1 chooses this conservative drain
instead of overlapping controller generations, because overlapping old and new
semaphores can violate the backend-level `max_concurrent` bound during capacity
changes or unhealthy→healthy flaps.

For capacity decreases, already-running calls from the old generation may
temporarily exceed the new configured target. v1 does not abort them; it
prevents additional admission until those calls drain.

The registry key is `backend_id`, so multiple behaviors sharing one backend
also share one controller and one `max_concurrent` budget.

`AdmissionRegistry::acquire` assigns the final `call_id` before it looks up the
active controller. If the backend has no active controller because it is
missing, unhealthy, disabled, or draining after reconfiguration, the registry
writes a terminal `InferenceCall { call_state = cancelled, failure_reason =
BackendGone }` for that attempt and returns `Err(BackendGone)`. Missing async
context is the one exception: without request metadata, the wrapper returns a
programming error and writes no `InferenceCall`.

### `BackendAdmissionController`

One instance per backend currently available in the `AdmissionRegistry`.
Completion code resolves it at call time via `backend_id`.

Ownership:

- An `Arc<tokio::sync::Semaphore>` sized to `InferenceBackend.max_concurrent`,
  used via `Semaphore::acquire_owned` so permits can live in a RAII guard
  detached from the controller's lifetime.
- A counter of current waiters (pending `acquire` futures), used to enforce
  `max_queue_depth`. Tokio's semaphore gives FIFO fairness but does not
  expose waiter count; we track it in a `Mutex<usize>` alongside the
  per-call metadata needed for the terminal write.
- A monotonically increasing registration sequence, assigned when an acquire
  future actually enters the semaphore wait path. This is the fairness order
  the Lean model talks about.
- A tracking map of `call_id → CallMetadata` (`queued_at`, `started_at`,
  etc.), built up over the call's life and consumed at terminal write.
- The terminal-write sink (GraphQL mutation against `EmbeddedNode`).
- A closed/draining bit. Closed controllers admit no new waiters. A draining
  controller keeps only its in-flight permit holders until they drop.

Public surface (sketch):

```rust
impl AdmissionRegistry {
    async fn acquire(&self, backend_id: &str, meta: CallMetadata) -> Result<PermitGuard, AdmissionError>;
}

impl BackendAdmissionController {
    async fn acquire(&self, meta: CallMetadata) -> Result<PermitGuard, AdmissionError>;
}

pub struct PermitGuard {
    // holds OwnedSemaphorePermit + call metadata + stats sink
}

struct WaiterRegistration {
    // decrements waiter count on Drop unless consumed by a permit grant
}

impl PermitGuard {
    pub fn record_usage(&mut self, prompt_tokens: u32, completion_tokens: u32);
    pub fn mark_backend_error(&mut self, err: &BackendError);
    // terminal write (call_state=released) happens on Drop
}

pub enum AdmissionError {
    QueueFull,       // surfaces as InferenceCall { call_state: rejected }
    BackendGone,     // no active controller, or controller closed mid-wait
}
```

`acquire` is the wait-by-default entrypoint. It:

1. Records `queued_at` and first tries `Semaphore::try_acquire_owned`.
   If a permit is immediately available, the call enters `running` without
   consuming queue depth. This is what makes `max_queue_depth = 0` mean
   "do not wait" rather than "reject every call."
2. If no permit is immediately available, takes the waiter-count lock. If
   `waiters >= max_queue_depth`, drops the lock, synchronously writes an
   `InferenceCall` with `call_state = rejected` and
   `failure_reason = AdmissionRejected`, and returns `Err(QueueFull)`.
3. Otherwise, assigns a registration sequence, increments the waiter count,
   stores waiter metadata, drops the lock, and awaits
   `Semaphore::acquire_owned`, which suspends until a permit is available.
   Tokio's internal fairness applies to semaphore registration order, not
   wall-clock timestamp order.
4. A `WaiterRegistration` RAII guard owns the waiter-count increment while
   the future is pending. If the acquire future is dropped because of
   shutdown, task abort, timeout, or future cancellation, the guard still
   decrements the waiter count and removes waiter metadata. The cancellation
   mini-spec decides when arbitrary parent-request cancellation writes a
   terminal `cancelled` call; controller close writes `BackendGone`
   explicitly as described below.
5. On permit grant, consumes the `WaiterRegistration`, records `started_at`,
   and returns a `PermitGuard` carrying the permit, timestamps, registration
   sequence, and a handle to the terminal sink.

Drop of `PermitGuard` spawns a fire-and-forget task that writes the
terminal `InferenceCall` document with `call_state = released` (or
`cancelled` when the cancellation mechanism lands) and the stats the caller
recorded via `record_usage` / `mark_backend_error`. Write failures are
logged but do not stall the backend.

### Lifecycle

A controller is created when its backend is present and available in the
`AdmissionRegistry`, and closed when the backend leaves the registry
(provider removal, config disable, unhealthy transition,
admission-capacity/config change, shutdown). Close is explicit:

- The registry calls `Semaphore::close()` and marks the controller closed.
- Pending `acquire` futures resolve with `Err(BackendGone)`.
- Exactly one terminal `InferenceCall` with `call_state = cancelled` and
  `failure_reason = BackendGone` is written for each cancelled waiter. The
  close path marks waiter metadata terminal before spawning writes, so resumed
  waiter futures and RAII cleanup observe the already-written state instead of
  writing a second terminal document.
- In-flight permit holders finish naturally (their `PermitGuard` outlives
  the controller) and write their own terminal docs on drop. The controller
  notifies the registry when the final permit drops so any pending replacement
  config can be installed.

Health gates new admission only. If a backend flips unhealthy, pending queued
calls are cancelled with `BackendGone`, but already-running HTTP calls are not
aborted by v1 admission.

### Rig integration and call coverage

Admission attaches to rig's `CompletionModel`, not to the outer request
handler. `completion_factory.rs` should keep the current raw `build_agent`
helper for public oneshot callers and add a separate admitted builder used by
runtime behavior daemons and scheduled-task execution.

The admitted builder wraps the provider client before constructing the agent.
This is slightly more code than a model-only wrapper, but it fits rig's trait
shape: `CompletionModel` requires `fn make(client: &Self::Client, model)`, so
the wrapper needs a client type that can construct admitted models.

Sketch:

```rust
#[derive(Clone)]
pub struct AdmittedCompletionClient<C> {
    inner: C,
    admission: Arc<AdmissionRegistry>,
}

#[derive(Clone)]
pub struct AdmittedCompletionModel<M> {
    inner: M,
    admission: Arc<AdmissionRegistry>,
}

impl<C: CompletionClient> CompletionClient for AdmittedCompletionClient<C> {
    type CompletionModel = AdmittedCompletionModel<C::CompletionModel>;

    fn completion_model(&self, model: impl Into<String>) -> Self::CompletionModel {
        AdmittedCompletionModel {
            inner: self.inner.completion_model(model),
            admission: self.admission.clone(),
        }
    }
}

impl<M: CompletionModel> CompletionModel for AdmittedCompletionModel<M> {
    type Response = M::Response;
    type StreamingResponse = M::StreamingResponse;
    type Client = AdmittedCompletionClient<M::Client>;

    fn make(client: &Self::Client, model: impl Into<String>) -> Self {
        client.completion_model(model)
    }

    async fn completion(&self, request: CompletionRequest) -> Result<CompletionResponse<M::Response>, CompletionError> {
        let context = AdmissionCallContext::current()?;
        let mut permit = self.admission.acquire(&context.backend_id, context.call_metadata()).await?;
        let result = self.inner.completion(request).await;
        match &result {
            Ok(response) => permit.record_usage(response.usage.input_tokens, response.usage.output_tokens),
            Err(error) => permit.mark_completion_error(error),
        }
        result
    }

    async fn stream(&self, request: CompletionRequest) -> Result<StreamingCompletionResponse<M::StreamingResponse>, CompletionError> {
        // acquire permit, call inner.stream(request), and keep the permit alive
        // until the provider response stream ends or is dropped
    }
}
```

The wrapper is built once with the behavior runtime, but request attribution
comes from async-scoped context. Each parent request gets a root
`AdmissionCallContext` with request/behavior/backend identity and a shared
per-request call sequence counter. Narrow nested scopes set the call kind and
attempt for the operation that may call the model:

```rust
admission::scope(
    AdmissionCallContext::for_request(...),
    async {
        admission::scope_call_kind(CallKind::Compaction, maybe_compact_prompt()).await?;
        admission::scope_call_kind(CallKind::Inference, daemon.run_inference(...)).await
    },
).await
```

Scheduled tasks materialize an `AgentRequest` and use `CallKind::Scheduled`
around their `.prompt` calls. Prompt compaction wraps only the summary model
call with `CallKind::Compaction`; prompt preparation that only reads DefraDB or
builds local history is outside admission. Retry loops update the scoped
`attempt` before each model attempt. This keeps the long-lived rig agent and
tool surface intact while avoiding stale request metadata in the model wrapper.

If an admitted model is used without an `AdmissionCallContext`, that is a
programming error. The wrapper maps it to a non-retryable completion error and
does not write `InferenceCall`, because there is no safe request identity to
record. Tests should cover that every runtime/scheduler admitted call path is
scoped. Public `oneshot.rs` keeps using the raw builder and is out of scope for
v1 admission telemetry.

For non-streaming completion this can be implemented entirely outside rig.
For streaming completion, the permit must live until the provider response
stream reaches EOF or is dropped; holding it only until `inner.stream(request)`
returns would undercount long-running streams. `rig-core 0.31` keeps the raw
stream inside `StreamingCompletionResponse` private, so this implementation
requires a small rig adapter seam in this repo's dependency boundary, such as:

- `StreamingCompletionResponse::into_raw_stream` plus a constructor from a raw
  `StreamingResult`, or
- a provider-call hook around `CompletionRequestBuilder::stream()` /
  `CompletionModel::stream()` that can hold arbitrary RAII state for the raw
  stream lifetime.

This adapter seam is in scope for the Rust wiring. Without it, the design
cannot honestly enforce per-HTTP-call concurrency for rig multi-turn streaming.
The wrapped raw stream records token usage from each final provider response
via `GetTokenUsage`, marks backend errors when polling yields
`CompletionError`, and drops the `PermitGuard` on EOF or stream drop.

Because every backend-bound runtime/scheduler agent is built with the admitted
model, the same call-level limit covers:

- interactive `stream_prompt` model turns,
- scheduled-task `.prompt` model turns,
- compaction summary calls through `DefraCompactor`,
- retry attempts, each as its own `InferenceCall`.

Health probes, model discovery, and public oneshot calls are not counted by
`max_concurrent` in v1; they are operational/non-request paths rather than
runtime `AgentRequest` inference calls.

### Integration points

- `src/backend_registry.rs` — gains `max_queue_depth` parsing / queries; loses
  `BackendTracker` and `BackendPermit`.
- `src/admission/` (new module) — `AdmissionRegistry`,
  `BackendAdmissionController`, `AdmittedCompletionModel`, `PermitGuard`,
  `WaiterRegistration`, `AdmissionCallContext`, `AdmissionError`,
  `CallMetadata`.
- `src/completion_factory.rs` — keeps raw `build_agent` for oneshot/tests and
  adds admitted builder support via `AdmittedCompletionClient` /
  `AdmittedCompletionModel` for runtime and scheduler paths.
- `src/agent/runtime.rs`, `src/agent/reconcile.rs` — own and reconcile the
  `AdmissionRegistry` and backend admission config maps alongside behavior
  generations.
- `src/scheduler/execution.rs`, `src/agent/daemon/request.rs`,
  `src/agent/runtime.rs` — lose their `BackendTracker` acquire calls and
  field holdings.
- `src/lifecycle/*.rs`, `src/toolset/delegate.rs` — lose
  `AgentRequest.admission_state` writes, filters, query fields, and defaults.

### Error mapping

Admission errors cross rig as completion errors because `CompletionModel`
methods return `rig::completion::CompletionError`.

- `AdmissionError::QueueFull` writes
  `InferenceCall { call_state = rejected, failure_reason = AdmissionRejected }`
  before returning. The wrapper maps it to a sentinel provider error such as
  `ProviderError("admission rejected: queue full")`. `classify_completion_error`
  maps that sentinel to retryable backpressure, preferably
  `InferenceError::RateLimited { retry_after_secs: 1 }`.
- `AdmissionError::BackendGone` writes
  `InferenceCall { call_state = cancelled, failure_reason = BackendGone }`
  for registered waiters and for acquire attempts that find no active
  controller, then maps to `InferenceError::TransientFailure` unless the
  caller is already shutting down.
- The admission controller itself does not retry. Existing inference retry
  policy handles retryable admission failures using the same backoff path as
  transient provider failures, and only when no observable response activity
  has already been emitted.

### Terminal write idempotency

`AdmissionRegistry` generates a `runtime_instance_id` UUID at startup.
`AdmissionCallContext` owns an `Arc<AtomicU64>` per parent request and assigns
`call_seq` before each acquire attempt. The call id is then stable and
human-debuggable:

```text
inference-call:{runtime_instance_id}:{request_id}:{call_seq}
```

The `call_seq` is monotonic within the parent request and is written to
`InferenceCall`, so operators can query "call 3 of request R" without parsing
the id. The runtime instance component prevents collisions if a process writes
a terminal call document and then crashes before the parent `AgentRequest`
reaches a terminal lifecycle state; the restarted process may replay the
request from scratch, but its new calls get a different id prefix.

Terminal writes use `call_id` as the unique key. The terminal writer also
carries a local "already written" guard so Drop paths cannot enqueue two
writes in normal control flow.

If DefraDB reports a duplicate-key error for the same `call_id`, the writer
treats it as success and logs at debug level. Any other write failure is logged
and not retried in v1.

### Recovery and cancellation semantics

Admission has no recovery responsibility of its own. A crash takes the
controller with it; all `InferenceCall`s that were `queued` or `running`
at crash time leave no terminal document and are simply lost. This is
acceptable: the authoritative state for "the request is not done yet" is
the parent `AgentRequest`'s lifecycle, and the existing retry path is
what picks the work back up.

Because this is the recovery model, one claim holds throughout — **a
cancelled or crashed call discards any partial response and restarts the
parent request from scratch**. Concretely:

- Agent crash while a request is in `processing`, any HTTP call in flight:
  on restart, the request's retry path re-enters `processing` and makes a
  fresh `InferenceCall`. Any partially-streamed response tokens written
  before the crash are discarded. Token counts from the lost call are not
  recovered; the fresh call spends tokens again (potential double-spend is
  accepted and not deduplicated).
- Parent request cancelled (v2 cancellation work) while streaming:
  same shape — partial response is discarded, permit released. If the
  cancellation is followed by a retry rather than a terminal, the fresh
  attempt starts from scratch.

The design consequence for v2 cancellation: the cancellation mini-spec
does not need to preserve partial streaming state, which makes the
mechanism materially simpler. Cancel = permit drop + partial-response
discard + let retry handle the rest.

This recovery model also means `InferenceCall` cannot be used as a
source-of-truth audit of "every HTTP call ever dispatched against this
backend" — crashed calls are invisible. The document is best read as a
ledger of *completed* admission lifecycles.

## State machine

States, five total, three terminal:

```
(enter) ── immediate permit ─────────▶ running ── call returns ───▶ released
   │                                    │
   │── saturated, room to wait ─▶ queued│── (future) cancelled ──▶ cancelled
   │                              │
   │                              └── permit acquired ───────────▶ running
   │
   └── saturated, queue full ───▶ rejected
```

Legal transitions:

| From | To | Trigger | In v1? |
|---|---|---|---|
| (enter) | `running` | `acquire` called and permit is immediately available | yes |
| (enter) | `queued` | `acquire` called, backend saturated, waiter-count under limit | yes |
| (enter) | `rejected` | `acquire` called, backend saturated, waiter-count at limit | yes |
| `queued` | `running` | FIFO-fair semaphore grants permit | yes |
| `running` | `released` | HTTP call returns (success or backend error in body) | yes |
| `queued` | `cancelled` | controller closes because backend/config went away | yes |
| `queued` | `cancelled` | parent-request cancellation reaches controller | **reserved, mechanism TBD** |
| `running` | `cancelled` | parent-request cancellation reaches controller | **reserved, mechanism TBD** |

Terminals (`released`, `rejected`, `cancelled`) are irreversible. Each
produces exactly one `InferenceCall` document write.

### Properties to prove in Lean

New module `crates/defra-agent/proofs/Proofs/CallAdmission.lean`. The
existing `Scheduling.lean` has its slot-accounting vocabulary stripped
(see Refactor); the replacement invariants live here.

Safety:

- **C-S1 per-generation call-concurrency bound** — for each backend controller
  generation `g`, `|{calls | call.backend = b ∧ call.controller_generation =
  g ∧ call.state = running}| ≤ max_concurrent(g)` at every state.
- **C-S2 waiter bound** — `|{calls | call.backend = b ∧ call.state = queued}| ≤ max_queue_depth b`.
- **C-S3 terminal irreversibility** — `released | rejected | cancelled` never transition out.
- **C-S4 transition legality** — only the transitions in the table above.
- **C-S5 no overlapping admission generations** — a backend has at most one
  active or draining controller generation that can admit or hold permits;
  replacement installation waits until the draining generation has zero running
  calls.

Liveness:

- **C-F1 FIFO fairness** — for calls `a`, `b` on the same backend that both
  enter the wait path and both reach `running`, if
  `a.registration_seq < b.registration_seq` then
  `a.started_at ≤ b.started_at` (modulo `a` cancelling or the backend closing).
  Immediate acquisitions do not participate in this FIFO claim because they
  never join the semaphore wait queue.
- **C-L1 queue drains** — while the backend remains available and existing calls terminate in bounded time, every `queued` call reaches a terminal state.

Assumptions:

- **C-A1 running-call completion assumption** — once a permit is granted, the
  provider stream eventually returns EOF/error or the caller drops it. v1 does
  not introduce a total call deadline; it relies on existing stream liveness
  timeout and caller shutdown/drop behavior.

Cancellation-related properties are stated as axioms (the `cancelled`
transitions are admitted) and discharged when the cancellation mini-spec
lands.

## Schema changes

### New: `InferenceCall`

New collection `crates/defra-agent/schemas/inference/inference_call.graphql`.
Written exactly once per call, at terminal state.

```graphql
type InferenceCall {
    call_id: String @index(unique: true)
    runtime_instance_id: String @index
    request_id: String @index
    call_seq: Int
    backend_id: String @index
    behavior_id: String @index
    agent_did: String @index
    call_kind: String @index      # inference | compaction | scheduled
    attempt: Int

    call_state: String @index       # released | rejected | cancelled
    failure_reason: String          # AdmissionRejected | BackendError | BackendTimeout | BackendGone | Cancelled (future) | null

    queued_at: DateTime
    started_at: DateTime            # null iff call never left queued
    ended_at: DateTime

    priority: Int                   # reserved, unused in v1
    queue_depth_at_enqueue: Int     # waiter count observed before this call enqueues
    controller_generation: Int
    backend_config_fingerprint: String

    prompt_tokens: Int
    completion_tokens: Int
}
```

### Changed: `InferenceBackend`

- `max_concurrent` — **semantics shift** from "max active requests holding
  a backend slot" to "max concurrent HTTP calls to this backend." No
  rename; no migration (no deployed operators to migrate). The apply layer
  rejects values below 1.
- `max_queue_depth: Int` — added. Default 100. `0` means no waiting: an
  immediately available permit still runs, but a saturated backend rejects
  instead of enqueueing. The apply layer rejects negative values.

### Removed: `AgentRequest.admission_state`

Delete the line from `schemas/agent/agent_request.graphql`. Update
`schemas/README.md` to drop it from the `AgentRequest` column listing. No
data migration needed because no production query path depends on it.

### Cross-references to update

- `schemas/README.md` — `InferenceBackend` row gains `max_queue_depth`;
  `AgentRequest` row loses `admission_state`; `InferenceCall` gets its own
  row.

## Lean changes

`Proofs/Scheduling.lean`:

- Keep `ExecutionOrigin` and `BackendId`.
- Delete `BackendState`, `AdmissionState`, `holdsSlot`, `SchedulerState`,
  `SchedulerState.capacityInvariant`, `SchedulerState.ext`.

`Proofs/Request.lean`:

- Remove `admission : AdmissionState` from `RequestContext`.
- Remove `coherentStateAdmission`, `RequestContext.coherent`,
  `claimed_coherent_cases`.
- Simplify `releaseToTerminal` to a pure state-setter (no admission field
  to reset).
- Each `Transition` constructor loses its `admission = …` preconditions.
  The resulting transition relation is strictly smaller. The shape theorems
  around it (`Trace`, `replay?`, `step_sound`, `transition_complete`,
  `replay_sound`, `trace_complete`) are structurally preserved, but each
  constructor arm needs mechanical updating to match the slimmer signature;
  the proofs themselves get simpler, not the same proofs reused verbatim.
- Theorems `terminal_implies_released_local`, `transition_produces_coherent`
  delete.

`Proofs/CallAdmission.lean` (new):

- `CallState` inductive: `queued | running | released | rejected | cancelled`.
- `BackendCapacity`: `{ max_concurrent : Nat, max_queue_depth : Nat }`.
- `Controller`: `{ generation : Nat, capacity : BackendCapacity, closed : Bool, running : Nat, queued : List (CallId × RegistrationSeq) }`.
- `Registry`: enough backend→controller state to express active vs draining
  controller generations and pending replacement configs.
- `Transition` relation with the six v1 transitions above, plus the
  parent-request cancellation transitions marked as axioms pending the
  cancellation mini-spec.
- Theorems C-S1..C-S5, C-F1, C-L1 plus assumption C-A1.

Downstream files touched:

- `Proofs/Composed.lean` — remove `.released` references to the deleted
  field; potentially simplify `Composed` constructor.
- `Proofs/Conformance/SchedulerConformance.lean` — drop scheduler-slot
  conformance; either delete file or narrow it to non-admission scheduling
  properties.
- `Proofs/Conformance/Deviations.lean` — drop S8 slot-accounting row and
  any prose about admission-state invariants.
- `Proofs/Conformance/DefraAgent.lean` — remove admission-related fixture
  setup.
- `Proofs/Properties/SchedulingSafety.lean`,
  `Proofs/Properties/SchedulingLiveness.lean` — delete (their subjects no
  longer exist) or narrow to non-slot properties.
- `Proofs/Properties/Decidable.lean`, `Proofs/Properties/Liveness.lean`,
  `Proofs/Properties/Safety.lean`, `Proofs/Fleet.lean`,
  `Proofs/SessionRecovery.lean`, `Proofs/RuntimeReconcile.lean` — remove
  admission references (uses of `.released` in default contexts, etc.).

`proofs/README.md` — remove S8 row; update the state-machine list to name
the new call-admission module; prose mentioning "admission-state invariants"
gets reframed as "call-admission invariants".

## Implementation order

1. **Streaming adapter spike.** Prove the minimum rig adapter seam before the
   broad refactor. The spike must show that an external RAII guard can be held
   until `StreamingCompletionResponse` EOF/drop while preserving final
   response aggregation and token usage extraction.
2. **Lean first.** Three commits:
   - Delete the slot-accounting vocabulary from `Scheduling.lean` and
     `Request.lean` and propagate. Build must stay green end-to-end.
   - Add `CallAdmission.lean` with state-space and legal-transition
     definitions; stub proofs with `sorry` if needed for a clean
     compile boundary.
   - Discharge C-S1..C-S5, C-F1, C-L1; state C-A1 as the running-call
     completion assumption rather than proving a total call deadline.
3. **Schema.** Add `inference_call.graphql`. Add `max_queue_depth` to
   `InferenceBackend`. Remove `admission_state` from `AgentRequest`. Update
   `schemas/README.md` and the `include_str!` wiring.
4. **Rust — remove slot layer.** Delete `BackendTracker` and `BackendPermit`
   from `backend_registry.rs`. Strip their use from `scheduler/execution.rs`,
   `agent/daemon/request.rs`, `agent/runtime.rs`, `agent/daemon.rs`,
   `scheduler.rs`, `lib.rs`, and `src/lifecycle/*.rs`. Update
   `toolset/delegate.rs` and tests to drop the `admission_state` field.
5. **Rust — add call layer.** New `src/admission/` module:
   `AdmissionRegistry`, `BackendAdmissionController`,
   `AdmittedCompletionClient`, `AdmittedCompletionModel`, `PermitGuard`,
   `WaiterRegistration`, `AdmissionCallContext`, `AdmissionError`,
   `CallMetadata`. Built on `tokio::sync::Semaphore::acquire_owned`.
6. **Wire it.** `agent/runtime.rs` owns `AdmissionRegistry` and reconcile
   keeps it aligned with snapshot-owned `BackendAdmissionConfig` entries.
   `completion_factory.rs` adds an admitted builder that wraps provider
   clients before building rig agents, while leaving the raw builder available
   for oneshot. Interactive, scheduled, retry, and compaction summary model
   calls all pass through the wrapper with scoped `AdmissionCallContext`.
7. **Apply layer / config.** Backend parsing and runtime snapshot code read
   `max_queue_depth` into `BackendAdmissionConfig`. Manifest fixtures updated.
   CLI queries stop
   selecting `admission_state` and start selecting `call_state` on
   `InferenceCall` where relevant.
8. **Conformance tests.** New `tests/admission_conformance.rs` mirrors
   C-S1..C-S5, C-F1. Update `tests/state_machine_conformance.rs` and
   `tests/lifecycle_regression.rs` to drop `admission_state` fixtures.
9. **Integration tests.** New `tests/admission_integration.rs` covers:
   waiting under saturation, FIFO admission order, rejection on overflow,
   `max_queue_depth = 0` allowing immediate permits but rejecting saturated
   waits, terminal-doc shape for each terminal, multi-behavior sharing a
   backend honouring `max_concurrent` at the call level, scheduled-task calls,
   compaction summary calls, backend-health flips cancelling queued calls but
   not aborting running calls, monotonic `call_seq` per request,
   terminal-write duplicate `call_id` handling, runtime-instance-prefixed
   `call_id` avoiding replay collisions after restart, admitted-model
   missing-context failures, no admission requirement for public oneshot, no
   overlapping controller generations during backend reconfiguration, and a
   regression showing multiple requests can be claimed/prepared while only one
   backend HTTP call runs.

The destructive refactor steps (2 Lean cleanup, 4 Rust slot removal) are
ordered after the streaming adapter spike and before the call-layer additions
so each commit remains internally consistent: the repo never sits in a state
where both layers coexist.

## Open items

- **Cancellation mechanism — out of scope for v1, reserved in the design.**
  The state machine reserves `cancelled`; the schema reserves
  `failure_reason = Cancelled`; `PermitGuard` drop releases the permit
  synchronously. v1 reaches the `cancelled` terminal only via
  `BackendGone` (controller close/drop for queued waiters).
  External-request-driven cancellation —
  how a parent request's cancellation is delivered to the controller,
  whether the in-flight HTTP future is aborted or its response discarded,
  and whether token counts from a cancelled-but-completed response are
  recorded — is deferred to a follow-up design. The v1 Rust path triggers
  `queued → cancelled` only for `BackendGone`; parent-request-driven
  `queued → cancelled` and `running → cancelled` are present in the Lean model
  as admitted axioms, so the follow-up can ship without reshaping the state
  machine.
- **`max_queue_depth = 100` default.** Informed guess; revisit against
  real workload data once this lands.
- **`failure_reason` enum stability.** String in schema for now; tighten
  to an enum once the set is stable.
- **Terminal-write failure policy.** Fire-and-forget with log line for v1.
  An on-disk spool is possible if observability gaps prove costly, but
  out of scope here.

## Relationship to wider architecture

- **Simplifies the request lifecycle.** `AgentRequest` states and
  `RequestState` transitions remain; the admission bookkeeping on top is
  gone. Proofs about request lifecycle become cleaner.
- **Tangential to Principal / Behavior / Deployment split (#9).**
  `AgentBehavior` still owns the backend binding; admission is a property
  of the backend and does not move across the principal boundary. The
  `InferenceCall` doc carries `agent_did` and `behavior_id` so audit
  continues to work after #9 lands.
- **Consistent with the document-driven control plane (#8).**
  `InferenceCall` is a new control-plane document, but only at terminal —
  the apply path does not own any `InferenceCall` fields.
- **Unrelated to the filetool work** implied by the originating worktree;
  that session pivoted into this admission design.
