import Proofs.Automation.GoalAutomation.SessionObservation
import Lean

namespace Conformance.GoalSessionObservationContracts

open GoalAutomation.SessionObservation GoalAutomation.RequestAssignment GoalAutomation Lean

structure Case where
  session : SessionRead
  projection : ProjectionRead
  captured : Option Reference
  deriving DecidableEq, Repr

def projections : List ProjectionRead :=
  [.missing, .unverified, .verified none, .verified (some ⟨1, none⟩),
    .verified (some ⟨2, some 200⟩)]

def cases : List Case :=
  [SessionRead.absent, .present].flatMap fun session =>
    projections.flatMap fun projection =>
      [none, some ⟨1, some 100⟩].map fun captured => ⟨session, projection, captured⟩

def referenceJson (reference : Reference) : Json :=
  Json.mkObj [("goal", toJson reference.goal), ("root", toJson reference.root)]

def sessionJson : SessionRead → Json
  | .absent => "absent" | .present => "present"

def projectionJson : ProjectionRead → Json
  | .missing => Json.mkObj [("kind", "missing")]
  | .unverified => Json.mkObj [("kind", "unverified")]
  | .verified goal => Json.mkObj [("kind", "verified"),
      ("goal", goal.map referenceJson |>.getD Json.null)]

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

def preparedJson : PreparedInput → Json
  | .pending => Json.mkObj [("kind", "pending")]
  | .prepared reference => Json.mkObj [("kind", "prepared"),
      ("reference", reference.map referenceJson |>.getD Json.null)]

def caseJson (c : Case) : Json :=
  let state := observe c.session c.projection
  Json.mkObj [("session", sessionJson c.session), ("projection", projectionJson c.projection),
    ("captured", c.captured.map referenceJson |>.getD Json.null),
    ("expected_state", stateJson state), ("expected_view", viewJson (view state)),
    ("expected_next", nextJson (next state)),
    ("expected_bind", preparedJson (bind c.captured state)),
    ("expected_refusal", (refusal c.captured state).map unknownCauseJson |>.getD Json.null)]

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
