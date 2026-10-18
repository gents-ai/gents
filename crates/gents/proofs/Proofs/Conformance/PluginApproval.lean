import Lean
import Proofs.ToolPolicy.PluginApproval
import Proofs.ToolPolicy.WorkspacePermissions

namespace Conformance.PluginApproval

open Lean ToolPolicy.PluginApproval

inductive Action where
  | decide (id : String) (now : Nat) (choice : Answer)
  | cancel
  deriving DecidableEq, Repr

def Action.run (s : State) : Action → State × Bool
  | .decide id now choice => answer s id now choice
  | .cancel => (ToolPolicy.PluginApproval.cancel s, true)

def Action.runScoped (available : List ToolPolicy.WorkspacePermissions.Scope)
    (expected : ToolPolicy.WorkspacePermissions.Grant) (s : State) : Action → State × Bool
  | .decide id now choice =>
      ToolPolicy.WorkspacePermissions.answerApproval available expected s id now choice
  | .cancel => (ToolPolicy.PluginApproval.cancel s, true)

def answerName : Answer → String
  | .once => "once" | .path => "file" | .folder => "always" | .deny => "deny"

def stateJson (s : State) : Json := Json.mkObj [
  ("present", toJson s.question.isSome),
  ("decision", toJson s.decision),
  ("path_grant", toJson s.pathGrant),
  ("folder_grant", toJson s.folderGrant)]

def stepJson (run : State → Action → State × Bool) (s : State) (a : Action) : Json :=
  let (next, accepted) := run s a
  let fields := match a with
    | .cancel => [("kind", toJson "cancel")]
    | .decide id now choice => [("kind", toJson "decide"), ("id", toJson id),
        ("now", toJson now), ("answer", toJson (answerName choice))]
  Json.mkObj (fields ++ [("accepted", toJson accepted), ("expected", stateJson next)])

def traceJson (run : State → Action → State × Bool) (s : State) : List Action → List Json
  | [] => []
  | action :: rest => stepJson run s action :: traceJson run (run s action).1 rest

def histories : List (List Action) :=
  ([ [.decide "q1" 10 .once, .decide "q1" 11 .folder]
  , [.decide "q1" 19 .deny, .decide "q1" 19 .folder]
  , [.decide "q1" 11 .folder, .decide "q1" 12 .deny]
  , [.decide "q1" 11 .path, .cancel, .decide "q1" 12 .folder]
  , [.cancel, .decide "q1" 12 .folder]
  ] : List (List Action)) ++ [Answer.once, .path, .folder, .deny].flatMap fun choice =>
    ([9, 10, 19, 20, 21].map fun now => [Action.decide "q1" now choice])
    ++ [[Action.decide "wrong" 11 choice], [Action.decide "" 11 choice]]

def unscopedCases : List Json :=
  [false, true].flatMap fun directory =>
  [false, true].flatMap fun write =>
  [none, some false, some true].flatMap fun pathGrant =>
  [none, some false, some true].flatMap fun folderGrant =>
  histories.map fun history =>
    let question : Question := ⟨"q1", 10, 20, directory, write⟩
    let initial : State := ⟨some question, none, pathGrant, folderGrant⟩
    Json.mkObj [
      ("directory", toJson directory), ("write", toJson write),
      ("initial", stateJson initial), ("steps", toJson (traceJson Action.run initial history))]

private def scopeJson (scope : ToolPolicy.WorkspacePermissions.Scope) : Json :=
  Json.mkObj [("workspace", toJson scope.workspace), ("revision", toJson scope.revision)]

private def grantJson (grant : ToolPolicy.WorkspacePermissions.Grant) : Json :=
  Json.mkObj [("scope", scopeJson grant.scope), ("action", toJson grant.action)]

def approvalGrant : ToolPolicy.WorkspacePermissions.Grant :=
  { scope := { workspace := 1, revision := 2 }, action := 3 }

def scopeObservations : List (List ToolPolicy.WorkspacePermissions.Scope) :=
  let current := approvalGrant.scope
  let other := { current with workspace := 2 }
  [[], [other], [current], [{ current with revision := 1 }], [other, current]]

def deliveryGrants : List ToolPolicy.WorkspacePermissions.Grant :=
  [ approvalGrant
  , { approvalGrant with scope := { approvalGrant.scope with revision := 1 } }
  , { approvalGrant with scope := { approvalGrant.scope with workspace := 2 } }
  , { approvalGrant with action := 4 } ]

def deliveryCases (decision : Option Bool) : List Json :=
  scopeObservations.flatMap fun available => deliveryGrants.map fun received =>
    let expected := decision.bind fun allowed =>
      ToolPolicy.WorkspacePermissions.consumeApproval available approvalGrant received allowed
    Json.mkObj [
      ("available", toJson (available.map scopeJson)),
      ("received", grantJson received),
      ("expected", toJson expected)]

def scopedCases : List Json :=
  [false, true].flatMap fun directory =>
  [false, true].flatMap fun write =>
  scopeObservations.flatMap fun available =>
  histories.map fun history =>
    let question : Question := ⟨"q1", 10, 20, directory, write⟩
    let initial : State := ⟨some question, none, none, none⟩
    let run := Action.runScoped available approvalGrant
    let final := history.foldl (fun state action => (run state action).1) initial
    Json.mkObj [
      ("directory", toJson directory), ("write", toJson write),
      ("initial", stateJson initial), ("steps", toJson (traceJson run initial history)),
      ("scoped", Json.mkObj [
        ("available", toJson (available.map scopeJson)),
        ("expected", grantJson approvalGrant),
        ("deliveries", toJson (deliveryCases final.decision))])]

def casesJson : Json := toJson (unscopedCases ++ scopedCases)

example : histories.length = 33 := by native_decide
example : unscopedCases.length = 1188 := by native_decide
example : scopedCases.length = 660 := by native_decide

end Conformance.PluginApproval
