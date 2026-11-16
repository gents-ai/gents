import Lean
import Proofs.Conformance.Family
import Proofs.Tools.ToolPolicy.WorkspaceCommandGrants
import Proofs.Conformance.PluginApproval
import Proofs.Conformance.Contracts.Json.ToolPolicy
import Proofs.Conformance.WorkspacePermissions

namespace Conformance.WorkspaceCommandGrants

open Lean ToolPolicy WorkspacePermissions
open Conformance.Contracts

deriving instance ToJson for Scope, Grant

private def target : Grant := ⟨⟨1, 2⟩, 1⟩

structure QuestionWire where
  present : Bool
  decision : Option Bool
  deriving ToJson

structure StateWire where
  question : QuestionWire
  grants : List Grant
  deriving ToJson

structure AnswerInput where
  available : List Scope
  binding : Grant
  initial : StateWire
  id : String
  now : Nat
  choice : String
  kind : String
  supported : Bool
  candidates : List Grant
  deriving ToJson

structure Reuse where
  candidate : Grant
  allowed : Bool
  deriving ToJson

structure AnswerOutput where
  accepted : Bool
  expected : StateWire
  reuse : List Reuse
  deriving ToJson

/-- A command question is the one fixed question `q1`, published at 10 and
expiring at 20, that the native fixture writes; only its presence and decision
vary. -/
private def questionOf (question : QuestionWire) : PluginApproval.State :=
  ⟨if question.present then some ⟨"q1", 10, 20, false, false⟩ else none, question.decision, none, none⟩

private def questionWire (state : PluginApproval.State) : QuestionWire :=
  ⟨state.question.isSome, state.decision⟩

private def choiceName : WorkspaceCommandGrants.Choice → String
  | .once => "once" | .remember => "invocation" | .deny => "deny"

private def choiceOf (name : String) : WorkspaceCommandGrants.Choice :=
  (([.once, .remember, .deny] : List WorkspaceCommandGrants.Choice).find? (choiceName · == name)).getD .once

private def kindOf (name : String) : ApprovalKind :=
  (([.filesystem, .command, .fileMutation] : List ApprovalKind).find? (·.wireName == name)).getD .command

private def questionStates : List PluginApproval.State :=
  let question : PluginApproval.Question := ⟨"q1", 10, 20, false, false⟩
  [ ⟨some question, none, none, none⟩
  , ⟨some question, some true, none, none⟩
  , ⟨some question, some false, none, none⟩
  , ⟨none, none, none, none⟩ ]

private def scopes : List (List Scope) :=
  [[], [target.scope], [⟨1, 1⟩], [⟨2, 2⟩], [⟨2, 2⟩, target.scope]]

private def answerInput (available : List Scope) (question : PluginApproval.State)
    (id : String) (now : Nat) (choice : WorkspaceCommandGrants.Choice)
    (kind : ApprovalKind) (supported : Bool) : AnswerInput :=
  { available, binding := target, initial := ⟨questionWire question, []⟩, id, now
    choice := choiceName choice, kind := kind.wireName, supported
    candidates := (List.range 8).map fun index => { target with action := index + 1 } }

def answerInputs : List AnswerInput :=
  scopes.flatMap fun available =>
    questionStates.flatMap fun question =>
      ["q1", "wrong"].flatMap fun id =>
        [9, 10, 19, 20].flatMap fun now =>
          [WorkspaceCommandGrants.Choice.once, .remember, .deny].flatMap fun choice =>
            [false, true].map fun supported =>
              answerInput available question id now choice .command supported

def otherKindInputs : List AnswerInput :=
  [ApprovalKind.filesystem, .fileMutation].map fun kind =>
    answerInput [target.scope] ⟨some ⟨"q1", 10, 20, false, false⟩, none, none, none⟩ "q1" 10 .remember kind false

@[conformance_family]
def answers : Family AnswerInput AnswerOutput where
  inputs := answerInputs ++ otherKindInputs
  model := fun i =>
    let state : WorkspaceCommandGrants.State := ⟨questionOf i.initial.question, i.initial.grants⟩
    let (next, accepted) := WorkspaceCommandGrants.answer i.available i.binding state i.id i.now
      (choiceOf i.choice) (kindOf i.kind) i.supported
    { accepted, expected := ⟨questionWire next.question, next.grants⟩
      reuse := i.candidates.map fun candidate =>
        ⟨candidate, WorkspaceCommandGrants.hasGrant next.grants candidate⟩ }

structure PreferenceWire where
  revision : Nat
  mode : String
  reviewTask : Option Nat
  deriving ToJson

structure RevokeInput where
  previous : PreferenceWire
  expectedRevision : Nat
  grants : List Grant
  deriving ToJson

structure RevokeResult where
  accepted : Bool
  changed : Bool
  next : Option PreferenceWire
  deriving ToJson

structure RevokeOutput where
  result : RevokeResult
  remaining : List Grant
  deriving ToJson

private def preferenceWire (value : Preference) : PreferenceWire :=
  ⟨value.revision, value.mode.wireName, value.reviewTask⟩

private def modeOf (name : String) : Mode :=
  (([.ask, .autoReview, .fullAccess] : List Mode).find? (·.wireName == name)).getD .ask

@[conformance_family]
def revocations : Family RevokeInput RevokeOutput where
  inputs :=
    [0, 1, maxRevision].flatMap fun revision =>
    [Mode.ask, .autoReview, .fullAccess].flatMap fun mode =>
    [none, some 1].flatMap fun reviewTask =>
    [0, 1, maxRevision].map fun expectedRevision =>
      { previous := preferenceWire ⟨revision, mode, reviewTask⟩, expectedRevision
        grants := [{ target with scope := { target.scope with revision := revision } }] }
  model := fun i =>
    let (result, remaining) := WorkspaceCommandGrants.revokeLedger
      ⟨i.previous.revision, modeOf i.previous.mode, i.previous.reviewTask⟩ i.expectedRevision i.grants
    { result := ⟨result.accepted, result.changed, result.next.map preferenceWire⟩, remaining }

structure LookupInput where
  grants : List Grant
  expected : Grant
  deriving ToJson

@[conformance_family]
def lookup : Family LookupInput Bool where
  inputs :=
    [[], [target], [{ target with action := 2 }], [{ target with scope := ⟨2, 2⟩ }],
      [{ target with scope := ⟨1, 1⟩ }], [target, target]].flatMap fun grants =>
      [target, { target with action := 2 }, { target with scope := ⟨2, 2⟩ },
        { target with scope := ⟨1, 1⟩ }].map fun expected => { grants, expected }
  model := fun i => WorkspaceCommandGrants.hasGrant i.grants i.expected

end Conformance.WorkspaceCommandGrants
