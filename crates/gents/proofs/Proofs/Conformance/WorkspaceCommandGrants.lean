import Lean
import Proofs.ToolPolicy.WorkspaceCommandGrants
import Proofs.Conformance.PluginApproval
import Proofs.Conformance.Contracts.Json.ToolPolicy

namespace Conformance.WorkspaceCommandGrants

open Lean ToolPolicy WorkspacePermissions
open Conformance.Contracts

private def choiceName : WorkspaceCommandGrants.Choice → String
  | .once => "once" | .remember => "invocation" | .deny => "deny"

private def stateJson (state : WorkspaceCommandGrants.State) : Json := Json.mkObj [
  ("question", Conformance.PluginApproval.stateJson state.question),
  ("grants", toJson (state.grants.map permissionGrantJson))]

private def target : Grant := ⟨⟨1, 2⟩, 1⟩

private def questionStates : List PluginApproval.State :=
  let question : PluginApproval.Question := ⟨"q1", 10, 20, false, false⟩
  [ ⟨some question, none, none, none⟩
  , ⟨some question, some true, none, none⟩
  , ⟨some question, some false, none, none⟩
  , ⟨none, none, none, none⟩ ]

private def scopes : List (List Scope) :=
  [[], [target.scope], [⟨1, 1⟩], [⟨2, 2⟩], [⟨2, 2⟩, target.scope]]

private def answerCase (available : List Scope) (question : PluginApproval.State)
    (id : String) (now : Nat) (choice : WorkspaceCommandGrants.Choice)
    (kind : ApprovalKind) (supported : Bool) : Json :=
  let state : WorkspaceCommandGrants.State := ⟨question, []⟩
  let (next, accepted) := WorkspaceCommandGrants.answer available target state id now choice kind supported
  Json.mkObj [
    ("available", toJson (available.map permissionScopeJson)),
    ("binding", permissionGrantJson target), ("initial", stateJson state),
    ("id", toJson id), ("now", toJson now), ("choice", toJson (choiceName choice)),
    ("kind", toJson kind.wireName), ("supported", toJson supported), ("accepted", toJson accepted),
    ("expected", stateJson next),
    ("reuse", toJson ((List.range 8).map fun index =>
      let candidate := { target with action := index + 1 }
      Json.mkObj [("candidate", permissionGrantJson candidate),
        ("allowed", toJson (WorkspaceCommandGrants.hasGrant next.grants candidate))]))]

def answerCases : List Json :=
  scopes.flatMap fun available =>
    questionStates.flatMap fun question =>
      ["q1", "wrong"].flatMap fun id =>
        [9, 10, 19, 20].flatMap fun now =>
          [WorkspaceCommandGrants.Choice.once, .remember, .deny].flatMap fun choice =>
            [false, true].map fun supported =>
              answerCase available question id now choice .command supported

def otherKindCases : List Json :=
  [ApprovalKind.filesystem, .fileMutation].map fun kind =>
    answerCase [target.scope] ⟨some ⟨"q1", 10, 20, false, false⟩, none, none, none⟩ "q1" 10 .remember kind false

private def preferenceJson (value : Preference) : Json := Json.mkObj [
  ("revision", toJson value.revision), ("mode", toJson value.mode.wireName),
  ("reviewTask", toJson value.reviewTask)]

private def resultJson (value : WriteResult) : Json := Json.mkObj [
  ("accepted", toJson value.accepted), ("changed", toJson value.changed),
  ("next", match value.next with | none => .null | some next => preferenceJson next)]

def revokeCases : List Json :=
  [0, 1, maxRevision].flatMap fun revision =>
  [Mode.ask, .autoReview, .fullAccess].flatMap fun mode =>
  [none, some 1].flatMap fun reviewTask =>
  [0, 1, maxRevision].map fun expected =>
    let previous : Preference := ⟨revision, mode, reviewTask⟩
    let grants := [{ target with scope := { target.scope with revision := revision } }]
    let (result, nextGrants) := WorkspaceCommandGrants.revokeLedger previous expected grants
    Json.mkObj [("previous", preferenceJson previous), ("expectedRevision", toJson expected),
      ("grants", toJson (grants.map permissionGrantJson)), ("result", resultJson result),
      ("remaining", toJson (nextGrants.map permissionGrantJson))]

def lookupCases : List Json :=
  [[], [target], [{ target with action := 2 }], [{ target with scope := ⟨2, 2⟩ }],
    [{ target with scope := ⟨1, 1⟩ }], [target, target]].flatMap fun grants =>
    [target, { target with action := 2 }, { target with scope := ⟨2, 2⟩ },
      { target with scope := ⟨1, 1⟩ }].map fun expected =>
      Json.mkObj [("grants", toJson (grants.map permissionGrantJson)),
        ("expected", permissionGrantJson expected),
        ("allowed", toJson (WorkspaceCommandGrants.hasGrant grants expected))]

def answersJson : Json := toJson (answerCases ++ otherKindCases)
def revocationsJson : Json := toJson revokeCases
def lookupsJson : Json := toJson lookupCases

example : answerCases.length = 960 := by native_decide
example : revokeCases.length = 54 := by native_decide
example : lookupCases.length = 24 := by native_decide

end Conformance.WorkspaceCommandGrants
