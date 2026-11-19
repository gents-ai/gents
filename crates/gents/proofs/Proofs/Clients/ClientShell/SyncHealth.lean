import Proofs.Clients.ClientShell.ObservationOrdering

namespace ClientSyncHealth
open RuntimeConnectionObservation (Status)

/-- Only the authenticated runtime observations determine connection health.
Local journal state and transport discovery supply no access or readiness. -/
structure Facts where
  runtimes : List (Option Status)
  deriving DecidableEq, Repr

inductive Cause where
  | runtimeConnecting | runtimeDisconnected | runtimeRefused | runtimeNoEndpoint
  deriving DecidableEq, Repr

inductive NextAction where
  | wait | restoreAccess | connectRuntime
  deriving DecidableEq, Repr

inductive State where
  | healthy | waiting | blocked
  deriving DecidableEq, Repr

structure View where
  state : State
  cause : Option Cause
  nextAction : Option NextAction
  deriving DecidableEq, Repr

def Cause.nextAction : Cause → NextAction
  | .runtimeConnecting | .runtimeDisconnected => .wait
  | .runtimeRefused => .restoreAccess
  | .runtimeNoEndpoint => .connectRuntime

def Cause.state (cause : Cause) : State :=
  if cause.nextAction = .wait then .waiting else .blocked

def runtimeCause : Option Status → Option Cause
  | none | some .connecting => some .runtimeConnecting
  | some .connected => none
  | some .disconnected => some .runtimeDisconnected
  | some .refused => some .runtimeRefused
  | some .noEndpoint => some .runtimeNoEndpoint

def causes (facts : Facts) : List Cause := facts.runtimes.filterMap runtimeCause

def project (facts : Facts) : Option Cause :=
  match (causes facts).find? (fun cause => cause.state == .blocked) with
  | some cause => some cause | none => (causes facts).head?

def view (facts : Facts) : View :=
  match project facts with
  | none => ⟨.healthy, none, none⟩
  | some cause => ⟨cause.state, some cause, some cause.nextAction⟩

def runtimeCount (facts : Facts) : Nat := facts.runtimes.length

def connectedRuntimeCount (facts : Facts) : Nat :=
  (facts.runtimes.filter (· == some .connected)).length

def allFacts : List Facts :=
  let statuses := [none, some .connecting, some .connected, some .disconnected, some .refused, some .noEndpoint]
  ⟨[]⟩ :: statuses.map (fun status => ⟨[status]⟩) ++
    statuses.flatMap fun first => statuses.map fun second => ⟨[first, second]⟩

/-- Every displayed recovery action comes from a witnessed runtime failure. -/
theorem unhealthy_view_names_cause_and_action (facts : Facts) :
    (view facts).state ≠ .healthy → (view facts).cause.isSome ∧ (view facts).nextAction.isSome := by
  unfold view
  cases project facts <;> simp

end ClientSyncHealth
