import Lean
import Proofs.ToolPolicy.PluginApproval

namespace Conformance.PluginApproval

open Lean ToolPolicy.PluginApproval

inductive Action where
  | decide (id : String) (now : Nat) (choice : Answer)
  | cancel
  deriving DecidableEq, Repr

def Action.run (s : State) : Action → State × Bool
  | .decide id now choice => answer s id now choice
  | .cancel => (ToolPolicy.PluginApproval.cancel s, true)

def answerName : Answer → String
  | .once => "once" | .path => "file" | .folder => "always" | .deny => "deny"

def stateJson (s : State) : Json := Json.mkObj [
  ("present", toJson s.question.isSome),
  ("decision", toJson s.decision),
  ("path_grant", toJson s.pathGrant),
  ("folder_grant", toJson s.folderGrant)]

def stepJson (s : State) (a : Action) : Json :=
  let (next, accepted) := a.run s
  let fields := match a with
    | .cancel => [("kind", toJson "cancel")]
    | .decide id now choice => [("kind", toJson "decide"), ("id", toJson id),
        ("now", toJson now), ("answer", toJson (answerName choice))]
  Json.mkObj (fields ++ [("accepted", toJson accepted), ("expected", stateJson next)])

def traceJson (s : State) : List Action → List Json
  | [] => []
  | action :: rest => stepJson s action :: traceJson (action.run s).1 rest

def histories : List (List Action) :=
  ([ [.decide "q1" 10 .once, .decide "q1" 11 .folder]
  , [.decide "q1" 19 .deny, .decide "q1" 19 .folder]
  , [.decide "q1" 11 .folder, .decide "q1" 12 .deny]
  , [.decide "q1" 11 .path, .cancel, .decide "q1" 12 .folder]
  , [.cancel, .decide "q1" 12 .folder]
  ] : List (List Action)) ++ [Answer.once, .path, .folder, .deny].flatMap fun choice =>
    ([9, 10, 19, 20, 21].map fun now => [Action.decide "q1" now choice])
    ++ [[Action.decide "wrong" 11 choice], [Action.decide "" 11 choice]]

def casesJson : Json := toJson <|
  [false, true].flatMap fun directory =>
  [false, true].flatMap fun write =>
  [none, some false, some true].flatMap fun pathGrant =>
  [none, some false, some true].flatMap fun folderGrant =>
  histories.map fun history =>
    let question : Question := ⟨"q1", 10, 20, directory, write⟩
    let initial : State := ⟨some question, none, pathGrant, folderGrant⟩
    Json.mkObj [
      ("directory", toJson directory), ("write", toJson write),
      ("initial", stateJson initial), ("steps", toJson (traceJson initial history))]

example : histories.length = 33 := by native_decide

end Conformance.PluginApproval
