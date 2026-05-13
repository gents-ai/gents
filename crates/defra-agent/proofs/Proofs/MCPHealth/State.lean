import Proofs.Basic

/-!
# MCP Health / Eviction — Types

Per-service state machine for the MCP connection-pool health checker. See
`docs/superpowers/specs/2026-05-13-mcp-health-lean-design.md` for the design.

`HealthState` is the four-state lifecycle from #186. `ServiceModel` carries the
state plus a `failureCount` that distinguishes the two semantic flavors of
`Degraded` (see doc comment). `Event` is the named-event vocabulary that
drives transitions; no async tick is modeled.
-/

namespace Proofs.MCPHealth

/-- The four-state lifecycle.

    `healthy` — last probe succeeded with fresh heartbeat.
    `degraded` — last probe succeeded but heartbeat is stale (`failureCount = 0`),
                 or saw `failureCount ≥ 1` consecutive failures with
                 `failureCount < K` (only under K ≥ 2).
    `evicted` — pool connection has been removed; no calls admitted.
    `reconnecting` — backoff expired after eviction; awaiting next probe.
                     Unreachable at K=1 / no backoff. -/
inductive HealthState where
  | healthy
  | degraded
  | evicted
  | reconnecting
  deriving DecidableEq, Repr

namespace HealthState

def toDefraDB : HealthState → String
  | .healthy      => "healthy"
  | .degraded     => "degraded"
  | .evicted      => "evicted"
  | .reconnecting => "reconnecting"

def all : List HealthState :=
  [ .healthy, .degraded, .evicted, .reconnecting ]

theorem all_complete (s : HealthState) : s ∈ all := by
  cases s <;> simp [all]

end HealthState

/-- Per-service state model.

    `failureCount` is the count of consecutive `probeFail` events since the
    last `probeSuccess`. It is reset to 0 by any `probeSuccess` regardless of
    the `staleness` flag.

    The single `degraded` constructor has two semantic flavors distinguished
    by `failureCount`:

    * `failureCount = 0`: staleness-degraded (entered via
      `probeSuccess(staleness = true)`). Equivalent to today's `Stale`
      `HealthStatus`.
    * `failureCount ≥ 1`: failure-count-degraded (entered via `probeFail`
      when `failureCount + 1 < K`). Only reachable under K ≥ 2; unreachable
      under K=1 because `failureCount + 1 ≥ 1 = K` always evicts immediately.

    Both flavors share `healthProjection .degraded = .stale` and therefore
    the same preflight dispatch decision. -/
structure ServiceModel where
  state        : HealthState
  failureCount : Nat
  deriving DecidableEq, Repr

namespace ServiceModel

/-- Initial model — used when the pool first observes a service. -/
def initial : ServiceModel := { state := .healthy, failureCount := 0 }

end ServiceModel

/-- The four events that drive transitions.

    `probeSuccess` carries a `staleness : Bool` flag derived from the
    heartbeat age at probe time (mirrors `health_checker.rs:247`).

    `probeFail` folds both probe error and probe timeout — operationally
    identical in `health_checker.rs:268,:289` (both call `mcp_pool.remove`
    and set `Unreachable`).

    `backoffExpiry` is a no-op outside `.evicted`.

    `registryAbsent` removes the service from the model entirely
    (`step? sm .registryAbsent K = none`). -/
inductive Event where
  | probeSuccess (staleness : Bool)
  | probeFail
  | backoffExpiry
  | registryAbsent
  deriving DecidableEq, Repr

namespace Event

def toDefraDB : Event → String
  | .probeSuccess false => "probeSuccessFresh"
  | .probeSuccess true  => "probeSuccessStale"
  | .probeFail          => "probeFail"
  | .backoffExpiry      => "backoffExpiry"
  | .registryAbsent     => "registryAbsent"

def all : List Event :=
  [ .probeSuccess false, .probeSuccess true
  , .probeFail, .backoffExpiry, .registryAbsent ]

theorem all_complete (e : Event) : e ∈ all := by
  cases e
  · rename_i b; cases b <;> simp [all]
  all_goals simp [all]

end Event

end Proofs.MCPHealth
