/-! Settling a provider tool call whose execution has become ready. The call
re-reads its wall deadline at the moment it is ready: a deadline that has passed
settles as timed out whatever the call produced, and otherwise the call settles
by its receipt failure, then its early failure, then completion. -/

namespace ToolDispatch

inductive Settlement where
  | timedOut
  | receiptFailure
  | earlyFailure
  | completed
  deriving DecidableEq, Repr

def settleReady (expired receiptFailed earlyFailure : Bool) : Settlement :=
  if expired then .timedOut
  else if receiptFailed then .receiptFailure
  else if earlyFailure then .earlyFailure
  else .completed

theorem a_ready_call_past_its_deadline_is_timed_out (receiptFailed earlyFailure : Bool) :
    settleReady true receiptFailed earlyFailure = .timedOut := rfl

theorem timed_out_exactly_when_the_deadline_has_passed (expired receiptFailed earlyFailure : Bool) :
    settleReady expired receiptFailed earlyFailure = .timedOut ↔ expired = true := by
  cases expired <;> cases receiptFailed <;> cases earlyFailure <;> decide

theorem completed_only_without_a_deadline_or_failure (expired receiptFailed earlyFailure : Bool)
    (h : settleReady expired receiptFailed earlyFailure = .completed) :
    expired = false ∧ receiptFailed = false ∧ earlyFailure = false := by
  cases expired <;> cases receiptFailed <;> cases earlyFailure <;>
    first | exact ⟨rfl, rfl, rfl⟩ | simp [settleReady] at h

/-- What a dispatch under a wall deadline shows: whether the tool was started
and how the call settled. A deadline already past at dispatch settles the call
without starting it; a started call settles by `settleReady` when it becomes
ready, so a call that becomes ready past the deadline is timed out. -/
structure DeadlineDispatch where
  called : Bool
  settlement : Settlement
  deriving DecidableEq, Repr

def dispatchUnderDeadline (expiredAtDispatch readyAfterDeadline fails : Bool) : DeadlineDispatch :=
  if expiredAtDispatch then { called := false, settlement := .timedOut }
  else { called := true, settlement := settleReady readyAfterDeadline false fails }

theorem ready_tool_after_wall_deadline_is_timed_out :
    dispatchUnderDeadline false true false = { called := true, settlement := .timedOut } := rfl

theorem a_call_past_its_deadline_at_dispatch_is_never_started (readyAfterDeadline fails : Bool) :
    dispatchUnderDeadline true readyAfterDeadline fails =
      { called := false, settlement := .timedOut } := rfl

end ToolDispatch
