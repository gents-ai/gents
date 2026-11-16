import Proofs.Shared.Basic
import Proofs.Automation.RuntimeReconcile.State

namespace Proofs.BackendHealth

inductive HealthState where
  | unknown
  | healthy
  | degraded
  | unhealthy
  | credentialsRequired
  | subscriptionNotEntitled
  deriving DecidableEq, Repr

namespace HealthState

def toDefraDB : HealthState → String
  | .unknown   => "unknown"
  | .healthy   => "healthy"
  | .degraded  => "degraded"
  | .unhealthy => "unhealthy"
  | .credentialsRequired => "credentials_required"
  | .subscriptionNotEntitled => "subscription_not_entitled"

def all : List HealthState :=
  [ .unknown, .healthy, .degraded, .unhealthy, .credentialsRequired, .subscriptionNotEntitled ]

theorem all_complete (s : HealthState) : s ∈ all := by
  cases s <;> simp [all]

def blocksRouting : HealthState → Bool
  | .unhealthy | .credentialsRequired | .subscriptionNotEntitled => true
  | _          => false

/-- A rejected refresh credential or account eligibility decision needs account
recovery; an intervening network error does not establish that it was repaired. -/
def needsAccountRecovery : HealthState → Bool
  | .credentialsRequired | .subscriptionNotEntitled => true
  | _ => false

def readinessReason : HealthState → Option RuntimeState.AgentUnavailableReason
  | .credentialsRequired => some .credentialsRequired
  | .subscriptionNotEntitled => some .subscriptionNotEntitled
  | _ => none

end HealthState

structure Model where
  state        : HealthState
  failureCount : Nat
  deriving DecidableEq, Repr

namespace Model

def initial : Model := { state := .unknown, failureCount := 0 }

end Model

/-- Events are completed observations. Cancelling an in-flight probe supplies no
event: it is neither failure evidence nor a complete registry observation. -/
inductive Event where
  | probeSuccess
  | probeFail
  | credentialsRejected
  | subscriptionNotEntitled
  | configurationChanged
  deriving DecidableEq, Repr

namespace Event

def toDefraDB : Event → String
  | .probeSuccess => "probeSuccess"
  | .probeFail    => "probeFail"
  | .credentialsRejected => "credentialsRejected"
  | .subscriptionNotEntitled => "subscriptionNotEntitled"
  | .configurationChanged => "configurationChanged"

def all : List Event :=
  [ .probeSuccess, .probeFail, .credentialsRejected, .subscriptionNotEntitled, .configurationChanged ]

theorem all_complete (e : Event) : e ∈ all := by
  cases e <;> simp [all]

end Event

def effectiveAvailable (intent : Bool) (m : Model) : Bool :=
  intent && !(m.state.blocksRouting)

/-- An enabled backend without a completed probe holds requests without granting
provider admission. A local health veto prevents indefinite first-probe waiting. -/
def awaitingFirstProbe (enabled : Bool) (probeStatus : String)
    (measuredUnhealthy : Bool) : Bool :=
  enabled && probeStatus == HealthState.unknown.toDefraDB && !measuredUnhealthy

/-- Startup failure remains terminal even while configuration awaits its first probe. -/
def awaitProbeRequest (awaitingProbe startupFailed : Bool) : Bool :=
  awaitingProbe && !startupFailed

theorem disabled_never_awaits (status : String) (unhealthy : Bool) :
    awaitingFirstProbe false status unhealthy = false := by
  simp [awaitingFirstProbe]

theorem startup_failure_never_awaits (waiting : Bool) :
    awaitProbeRequest waiting true = false := by
  simp [awaitProbeRequest]

end Proofs.BackendHealth
