import Proofs.Automation.GoalAutomation.ReadinessGate
import Proofs.Conformance.Family

namespace Conformance.GoalClaimedReadiness
open GoalAutomation.ReadinessGate Lean

structure Case where
  name : String
  observation : Observation
  settled : Bool
  childExists : Bool := false
  before : ClaimedRecovery

private def active : ClaimedRecovery :=
  { goal := ⟨.active, 0, false, false⟩, retries := 1,
    lastFailure := some "failed", sequence := 1, parent := some "claimed-parent" }

private def wrapup : ClaimedRecovery :=
  { active with goal := ⟨.budgetLimited, 0, true, false⟩ }

def cases : List Case :=
  [ ⟨"claimed_ready_materializes_retry", .ready true, true, false, active⟩
  , ⟨"claimed_stale_ready_still_materializes", .ready false, true, false, active⟩
  , ⟨"claimed_backend_recovering_waits", .backendRecovering, true, false, active⟩
  , ⟨"claimed_unknown_waits", .unknown, true, false, active⟩
  , ⟨"claimed_unsettled_invalid_waits", .unavailable, false, false, active⟩
  , ⟨"claimed_settled_invalid_pauses", .unavailable, true, false, active⟩
  , ⟨"claimed_settled_unassigned_pauses", .unassigned, true, false, active⟩
  , ⟨"claimed_wrapup_unavailable_abandons", .unavailable, true, false, wrapup⟩
  , ⟨"claimed_wrapup_recovering_waits", .backendRecovering, true, false, wrapup⟩
  , ⟨"claimed_child_present_is_unchanged", .unavailable, true, true, active⟩
  , ⟨"claimed_abandoned_wrapup_stays_inactive", .ready true, true, false,
      { wrapup with goal := { wrapup.goal with wrapupCompleted := true } }⟩
  , ⟨"claimed_paused_goal_stays_inactive", .ready true, true, false,
      { active with goal := { active.goal with status := .paused } }⟩ ]

inductive ObservationKind where
  | ready
  | backendRecovering
  | unavailable
  | unassigned
  | unknown
  deriving ToJson

def ObservationKind.ofModel : Observation → ObservationKind
  | .ready _ => .ready
  | .backendRecovering => .backendRecovering
  | .unavailable => .unavailable
  | .unassigned => .unassigned
  | .unknown => .unknown

def ObservationKind.toModel (newerThanTerminal : Bool) : ObservationKind → Observation
  | .ready => .ready newerThanTerminal
  | .backendRecovering => .backendRecovering
  | .unavailable => .unavailable
  | .unassigned => .unassigned
  | .unknown => .unknown

deriving instance ToJson for ClaimedDecision

structure RecoveryWire where
  status : String
  blockedAudits : Nat
  wrapupRequested : Bool
  wrapupCompleted : Bool
  infrastructureRetries : Nat
  lastFailure : Option String
  continuationSequence : Nat
  parent : Option String
  deriving ToJson

def RecoveryWire.ofModel (state : ClaimedRecovery) : RecoveryWire :=
  { status := state.goal.status.toDefraDB, blockedAudits := state.goal.blockedAudits
    wrapupRequested := state.goal.wrapupRequested, wrapupCompleted := state.goal.wrapupCompleted
    infrastructureRetries := state.retries, lastFailure := state.lastFailure
    continuationSequence := state.sequence, parent := state.parent }

def RecoveryWire.toModel (state : RecoveryWire) : ClaimedRecovery :=
  { goal := ⟨(Goals.Status.fromDefraDB? state.status).getD .active, state.blockedAudits,
      state.wrapupRequested, state.wrapupCompleted⟩
    retries := state.infrastructureRetries, lastFailure := state.lastFailure
    sequence := state.continuationSequence, parent := state.parent }

structure ClaimedInput where
  name : String
  observation : ObservationKind
  newerThanTerminal : Bool
  settled : Bool
  childExists : Bool
  before : RecoveryWire
  deriving ToJson

structure ClaimedOutput where
  decision : ClaimedDecision
  expected : Option RecoveryWire
  deriving ToJson

@[conformance_family]
def claimed : Family ClaimedInput ClaimedOutput where
  inputs := cases.map fun c =>
    { name := c.name, observation := .ofModel c.observation
      newerThanTerminal := c.observation.newerThanTerminal, settled := c.settled
      childExists := c.childExists, before := .ofModel c.before }
  model := fun i =>
    let observation := i.observation.toModel i.newerThanTerminal
    let before := i.before.toModel
    { decision := claimedGate observation i.settled i.childExists before.goal
      expected := (claimedStep observation i.settled i.childExists before).map RecoveryWire.ofModel }

theorem all_cases_have_legal_owned_outcomes :
    cases.all (fun c => (claimedStep c.observation c.settled c.childExists c.before).isSome) = true := by
  native_decide

theorem settled_unavailable_cases_end_automatic_continuation :
    (claimedStep .unavailable true false active).map (·.goal.status) = some .paused ∧
    (claimedStep .unavailable true false wrapup).map (·.goal.wrapupCompleted) = some true := by
  native_decide

end Conformance.GoalClaimedReadiness
