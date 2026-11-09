import Proofs.GoalAutomation.SessionObservation
import Lean

namespace Conformance.GoalSessionObservationContracts

open GoalAutomation.SessionObservation GoalAutomation.RequestAssignment Lean

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

example : cases.length = 20 := by decide

end Conformance.GoalSessionObservationContracts
