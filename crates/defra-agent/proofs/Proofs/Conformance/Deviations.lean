/-!
# Deviations: defra-agent vs Ideal Model

Every entry is classified using the conformance statuses required by the proof
docs:

- Closed
- Model adjusted
- Intentional design choice
- External/operational boundary
- Unresolved implementation gap

## Deviation 1: `recovering` process state
- **Classification:** Closed
- **Rust boundary:** `AgentRuntime.process_state` now has the explicit
  persisted vocabulary value `recovering`; startup publishes `recovering`
  before recovery work and reaches `ready` only after runtime publication.
  The closure is by startup ordering: request watchers/routers are created
  after `recover_all()` and startup runtime publication, rather than by every
  claim path re-reading `AgentRuntime.process_state`.
- **Lean references:** `ProcessState.toDefraDB`,
  `ProcessState.fromDefraDB_toDefraDB`, and
  `DefraProcessState.recovering_blocks_work`.
- **Rust references:** `runtime_status::tests::rust_process_state_vocabulary_matches_lean_model`,
  `runtime_status_persists_process_and_reconcile_state`, and
  `startup_recovery::run_agent_starts_when_startup_probe_cannot_validate_model`.

## Deviation 2: `inputRequired` request state
- **Classification:** Intentional design choice
- **Rust boundary:** `inputRequired` is reserved in the persisted lifecycle
  vocabulary and client protocol, but the current product runs autonomous tool
  calls inline and has no human/tool-approval loop that would intentionally
  block a request for external input.
- **Lean references:** `RequestState.toDefraDB` and
  `ClientLifecycle.validClientStep`; the model includes the state so future
  approval semantics have an explicit place to attach.
- **Rust references:** `state_machine_conformance::conformance_mapping_all_9_lifecycle_states_round_trip`
  and `lifecycle::tests::rust_request_lifecycle_state_vocabulary_matches_lean_model`.
- **Next product step if this changes:** add a first-class approval document,
  a Rust writer for `lifecycle_state = "inputRequired"`, and a conformance test
  proving the request remains non-terminal until input arrives or times out.

## Deviation 3: `dead` request state
- **Classification:** Intentional design choice
- **Rust boundary:** `dead` is now a real persisted state for stale pre-claim
  work: a pending request whose `valid_until` has passed is transitioned to
  `status = "dead"`, `lifecycle_state = "dead"`, `failure_reason = "Stale"`,
  and is never sent to a backend. Rust still represents post-claim provider
  failure and retry exhaustion as `failed`, not as a separate dead document.
- **Lean references:** `RequestState.toDefraDB`, `RequestContext.ttlOpen`,
  `RequestContext.claim_requires_ttl_open`, and
  `RequestContext.claim_with_ttl_bounds_time`.
- **Rust references:** `interruption_integration::offline_replay_of_stale_requests_does_not_call_backend`,
  `state_machine_conformance::pending_dead_stale_via_expire`, and
  `state_machine_conformance::conformance_mapping_all_9_lifecycle_states_round_trip`.
- **Boundary made precise:** the proof-level `dead` state is the terminal
  no-backend-progress sink for stale/expired work; Rust conformance currently
  commits to that state for stale pre-claim TTL expiry, while retry-exhausted
  inference failures remain ordinary `failed` terminal requests.
- **Product rationale:** stale pre-claim work never ran and can be marked dead
  without losing a provider outcome; post-claim provider failures did run and
  remain observable as failed attempts.

## Deviation 4: Explicit persisted `PersistenceState`
- **Classification:** External/operational boundary
- **Rust boundary:** durability is implemented by `StreamBuffer`,
  `DefraSessionHook`, and hook `FailurePolicy`, not by a per-token persisted
  `PersistenceState` document. The proof abstracts the storage commit boundary
  rather than claiming DefraDB stores `uncommitted/committing/committed/lost`
  rows.
- **Lean references:** `PersistenceState.Transition`, `PersistenceState.step_sound`,
  and `PersistenceState.transition_complete`.
- **Named assumption:** DefraDB mutations that return success are durable for
  the modeled stream/session writes; crash windows inside the storage engine or
  transport are outside the core request proof.
- **Next implementation step if this becomes product-visible:** persist an
  `AgentPersistenceEvent` or equivalent state row and add Rust/Lean vocabulary
  parity tests for it.

## Deviation 5: Deadline/TTL-bounded retries and claims
- **Classification:** Closed
- **Rust boundary:** claims check `valid_until` before acquiring work, stale
  rows become `dead/Stale`, and inference attempt start, retry backoff, stream
  start, stream item waits, and finalization are all bounded by the claimed
  request deadline.
- **Lean references:** `RequestContext.ttlOpen`,
  `RequestContext.claim_requires_ttl_open`,
  `RequestContext.claim_with_ttl_bounds_time`,
  `SessionState.reissue_source_deadline_open`, and
  `SessionState.reissue_latest_deadline_open`.
- **Rust references:** `interruption_integration::offline_replay_of_stale_requests_does_not_call_backend`,
  `agent::daemon::inference::tests::retry_backoff_wait_is_cut_off_by_request_deadline`,
  `await_with_request_deadline_bounds_waits`, and
  `request_deadline_remaining_reports_expired_deadline`.

## Deviation 6: Tool failure retryability
- **Classification:** Intentional design choice
- **Rust boundary:** `StreamingError::Tool(_)` is classified as
  `PermanentFailure` until tools expose enough health, idempotency, and
  side-effect metadata to distinguish retry-safe failures from permanent tool
  contract failures.
- **Lean references:** the request model leaves retry eligibility as transition
  preconditions (`retriesExhausted`, `deadlineExceeded`) and does not assign
  per-tool health semantics.
- **Rust references:** `classify_completion_error` and
  `error::tests::tool_streaming_errors_are_permanent_until_retry_metadata_exists`.
- **Product rationale:** retrying a tool call without idempotency metadata can
  repeat side effects, so the coarse permanent classification is intentional.

## Deviation 7: Fleet scheduler aggregate state persistence
- **Classification:** Model adjusted
- **Rust boundary:** `InferenceCall` rows are the persisted source of truth for
  call-level admission. A backend's held slot count is reconstructed from rows
  with `call_state = "running"`; there is intentionally no denormalized single
  `FleetState` document carrying the exact aggregate invariant.
- **Lean references:** `FleetState.slotAccountingInvariant` and
  `FleetState.slotAccountingInvariant_reconstructs_running`.
- **Rust references:** `admission::tests::queued_calls_start_in_tokio_registration_order_after_permit_release`,
  whose assertions reconstruct one held slot from a running row, exclude queued
  rows from slot ownership, and reconstruct zero held slots after terminal
  completion.
- **Boundary made precise:** the proof's aggregate `running` field is a derived
  view over active call contexts, not a persistence requirement.

## Deviation 8: Inference-call cancellation end-to-end coverage
- **Classification:** Closed
- **Lean references:** `ComposedState.interrupted_request_cancels_live_linked_call`.
- **Rust references:** unit tests cover pre-stream token cancellation,
  queued-call cancellation, and mid-stream permit-drop cancellation; integration
  tests drive a full `BehaviorDaemon` against a local mock stream and assert
  partial response preservation, linked
  `InferenceCall.call_state = "cancelled"`, and unrelated concurrent call
  isolation.
-/
