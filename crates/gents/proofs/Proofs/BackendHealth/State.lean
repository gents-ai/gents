import Proofs.Basic

/-!
# Backend Health — Types

Per-backend, per-runtime state machine for the scheduled inference-backend
prober (#640). See
`docs/superpowers/specs/2026-07-07-backend-probe-health-640-design.md` (removed from the tree; see git history).

Measured health is observer-relative and lives in-memory on each runtime;
this model governs the local machine only. The shared `InferenceBackend`
document's `probe_status` is operator/bootstrap **intent** and enters the
model solely as the `intent` argument of `effectiveAvailable`.

Unlike `Proofs.MCPHealth` there is no staleness flavor, no backoff, and no
removal event: the machine is total over its two probe events.
-/

namespace Proofs.BackendHealth

/-- The four-state lifecycle.

    `unknown` — never probed by this runtime (startup, or newly enabled).
    `healthy` — last probe succeeded.
    `degraded` — `1 ≤ failureCount < K` consecutive failures; still routable.
    `unhealthy` — `failureCount ≥ K` consecutive failures; blocks routing. -/
inductive HealthState where
  | unknown
  | healthy
  | degraded
  | unhealthy
  deriving DecidableEq, Repr

namespace HealthState

def toDefraDB : HealthState → String
  | .unknown   => "unknown"
  | .healthy   => "healthy"
  | .degraded  => "degraded"
  | .unhealthy => "unhealthy"

def all : List HealthState :=
  [ .unknown, .healthy, .degraded, .unhealthy ]

theorem all_complete (s : HealthState) : s ∈ all := by
  cases s <;> simp [all]

/-- Routing veto: only a measured `.unhealthy` blocks routing. `.unknown`
    must NOT block — startup grace, where doc intent governs until the first
    probe cycle completes. `.degraded` (below-threshold failures) must NOT
    block — that is the hysteresis: a blip does not flap routing. -/
def blocksRouting : HealthState → Bool
  | .unhealthy => true
  | _          => false

end HealthState

/-- Per-backend measured model. `failureCount` counts consecutive probe
    failures since the last success; any success resets it to 0. -/
structure Model where
  state        : HealthState
  failureCount : Nat
  deriving DecidableEq, Repr

namespace Model

/-- Initial model — a backend this runtime has never probed. -/
def initial : Model := { state := .unknown, failureCount := 0 }

end Model

/-- The two events that drive transitions. `probeFail` folds connect failure,
    non-2xx, and timeout — operationally identical at the prober. -/
inductive Event where
  | probeSuccess
  | probeFail
  deriving DecidableEq, Repr

namespace Event

def toDefraDB : Event → String
  | .probeSuccess => "probeSuccess"
  | .probeFail    => "probeFail"

def all : List Event := [ .probeSuccess, .probeFail ]

theorem all_complete (e : Event) : e ∈ all := by
  cases e <;> simp [all]

end Event

/-- Effective availability of a backend on this runtime: operator/bootstrap
    intent (the shared document's `enabled && probe_status == "healthy"`)
    AND the local measurement not vetoing. This is the contract
    `BackendAdmissionConfig::is_available` mirrors in Rust. -/
def effectiveAvailable (intent : Bool) (m : Model) : Bool :=
  intent && !(m.state.blocksRouting)

end Proofs.BackendHealth
