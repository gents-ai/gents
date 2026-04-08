import Proofs.Fleet

/-!
# Conformance Mapping: Rust Scheduler → Ideal Model

The intended ideal model is:

- interactive `AgentRequest` work enters the execution lifecycle at
  `claimed / waiting`
- scheduled work is materialized into the same lifecycle at
  `claimed / waiting`
- `BackendTracker.try_acquire` corresponds to `FleetState.CanAcquire` plus the
  `acquire_slot` transition
- inference start corresponds to `begin_execution`
- terminal completion/failure corresponds to `release_on_terminal`

The proof model treats `BackendId` as an opaque binding used for admission and
slot accounting. It does not model transport endpoint URLs or the service that
updates backend documents; those are external assumptions supplied to this
service via DefraDB.

Known deviations in the current Rust implementation:

1. aggregate scheduler counts remain process-local in `BackendTracker`, so the
   fleet-level `running` view used in `slotAccountingInvariant` is not directly
   inspectable from DefraDB state
2. backend health / availability facts are only as current as the backend
   documents observed at admission time; backend document freshness is an
   environmental assumption rather than a service-local proof obligation

Session-level retry/reissue is modeled compositionally in
`Proofs.SessionRecovery`: a failed latest request can spawn a fresh pending
request in the same session when retry budget remains, while preserving backend
binding and request history. The runtime now stages explicit
`retry_parent_request`, `retry_root_request`, and `superseded_by_request` writes,
and those fields now round-trip through DefraDB for DB-only debugging.
-/
