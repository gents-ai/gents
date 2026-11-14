import Lean
import Proofs.Tools.ToolPolicy.PluginApproval
import Proofs.Tools.ToolPolicy.WorkspacePermissions
import Proofs.Conformance.Family

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
    (expected : ToolPolicy.WorkspacePermissions.Grant)
    (kind : ToolPolicy.WorkspacePermissions.ApprovalKind) (s : State) : Action → State × Bool
  | .decide id now choice =>
      ToolPolicy.WorkspacePermissions.answerApproval available expected s id now choice kind
  | .cancel => (ToolPolicy.PluginApproval.cancel s, true)

def answerName : Answer → String
  | .once => "once" | .path => "file" | .folder => "always" | .deny => "deny"

def answerOf : String → Answer
  | "once" => .once | "file" => .path | "always" => .folder | _ => .deny

def histories : List (List Action) :=
  ([ [.decide "q1" 10 .once, .decide "q1" 11 .folder]
  , [.decide "q1" 19 .deny, .decide "q1" 19 .folder]
  , [.decide "q1" 11 .folder, .decide "q1" 12 .deny]
  , [.decide "q1" 11 .path, .cancel, .decide "q1" 12 .folder]
  , [.cancel, .decide "q1" 12 .folder]
  ] : List (List Action)) ++ [Answer.once, .path, .folder, .deny].flatMap fun choice =>
    ([9, 10, 19, 20, 21].map fun now => [Action.decide "q1" now choice])
    ++ [[Action.decide "wrong" 11 choice], [Action.decide "" 11 choice]]

def question (directory write : Bool) : Question := ⟨"q1", 10, 20, directory, write⟩

structure StateWire where
  present : Bool
  decision : Option Bool
  pathGrant : Option Bool
  folderGrant : Option Bool
  deriving ToJson, BEq

def stateWire (s : State) : StateWire :=
  { present := s.question.isSome, decision := s.decision, pathGrant := s.pathGrant
    folderGrant := s.folderGrant }

def stateOf (directory write : Bool) (w : StateWire) : State :=
  { question := if w.present then some (question directory write) else none
    decision := w.decision, pathGrant := w.pathGrant, folderGrant := w.folderGrant }

structure ScopeWire where
  workspace : Nat
  revision : Nat
  deriving ToJson, BEq

structure GrantWire where
  scope : ScopeWire
  action : Nat
  deriving ToJson, BEq

def scopeWire (scope : ToolPolicy.WorkspacePermissions.Scope) : ScopeWire :=
  { workspace := scope.workspace, revision := scope.revision }

def scopeOf (w : ScopeWire) : ToolPolicy.WorkspacePermissions.Scope :=
  { workspace := w.workspace, revision := w.revision }

def grantWire (grant : ToolPolicy.WorkspacePermissions.Grant) : GrantWire :=
  { scope := scopeWire grant.scope, action := grant.action }

def grantOf (w : GrantWire) : ToolPolicy.WorkspacePermissions.Grant :=
  { scope := scopeOf w.scope, action := w.action }

def kindOf : String → ToolPolicy.WorkspacePermissions.ApprovalKind
  | "command" => .command
  | "fileMutation" => .fileMutation
  | _ => .filesystem

/-- One answer or cancellation applied to the state the history reached before
it: the question (`q1`, open from 10 until 20) is present or removed, an answer
may already be recorded, and grants may already exist. -/
structure StepInput where
  approval : String
  available : List ScopeWire
  directory : Bool
  write : Bool
  initial : StateWire
  kind : String
  id : Option String
  now : Option Nat
  answer : Option String
  deriving ToJson, BEq

structure StepOutput where
  accepted : Bool
  expected : StateWire
  deriving ToJson

def actionOf (i : StepInput) : Action :=
  match i.kind, i.id, i.now, i.answer with
  | "decide", some id, some now, some choice => .decide id now (answerOf choice)
  | _, _, _, _ => .cancel

def stepInput (approval : String) (available : List ToolPolicy.WorkspacePermissions.Scope)
    (directory write : Bool) (s : State) : Action → StepInput
  | .decide id now choice =>
      { approval, available := available.map scopeWire, directory, write, initial := stateWire s
        kind := "decide", id := some id, now := some now, answer := some (answerName choice) }
  | .cancel =>
      { approval, available := available.map scopeWire, directory, write, initial := stateWire s
        kind := "cancel", id := none, now := none, answer := none }

def steps (run : State → Action → State × Bool) (input : State → Action → StepInput)
    (s : State) : List Action → List StepInput
  | [] => []
  | action :: rest => input s action :: steps run input (run s action).1 rest

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

def unscopedSteps : List StepInput :=
  ([false, true].flatMap fun directory =>
  [false, true].flatMap fun write =>
  [none, some false, some true].flatMap fun pathGrant =>
  [none, some false, some true].flatMap fun folderGrant =>
  histories.flatMap fun history =>
    steps Action.run (stepInput "" [] directory write)
      ⟨some (question directory write), none, pathGrant, folderGrant⟩ history).eraseDups

def scopedConfigurations : List (ToolPolicy.WorkspacePermissions.ApprovalKind × Bool × Bool) :=
  ([false, true].flatMap fun directory => [false, true].map fun write =>
    (ToolPolicy.WorkspacePermissions.ApprovalKind.filesystem, directory, write)) ++
    [(.command, false, false), (.fileMutation, false, true)]

def scopedSteps : List StepInput :=
  (scopedConfigurations.flatMap fun (kind, directory, write) =>
  scopeObservations.flatMap fun available =>
  histories.flatMap fun history =>
    steps (Action.runScoped available approvalGrant kind)
      (stepInput kind.wireName available directory write)
      ⟨some (question directory write), none, none, none⟩ history).eraseDups

def runStep (i : StepInput) : StepOutput :=
  let (next, accepted) := Action.run (stateOf i.directory i.write i.initial) (actionOf i)
  { accepted, expected := stateWire next }

def runScopedStep (i : StepInput) : StepOutput :=
  let (next, accepted) := Action.runScoped (i.available.map scopeOf) approvalGrant
    (kindOf i.approval) (stateOf i.directory i.write i.initial) (actionOf i)
  { accepted, expected := stateWire next }

/-- Each unscoped step starts from a state some history reaches and checks the
native queue, answer file and global grants after it. -/
@[conformance_family]
def unscopedStep : Conformance.Family StepInput StepOutput where
  inputs := unscopedSteps
  model := runStep

/-- Each workspace-scoped step starts from a state some scoped history reaches
under the workspace scopes the settings observe. -/
@[conformance_family]
def scopedStep : Conformance.Family StepInput StepOutput where
  inputs := scopedSteps
  model := runScopedStep

structure DeliveryInput where
  approval : String
  directory : Bool
  write : Bool
  decision : Option Bool
  available : List ScopeWire
  received : GrantWire
  deriving ToJson, BEq

/-- `delivered` is the answer the resuming call consumes; `refused` is a
recorded answer whose delivered grant no longer matches. -/
structure DeliveryOutput where
  delivered : Option Bool
  refused : Bool
  deriving ToJson

def finalDecisions : List (ToolPolicy.WorkspacePermissions.ApprovalKind × Bool × Bool × Option Bool) :=
  (scopedConfigurations.flatMap fun (kind, directory, write) =>
  scopeObservations.flatMap fun available =>
  histories.map fun history =>
    let run := Action.runScoped available approvalGrant kind
    let final := history.foldl (fun state action => (run state action).1)
      ⟨some (question directory write), none, none, none⟩
    (kind, directory, write, final.decision)).eraseDups

/-- A recorded answer is delivered to its waiting call under the workspace
scopes then observed and the grant the answer file then carries. -/
@[conformance_family]
def delivery : Conformance.Family DeliveryInput DeliveryOutput where
  inputs := finalDecisions.flatMap fun (kind, directory, write, decision) =>
    scopeObservations.flatMap fun available => deliveryGrants.map fun received =>
      { approval := kind.wireName, directory, write, decision
        available := available.map scopeWire, received := grantWire received }
  model := fun i =>
    let delivered := i.decision.bind fun allowed =>
      ToolPolicy.WorkspacePermissions.consumeApproval (i.available.map scopeOf) approvalGrant
        (grantOf i.received) allowed
    { delivered, refused := i.decision.isSome && delivered.isNone }

example : histories.length = 33 := by native_decide

end Conformance.PluginApproval
