import Proofs.GoalAutomation.ReadinessGate
import Lean

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

private def observationName : Observation → String
  | .ready _ => "ready"
  | .backendRecovering => "backend_recovering"
  | .unavailable => "unavailable"
  | .unassigned => "unassigned"
  | .unknown => "unknown"

private def decisionName : ClaimedDecision → String
  | .materialize => "materialize"
  | .awaitReadiness => "await_readiness"
  | .stop => "stop"
  | .inactive => "inactive"

private def optionalString : Option String → Json
  | none => .null
  | some value => .str value

private def recoveryJson (state : ClaimedRecovery) : Json := Json.mkObj
  [ ("status", .str state.goal.status.toDefraDB)
  , ("blocked_audits", toJson state.goal.blockedAudits)
  , ("wrapup_requested", toJson state.goal.wrapupRequested)
  , ("wrapup_completed", toJson state.goal.wrapupCompleted)
  , ("infrastructure_retries", toJson state.retries)
  , ("last_failure", optionalString state.lastFailure)
  , ("continuation_sequence", toJson state.sequence)
  , ("parent", optionalString state.parent) ]

private def caseJson (c : Case) : Json := Json.mkObj
  [ ("name", .str c.name)
  , ("observation", .str (observationName c.observation))
  , ("newer_than_terminal", toJson c.observation.newerThanTerminal)
  , ("settled", toJson c.settled)
  , ("child_exists", toJson c.childExists)
  , ("before", recoveryJson c.before)
  , ("decision", .str (decisionName (claimedGate c.observation c.settled c.childExists c.before.goal)))
  , ("expected", match claimedStep c.observation c.settled c.childExists c.before with
      | none => .null
      | some after => recoveryJson after) ]

def casesJson : String := (toJson (cases.map caseJson)).compress

theorem all_cases_have_legal_owned_outcomes :
    cases.all (fun c => (claimedStep c.observation c.settled c.childExists c.before).isSome) = true := by
  native_decide

theorem settled_unavailable_cases_end_automatic_continuation :
    (claimedStep .unavailable true false active).map (·.goal.status) = some .paused ∧
    (claimedStep .unavailable true false wrapup).map (·.goal.wrapupCompleted) = some true := by
  native_decide

end Conformance.GoalClaimedReadiness
