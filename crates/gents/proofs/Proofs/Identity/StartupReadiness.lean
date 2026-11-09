import Lean

namespace Identity.StartupReadiness
open Lean

inductive Join where
  | pending | clean | failed (message : String)
  deriving Repr

inductive Outcome where
  | wait | awaitJoin | ready | timeout | premature | failed (message : String)
  deriving BEq, DecidableEq, Repr

/-- A runtime termination observed alongside readiness wins. A shutting-down
runtime is joined even if the readiness deadline expires while cleanup drains;
the existing runtime owner determines that join's cleanup deadline. -/
def observe (join : Join) (shuttingDown readiness deadline : Bool) : Outcome :=
  match join with
  | .failed message => .failed message
  | .clean => .premature
  | .pending =>
      if shuttingDown then .awaitJoin
      else if readiness then .ready
      else if deadline then .timeout
      else .wait

theorem failure_wins_readiness (message : String) (shutting ready deadline : Bool) :
    observe (.failed message) shutting ready deadline = .failed message := by rfl

theorem clean_exit_is_premature (shutting ready deadline : Bool) :
    observe .clean shutting ready deadline = .premature := by rfl

theorem shutdown_waits_for_join (ready deadline : Bool) :
    observe .pending true ready deadline = .awaitJoin := by rfl

structure Case where
  name : String
  join : Join
  shuttingDown : Bool
  readiness : Bool
  deadline : Bool

def cases : List Case :=
  [ ⟨"waiting", .pending, false, false, false⟩
  , ⟨"ready", .pending, false, true, false⟩
  , ⟨"deadline", .pending, false, false, true⟩
  , ⟨"ready-before-deadline", .pending, false, true, true⟩
  , ⟨"failed-before-ready", .failed "original error", false, false, false⟩
  , ⟨"failed-with-ready", .failed "original error", false, true, false⟩
  , ⟨"clean-with-ready", .clean, false, true, false⟩
  , ⟨"shutting-before-deadline", .pending, true, false, false⟩
  , ⟨"shutting-at-deadline", .pending, true, false, true⟩
  , ⟨"shutting-with-ready", .pending, true, true, false⟩ ]

def joinJson : Join → Json
  | .pending => Json.mkObj [("kind", toJson "pending")]
  | .clean => Json.mkObj [("kind", toJson "clean")]
  | .failed message => Json.mkObj [("kind", toJson "failed"), ("message", toJson message)]

def outcomeJson : Outcome → Json
  | .wait => Json.mkObj [("kind", toJson "wait")]
  | .awaitJoin => Json.mkObj [("kind", toJson "await_join")]
  | .ready => Json.mkObj [("kind", toJson "ready")]
  | .timeout => Json.mkObj [("kind", toJson "timeout")]
  | .premature => Json.mkObj [("kind", toJson "premature")]
  | .failed message => Json.mkObj [("kind", toJson "failed"), ("message", toJson message)]

def caseJson (c : Case) : Json := Json.mkObj
  [("name", toJson c.name), ("join", joinJson c.join),
   ("shutting_down", toJson c.shuttingDown), ("readiness", toJson c.readiness),
   ("deadline", toJson c.deadline),
   ("expected", outcomeJson (observe c.join c.shuttingDown c.readiness c.deadline))]

def casesJson : String := (toJson (cases.map caseJson)).compress

end Identity.StartupReadiness
