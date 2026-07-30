import Proofs.Basic

namespace Proofs.BackendHealth

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

def blocksRouting : HealthState → Bool
  | .unhealthy => true
  | _          => false

end HealthState

structure Model where
  state        : HealthState
  failureCount : Nat
  deriving DecidableEq, Repr

namespace Model

def initial : Model := { state := .unknown, failureCount := 0 }

end Model

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

def effectiveAvailable (intent : Bool) (m : Model) : Bool :=
  intent && !(m.state.blocksRouting)

end Proofs.BackendHealth
