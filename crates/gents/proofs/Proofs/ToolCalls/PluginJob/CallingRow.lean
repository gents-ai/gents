import Proofs.ToolCalls.PluginJob.Admission

/-!
# The calling row of a step 0

What step 0 leaves on the job tool call's own row. A refused answer fails the
row in the foreground and commits nothing else. An admitted pinless answer is
a non-job call: its terminal, its execution receipt, its writes and the rows
its `control` ends commit together in the foreground and the row is never
pinned. An adopted row flips to the background with its pin and holds no
execution receipt until its terminal; a pinned step-0 result or suspend writes
that terminal in the adoption transaction.
-/

namespace PluginJob

structure CallingRow where
  state : RowState
  background : Bool
  pinned : Bool
  receipt : Bool
  deriving DecidableEq, Repr

def StepOutput.extras : StepOutput → Extras
  | .result x _ _ _ => x
  | .fail x => x
  | .suspend _ _ x _ _ => x
  | .effects e => e.extras

def settledState : StepOutput → RowState
  | .fail _ => .failed
  | _ => .completed

def callingRow (ctx : StepContext) (job : Job) (out : StepOutput) : CallingRow :=
  match admitStepGuarded ctx job out with
  | .error _ => { state := .failed, background := false, pinned := false, receipt := true }
  | .ok j =>
    match j.pin with
    | none => { state := settledState out, background := false, pinned := false, receipt := true }
    | some _ =>
      { state := j.state, background := true, pinned := true, receipt := j.state != .running }

def refusalCode (ctx : StepContext) (job : Job) (out : StepOutput) : Option String :=
  match admitStepGuarded ctx job out with
  | .error r => r.code
  | .ok _ => none

/-- The failure class a refused step 0 records on the calling row. -/
def Refusal.failureClass : Refusal → String
  | .notAvailable | .backgroundBudgetExceeded => "serviceUnavailable"
  | _ => "policyDenied"

def failureClass (ctx : StepContext) (job : Job) (out : StepOutput) : Option String :=
  match admitStepGuarded ctx job out with
  | .error r => some r.failureClass
  | .ok _ => none

def committedWrites (ctx : StepContext) (job : Job) (out : StepOutput) : Nat :=
  match admitStepGuarded ctx job out with
  | .error _ => 0
  | .ok _ => out.extras.writes

/-- The rows an admitted non-job answer ends; a refused answer ends none. -/
def controlKills (ctx : StepContext) (job : Job) (out : StepOutput) :
    List (DocId × ControlKind) :=
  match admitStepGuarded ctx job out, out with
  | .ok _, .result _ control _ _ => control
  | _, _ => []

def ControlKind.control : ControlKind → Control
  | .stop => .stop
  | .pause => .pause

/-- A job row after the controls of one answer that name it. -/
def controlled (kills : List (DocId × ControlKind)) (row : DocId) (job : Job)
    (obs : SpawnKey → Recovery.KillObservation) : Job × List (SpawnKey × Recovery.KillAction) :=
  kills.foldl (fun (acc : Job × List (SpawnKey × Recovery.KillAction)) (k : DocId × ControlKind) =>
    if k.1 = row then
      let killed := Control.kill acc.1 k.2.control obs
      (killed.1, acc.2 ++ killed.2)
    else acc) (job, [])

theorem refused_answer_fails_in_the_foreground (ctx : StepContext) (job : Job) (out : StepOutput)
    (r : Refusal) (h : admitStepGuarded ctx job out = .error r) :
    callingRow ctx job out = { state := .failed, background := false, pinned := false, receipt := true } ∧
      controlKills ctx job out = [] ∧ committedWrites ctx job out = 0 := by
  simp [callingRow, controlKills, committedWrites, h]

theorem admitted_pinless_answer_is_never_adopted (ctx : StepContext) (job : Job) (out : StepOutput)
    (j : Job) (h : admitStepGuarded ctx job out = .ok j) (hp : j.pin = none) :
    (callingRow ctx job out).background = false ∧ (callingRow ctx job out).pinned = false ∧
      (callingRow ctx job out).receipt = true := by
  simp [callingRow, h, hp]

end PluginJob
