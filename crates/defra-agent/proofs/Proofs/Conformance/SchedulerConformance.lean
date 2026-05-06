import Proofs.Fleet

/-!
# Conformance Mapping: Rust Scheduler → Ideal Model

The intended ideal model is:

- interactive `AgentRequest` work enters the execution lifecycle at `claimed`
- scheduled work is materialized into the same lifecycle at `claimed`
- call-level admission in `InferenceCall` corresponds to `FleetState.CanAcquire`
  plus the `acquire_slot` transition
- inference start corresponds to `begin_execution`
- terminal completion/failure corresponds to `release_on_terminal`

The proof model treats `BackendId` as an opaque binding used for admission and
slot accounting. It does not model transport endpoint URLs or the service that
updates backend documents; those are external assumptions supplied to this
service via DefraDB.

Implementation boundaries:

1. aggregate scheduler counts are reconstructed from `InferenceCall` rows; the
   fleet-level `running` view used in `slotAccountingInvariant` is a derived
   proof view, not a separate persisted `FleetState` document
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

namespace FleetState

/-- The aggregate `running` count in the proof is exactly the reconstructed
    count of active slot-holding work, not a second persisted Rust document. -/
theorem slotAccountingInvariant_reconstructs_running
    {s : FleetState}
    (h : s.slotAccountingInvariant)
    (bid : BackendId) :
    s.scheduler.running bid = s.slotCountFor bid :=
  h bid

end FleetState
