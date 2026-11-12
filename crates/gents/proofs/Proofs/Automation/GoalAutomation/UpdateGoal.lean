import Proofs.Automation.GoalAutomation.Edit

namespace GoalAutomation.UpdateGoal

/-- The trimmed `status` argument of `update_goal`. -/
inductive RequestedStatus where
  | omitted
  | active
  | complete
  | blocked
  | unsupported
  deriving DecidableEq, Repr

/-- The `reason` argument: absent, present but blank after trimming, or given. -/
inductive ReasonArgument where
  | absent
  | blank
  | given
  deriving DecidableEq, Repr

/-- What `update_goal` does with a call from the Goal's current assigned
caller. Only `edit`, `complete` and `blockedAudit` write; every other route
answers without changing the Goal. -/
inductive Route where
  | edit
  | reasonOutsideCompletion
  | notActiveResumable
  | notActiveFinal
  | fieldsOutsideActive
  | complete
  | completeIllegal
  | blockedAudit
  | blockedNotActive
  | blockedNeedsReason
  | unsupported
  deriving DecidableEq, Repr

def Route.writes : Route → Bool
  | .edit | .complete | .blockedAudit => true
  | _ => false

def observed (status : Goals.Status) : Goals.State := ⟨status, 0, false, false⟩

/-- Decision: an omitted status is an edit request, so an agent edits
without restating `active`. A reason accompanies only completion and a
blocked audit; on an edit it is refused rather than dropped. An agent edits
only an active Goal (`Edit.editable .agent`): any other status is refused
and the answer says whether an operator resume can make it active again
(the statuses `Goals.step? .resume` accepts) or the Goal is final. Objective
and budget edits never ride on a completion or a blocked audit. A blocked
audit is legal only from active (`Goals.step? (.blockedAudit _)`), so an
agent cannot turn a budget-limited Goal in wrap-up back into a blocked,
resumable one, and it names the repeated condition with a non-blank reason. -/
def route (requested : RequestedStatus) (fields : Bool) (reason : ReasonArgument)
    (status : Goals.Status) : Route :=
  match requested with
  | .omitted | .active =>
      match reason with
      | .blank | .given => .reasonOutsideCompletion
      | .absent =>
          if Edit.editable .agent status then .edit
          else if (Goals.step? (observed status) .resume).isSome then .notActiveResumable
          else .notActiveFinal
  | .complete =>
      if fields then .fieldsOutsideActive
      else if (Goals.step? (observed status) .complete).isSome then .complete
      else .completeIllegal
  | .blocked =>
      if fields then .fieldsOutsideActive
      else if (Goals.step? (observed status) (.blockedAudit .newCondition)).isNone then
        .blockedNotActive
      else match reason with
        | .given => .blockedAudit
        | .absent | .blank => .blockedNeedsReason
  | .unsupported => .unsupported

theorem edit_needs_an_active_goal (requested : RequestedStatus) (fields : Bool)
    (reason : ReasonArgument) (status : Goals.Status)
    (h : route requested fields reason status = .edit) : status = .active := by
  cases requested <;> cases reason <;> cases status <;> cases fields <;>
    simp_all [route, Edit.editable, Goals.step?, observed]

theorem blocked_audit_needs_an_active_goal (fields : Bool) (reason : ReasonArgument)
    (status : Goals.Status) (h : route .blocked fields reason status = .blockedAudit) :
    status = .active := by
  cases reason <;> cases status <;> cases fields <;>
    simp_all [route, Goals.step?, observed]

theorem budget_limited_goal_is_never_rewritten_by_its_agent
    (requested : RequestedStatus) (fields : Bool) (reason : ReasonArgument)
    (h : route requested fields reason .budgetLimited ≠ .complete) :
    (route requested fields reason .budgetLimited).writes = false := by
  cases requested <;> cases reason <;> cases fields <;>
    simp_all [route, Route.writes, Edit.editable, Goals.step?, observed]

end GoalAutomation.UpdateGoal
