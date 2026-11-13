import Proofs.Automation.GoalAutomation.SessionObservation
import Proofs.Conformance.GoalRequestAssignment
import Proofs.Conformance.Family
import Lean

namespace Conformance.GoalSessionObservationContracts

open GoalAutomation.SessionObservation GoalAutomation.RequestAssignment GoalAutomation Lean

deriving instance ToJson for GoalAutomation.SessionObservation.SessionRead,
  GoalAutomation.SessionObservation.UnknownCause, GoalAutomation.SessionObservation.NextAction

structure ProjectionWire where
  kind : String
  goal : Option Reference
  deriving ToJson, DecidableEq

def ProjectionWire.ofModel : ProjectionRead → ProjectionWire
  | .missing => ⟨"missing", none⟩
  | .unverified => ⟨"unverified", none⟩
  | .verified goal => ⟨"verified", goal⟩

def ProjectionWire.toModel (p : ProjectionWire) : ProjectionRead :=
  if p.kind == "verified" then .verified p.goal
  else if p.kind == "unverified" then .unverified else .missing

structure Case where
  session : SessionRead
  projection : ProjectionWire
  captured : Option Reference
  deriving ToJson

def projections : List ProjectionRead :=
  [.missing, .unverified, .verified none, .verified (some ⟨1, none⟩),
    .verified (some ⟨2, some 200⟩)]

def cases : List Case :=
  [SessionRead.absent, .present].flatMap fun session =>
    projections.flatMap fun projection =>
      [none, some ⟨1, some 100⟩].map fun captured =>
        ⟨session, ProjectionWire.ofModel projection, captured⟩

example : cases.all (fun c => ProjectionWire.ofModel c.projection.toModel == c.projection) := by
  native_decide

def Case.state (c : Case) : State := observe c.session c.projection.toModel

structure StateWire where
  kind : String
  cause : Option UnknownCause
  goal : Option Reference
  deriving ToJson

def StateWire.ofModel : State → StateWire
  | .noReadableSession => ⟨"no_readable_session", none, none⟩
  | .unknown cause => ⟨"unknown", some cause, none⟩
  | .observed goal => ⟨"observed", none, goal⟩

structure PreparedWire where
  kind : String
  reference : Option Reference
  deriving ToJson

def PreparedWire.ofModel : PreparedInput → PreparedWire
  | .pending => ⟨"pending", none⟩
  | .prepared reference => ⟨"prepared", reference⟩

structure ClassificationOutput where
  state : StateWire
  next : NextAction
  bind : PreparedWire
  refusal : Option UnknownCause
  deriving ToJson

@[conformance_family]
def classification : Family Case ClassificationOutput where
  inputs := cases
  model := fun c =>
    { state := StateWire.ofModel c.state, next := next c.state
      bind := PreparedWire.ofModel (bind c.captured c.state)
      refusal := refusal c.captured c.state }

structure ViewWire where
  kind : String
  goal : Option Reference
  cause : Option String
  deriving ToJson

def ViewWire.ofModel : View → ViewWire
  | .goal goal => ⟨"goal", some goal, none⟩
  | .noGoal => ⟨"no_goal", none, none⟩
  | .unavailable .sessionNotReadable => ⟨"unavailable", none, some "session_not_readable"⟩
  | .unavailable (.unknown .projectionMissing) => ⟨"unavailable", none, some "projection_missing"⟩
  | .unavailable (.unknown .projectionUnverified) =>
      ⟨"unavailable", none, some "projection_unverified"⟩

structure ViewOutput where
  view : ViewWire
  next : NextAction
  deriving ToJson

@[conformance_family]
def presentation : Family Case ViewOutput where
  inputs := cases
  model := fun c => { view := ViewWire.ofModel (view c.state), next := next c.state }

def referenceJson (reference : Reference) : Json :=
  Json.mkObj [("goal", toJson reference.goal), ("root", toJson reference.root)]

def unknownCauseJson : UnknownCause → Json
  | .projectionMissing => "projection_missing"
  | .projectionUnverified => "projection_unverified"

def stateJson : State → Json
  | .noReadableSession => Json.mkObj [("kind", "no_readable_session")]
  | .unknown cause => Json.mkObj [("kind", "unknown"), ("cause", unknownCauseJson cause)]
  | .observed goal => Json.mkObj [("kind", "observed"),
      ("goal", goal.map referenceJson |>.getD Json.null)]

def viewJson : View → Json
  | .goal goal => Json.mkObj [("kind", "goal"), ("goal", referenceJson goal)]
  | .noGoal => Json.mkObj [("kind", "no_goal")]
  | .unavailable .sessionNotReadable =>
      Json.mkObj [("kind", "unavailable"), ("cause", "session_not_readable")]
  | .unavailable (.unknown cause) =>
      Json.mkObj [("kind", "unavailable"), ("cause", unknownCauseJson cause)]

def nextJson : NextAction → Json
  | .controlGoal => "control_goal" | .startGoal => "start_goal"
  | .send => "send" | .awaitPublication => "await_publication"

def caseJson (c : Case) : Json :=
  (toJson c).mergeObj (Json.mkObj [("expected_state", stateJson c.state),
    ("expected_view", viewJson (view c.state)), ("expected_next", nextJson (next c.state))])

def casesJson : String := (toJson (cases.map caseJson)).compress

/-! Histories of reads the client folds (`SessionObservation.fold`). Session 1
is the one shown; session 2 is another chat. -/

def revisionA : ClientControl.Revision := ⟨"goal-1", "cid-a"⟩
def revisionB : ClientControl.Revision := ⟨"goal-1", "cid-b"⟩

def outcomes : List ReadOutcome :=
  [.completed .present (.verified (some ⟨1, none⟩)) (some revisionA),
    .completed .present (.verified (some ⟨1, none⟩)) (some revisionB),
    .completed .present (.verified none) none,
    .completed .present .missing none,
    .completed .absent .missing none,
    .incomplete, .refused]

def reads (sessions : List Nat) : List Read :=
  sessions.flatMap fun session => outcomes.map fun outcome => ⟨session, outcome⟩

def histories : List (List Read) :=
  let both := reads [1, 2]
  let shown := reads [1]
  [[]] ++ both.map (fun r => [r]) ++ both.flatMap (fun a => both.map fun b => [a, b]) ++
    shown.flatMap (fun a => shown.flatMap fun b => shown.map fun c => [a, b, c])

def revisionJson (revision : ClientControl.Revision) : Json :=
  Json.mkObj [("document", toJson revision.document), ("composite", toJson revision.composite)]

def outcomeJson : ReadOutcome → Json
  | .completed session projection revision => Json.mkObj [("kind", "completed"),
      ("state", stateJson (observe session projection)),
      ("next", nextJson (next (observe session projection))),
      ("revision", revision.map revisionJson |>.getD Json.null)]
  | .incomplete => Json.mkObj [("kind", "incomplete")]
  | .refused => Json.mkObj [("kind", "refused")]

def readJson (read : Read) : Json :=
  Json.mkObj [("session", toJson read.session), ("outcome", outcomeJson read.outcome)]

def shownJson : Shown → Json
  | .pending => Json.mkObj [("kind", "pending")]
  | .unread => Json.mkObj [("kind", "unread")]
  | .current v => Json.mkObj [("kind", "current"), ("view", viewJson v)]
  | .stale v => Json.mkObj [("kind", "stale"), ("view", viewJson v)]

def offeredJson : Option (NextAction × Option ClientControl.Revision) → Json
  | none => Json.null
  | some (action, revision) => Json.mkObj [("next", nextJson action),
      ("revision", revision.map revisionJson |>.getD Json.null)]

def foldCaseJson (history : List Read) : Json :=
  let held := fold history
  Json.mkObj [("reads", toJson (history.map readJson)), ("session", toJson (1 : Nat)),
    ("expected_shown", shownJson (shown 1 held)),
    ("expected_offered", offeredJson (offered 1 held))]

def foldCasesJson : String := (toJson (histories.map foldCaseJson)).compress

example : cases.length = 20 := by decide

end Conformance.GoalSessionObservationContracts
