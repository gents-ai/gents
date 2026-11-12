import Proofs.Shared.Basic

namespace Proofs.MCPHealth

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

structure ServiceModel where
  state        : HealthState
  failureCount : Nat
  deriving DecidableEq, Repr

namespace ServiceModel

def initial : ServiceModel := { state := .healthy, failureCount := 0 }

end ServiceModel

/-- Events are completed observations. Cancelling an in-flight probe supplies no
event: it is neither failure evidence nor a complete registry observation.

External premise for `probeSuccess`: its staleness flag is the age of the node's
last successful observation of the service, never the age of the service's
configuration document. `ToolServiceRegistry.updated_at` is a create-time
identity stamp that no writer refreshes, so it carries no liveness and cannot
supply this flag. A service with no prior observation has no age and is fresh. -/
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
