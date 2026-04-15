/-!
# Deviations: defra-agent vs Ideal Model

## Deviation 1: No `recovering` process state
- **Ideal:** ProcessState.recovering blocks claims during startup recovery
- **defra-agent:** recover_all() runs inline before main loop, no claim guard
- **Classification:** Bug — race condition possible if watcher starts early
- **Property violated:** S5 (recovery exclusivity)

## Deviation 2: No `inputRequired` request state
- **Ideal:** RequestState.inputRequired for tool approval / human-in-the-loop
- **defra-agent:** Tool execution blocks inline within processing state
- **Classification:** Missing feature — acceptable for autonomous tools

## Deviation 3: No `dead` request state
- **Ideal:** RequestState.dead for retries exhausted or unrecoverable deadline expiry
- **defra-agent:** Daemon persists `error`; clients derive `dead` from
  failed + retry_count + deadline and reissue a fresh request externally;
  the session-level reissue step itself is now modeled in
  `Proofs.SessionRecovery`
- **Classification:** Observability gap — terminal retry exhaustion is not
  explicit in a single persisted request document

## Deviation 4: No explicit `PersistenceState`
- **Ideal:** Tracks uncommitted/committing/committed/lost
- **defra-agent:** Implicit in StreamBuffer lifecycle and hook FailurePolicy
- **Classification:** Missing feature — tokens can be lost on crash
- **Property violated:** S6 weakened

## Deviation 5: Deadline does not bound retries
- **Ideal:** S4 requires totalRetryTime ≤ deadline - claimTime
- **defra-agent:** Retries bounded by count only, not time
- **Classification:** Bug
- **Property violated:** S4

## Deviation 6: Tool failures always classified as permanent
- **Ideal:** Service health should parameterize tool error retryability
- **defra-agent:** All tool errors → PermanentFailure
- **Classification:** Design choice

## Deviation 7: Fleet scheduler state persistence
- **Ideal:** FleetState exposes exact backend running counts and
  slot-accounting invariants alongside call-level admission state
- **defra-agent:** call-level admission is persisted through `InferenceCall`
- **Classification:** Resolved for backend HTTP-call admission
-/
