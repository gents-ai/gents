import Proofs.Conformance.ContractCases.BridgeStep

/-!
# R5 distributed scenario compositions

These traces replace the handwritten JSON scripts.  Their bridge outcomes are
computed by `Subagent.BridgedState.step`; this file only composes that owner
with explicit replication availability, delivery idempotence, session-keyed
wake coalescing, and stated crash/reopen premises.  Missing terminal request,
header, or segment facts leave an observer inert, so partitioned/reordered
arrival remains visible in the generated actions.
-/

namespace Conformance.ContractCases

inductive R5Node where | a | b deriving DecidableEq, Repr
inductive R5Terminal where | completed | failed | interrupted deriving DecidableEq, Repr

inductive R5ScenarioAction where
  | pair (node peer : R5Node)
  | acceptedBridge (tool child session : String) (parentDepth : Nat)
  | rejectedSpawnInvocation (tool child session : String) (parentDepth : Nat)
  | replicateBridge (tool : String) (source target : R5Node)
  | materializeChild (child tool : String)
  | replicateChild (child : String) (source target : R5Node)
  | publishTerminal (child : String) (terminal : R5Terminal) (hasMessage : Bool)
  | replicateTerminalRequest (child : String) (source target : R5Node)
  | replicateOutputSegments (child : String) (source target : R5Node)
  | replicateMessageHeader (child : String) (source target : R5Node)
  | observeCompletion
  | cancelBridge (tool : String)
  | replicateCancelIntent (tool : String) (source target : R5Node)
  | mirrorCancel (tool : String)
  | observeCancelAck
  | recover (node : R5Node)
  | crash (node : R5Node) (durableReopenPremise : Bool)
  | advanceClock (node : R5Node) (seconds : Nat)
  | converge
  deriving Repr

structure R5BridgeFact where
  tool : String
  child : String
  session : String
  parentDepth : Nat
  state : String := "running"
  cancelIntent : Bool := false
  deriving Repr

structure R5ChildFact where
  child : String
  depth : Nat := 0
  terminal : Option R5Terminal := none
  interruptRequested : Bool := false
  deriving Repr

structure R5ScenarioState where
  aBridges : List R5BridgeFact := []
  bBridges : List R5BridgeFact := []
  rejectedInvocations : List String := []
  aChildren : List R5ChildFact := []
  bChildren : List R5ChildFact := []
  aTerminalRequests : List String := []
  aSegments : List String := []
  aHeaders : List String := []
  notifications : List String := []
  wakeSessions : List String := []
  aGeneration : Nat := 0
  bGeneration : Nat := 0
  deriving Repr

def replaceBridge (rows : List R5BridgeFact) (next : R5BridgeFact) : List R5BridgeFact :=
  next :: rows.filter (fun row => row.tool != next.tool)

def replaceChild (rows : List R5ChildFact) (next : R5ChildFact) : List R5ChildFact :=
  next :: rows.filter (fun row => row.child != next.child)

def child? (rows : List R5ChildFact) (id : String) : Option R5ChildFact :=
  rows.find? (fun row => row.child == id)

def bridgeProjection? (terminal : R5Terminal) : Option String :=
  let childState := match terminal with
    | .completed => RequestState.completed
    | .failed => RequestState.failed
    | .interrupted => RequestState.interrupted
  let event := match terminal with
    | .completed => Subagent.BridgedState.Event.bridge_complete
    | .failed | .interrupted => Subagent.BridgedState.Event.bridge_failure
  let fixture := bridgeStepFixture childState .processing .cascade true
  match Subagent.BridgedState.step fixture event with
  | none => none
  | some post => (post.parent.findToolByCallId 77).map fun tool => tool.state.toDefraDB

def observeOne (s : R5ScenarioState) (bridge : R5BridgeFact) : R5ScenarioState :=
  match child? s.aChildren bridge.child with
  | none => s
  | some child =>
      if !(s.aTerminalRequests.contains bridge.child) then s else
      match child.terminal with
      | none => s
      | some terminal =>
          let dependencies := match terminal with
            | .completed => s.aSegments.contains bridge.child && s.aHeaders.contains bridge.child
            | .failed | .interrupted => true
          if !dependencies then s else
          match bridgeProjection? terminal with
          | none => s
          | some projected =>
              let bridge := { bridge with state := projected }
              { s with
                aBridges := replaceBridge s.aBridges bridge
                notifications := if s.notifications.contains bridge.tool then
                    s.notifications else s.notifications ++ [bridge.tool]
                wakeSessions := if s.wakeSessions.contains bridge.session then
                    s.wakeSessions else s.wakeSessions ++ [bridge.session] }

def observeAll (s : R5ScenarioState) : R5ScenarioState :=
  s.aBridges.foldl observeOne s

/-- A depth-rejected provider call is still a durable tool invocation, not a
    child-linked background bridge. Its pending → failed result is computed by
    the single-row tool owner; the delegation guard rejects creation at the
    maximum depth. -/
def rejectedSpawnPost? : Option ToolExecution.ToolCallContext :=
  ToolExecution.ToolCallContext.step?
    { bridgeStepToolRow .cascade .committed with
        state := .pending, awaitMode := .foreground, childRequestId := none }
    (.spawnFailed .argumentInvalid)

def R5ScenarioState.step (s : R5ScenarioState) : R5ScenarioAction → R5ScenarioState
  | .pair _ _ => s
  | .acceptedBridge tool child session parentDepth =>
      if parentDepth < Subagent.maxSubagentDepth then
        { s with aBridges := replaceBridge s.aBridges { tool, child, session, parentDepth } }
      else s
  | .rejectedSpawnInvocation tool _ _ parentDepth =>
      if parentDepth < Subagent.maxSubagentDepth then s else
      match rejectedSpawnPost? with
      | some post => if post.state = .failed ∧ post.failureClass = some .argumentInvalid then
          { s with rejectedInvocations := s.rejectedInvocations ++ [tool] }
        else s
      | none => s
  | .replicateBridge tool .a .b =>
      match s.aBridges.find? (fun row => row.tool == tool) with
      | some row => { s with bBridges := replaceBridge s.bBridges row }
      | none => s
  | .replicateBridge _ _ _ => s
  | .materializeChild child tool =>
      match s.bBridges.find? (fun row => row.tool == tool) with
      | some bridge => if bridge.parentDepth < Subagent.maxSubagentDepth then
          { s with bChildren := replaceChild s.bChildren { child, depth := bridge.parentDepth + 1 } }
        else s
      | none => s
  | .replicateChild child .b .a =>
      match child? s.bChildren child with
      | some row => { s with aChildren := replaceChild s.aChildren row }
      | none => s
  | .replicateChild _ _ _ => s
  | .publishTerminal child terminal _ =>
      match child? s.bChildren child with
      | some row => { s with bChildren := replaceChild s.bChildren { row with terminal := some terminal } }
      | none => s
  | .replicateTerminalRequest child .b .a =>
      match child? s.bChildren child with
      | some row => { s with
          aChildren := replaceChild s.aChildren row
          aTerminalRequests := if s.aTerminalRequests.contains child then s.aTerminalRequests else s.aTerminalRequests ++ [child] }
      | none => s
  | .replicateTerminalRequest _ _ _ => s
  | .replicateOutputSegments child .b .a =>
      { s with aSegments := if s.aSegments.contains child then s.aSegments else s.aSegments ++ [child] }
  | .replicateOutputSegments _ _ _ => s
  | .replicateMessageHeader child .b .a =>
      { s with aHeaders := if s.aHeaders.contains child then s.aHeaders else s.aHeaders ++ [child] }
  | .replicateMessageHeader _ _ _ => s
  | .observeCompletion => observeAll s
  | .cancelBridge tool =>
      { s with aBridges := s.aBridges.map fun row =>
          if row.tool == tool then { row with state := "cancelled", cancelIntent := true } else row }
  | .replicateCancelIntent tool .a .b =>
      match s.aBridges.find? (fun row => row.tool == tool) with
      | some row => { s with bBridges := replaceBridge s.bBridges row }
      | none => s
  | .replicateCancelIntent _ _ _ => s
  | .mirrorCancel tool =>
      match s.bBridges.find? (fun row => row.tool == tool && row.cancelIntent) with
      | none => s
      | some bridge => { s with bChildren := s.bChildren.map fun child =>
          if child.child == bridge.child && child.terminal.isNone then
            { child with interruptRequested := true } else child }
  | .observeCancelAck => s
  | .recover _ => s
  | .crash .a true => { s with aGeneration := s.aGeneration + 1 }
  | .crash .b true => { s with bGeneration := s.bGeneration + 1 }
  | .crash _ false => s
  | .advanceClock _ _ => s
  | .converge => observeAll s

def foldR5Scenario (actions : List R5ScenarioAction) : R5ScenarioState :=
  actions.foldl R5ScenarioState.step {}

structure R5ScenarioCase where
  name : String
  actions : List R5ScenarioAction
  post : R5ScenarioState
  deriving Repr

def r5Case (name : String) (actions : List R5ScenarioAction) : R5ScenarioCase :=
  { name, actions, post := foldR5Scenario actions }

def setup (tool child session : String) : List R5ScenarioAction :=
  [ .pair .a .b, .pair .b .a, .acceptedBridge tool child session 0,
    .replicateBridge tool .a .b, .materializeChild child tool,
    .replicateChild child .b .a ]

def complete (child : String) : List R5ScenarioAction :=
  [ .publishTerminal child .completed true,
    .replicateTerminalRequest child .b .a,
    .replicateOutputSegments child .b .a,
    .replicateMessageHeader child .b .a ]

def r5ScenarioCases : List R5ScenarioCase :=
  [ r5Case "happy_path"
      (setup "tool-call-1" "child-req-1" "parent-req-1-session" ++
       complete "child-req-1" ++ [.observeCompletion, .converge])
  , r5Case "b_crash_mid_execution"
      (setup "tool-call-b-crash" "child-req-b-crash" "parent-req-b-crash-session" ++
       [.crash .b true, .recover .b, .publishTerminal "child-req-b-crash" .failed false,
        .replicateTerminalRequest "child-req-b-crash" .b .a, .observeCompletion, .converge])
  , r5Case "a_crash_mid_wait"
      (setup "tool-call-a-crash-before" "child-req-a-crash-before" "parent-before-session" ++
       [.crash .a true] ++ complete "child-req-a-crash-before" ++
       [.recover .a, .observeCompletion] ++
       setup "tool-call-a-crash-after" "child-req-a-crash-after" "parent-after-session" ++
       complete "child-req-a-crash-after" ++
       [.crash .a true, .recover .a, .observeCompletion, .converge])
  , r5Case "partition_during_cancel"
      (setup "tool-call-cancel" "child-req-cancel" "parent-cancel-session" ++
       [.cancelBridge "tool-call-cancel", .advanceClock .a 360, .observeCancelAck,
        .replicateCancelIntent "tool-call-cancel" .a .b, .mirrorCancel "tool-call-cancel",
        .publishTerminal "child-req-cancel" .interrupted false,
        .replicateTerminalRequest "child-req-cancel" .b .a, .observeCancelAck, .converge])
  , r5Case "multi_completion_coalesce"
      (setup "tool-call-multi-1" "child-req-multi-1" "parent-multi-session" ++
       [.acceptedBridge "tool-call-multi-2" "child-req-multi-2" "parent-multi-session" 0,
        .replicateBridge "tool-call-multi-2" .a .b,
        .materializeChild "child-req-multi-2" "tool-call-multi-2",
        .replicateChild "child-req-multi-2" .b .a] ++
       complete "child-req-multi-1" ++ complete "child-req-multi-2" ++
       [.observeCompletion, .observeCompletion, .converge])
  , r5Case "remote_depth_ceiling"
      [ .pair .a .b, .pair .b .a,
        .rejectedSpawnInvocation "tool-call-depth-ceiling" "child-req-depth-ceiling"
          "parent-depth-ceiling-session" Subagent.maxSubagentDepth,
        .converge ]
  ]

theorem r5ScenarioCases_owner_computed :
    (r5ScenarioCases.map fun c =>
      (c.name, c.post.notifications.length, c.post.wakeSessions.length,
       c.post.aGeneration, c.post.bGeneration)) =
    [ ("happy_path", 1, 1, 0, 0)
    , ("b_crash_mid_execution", 1, 1, 0, 1)
    , ("a_crash_mid_wait", 2, 2, 2, 0)
    , ("partition_during_cancel", 1, 1, 0, 0)
    , ("multi_completion_coalesce", 2, 1, 0, 0)
    , ("remote_depth_ceiling", 0, 0, 0, 0) ] := by
  native_decide

theorem r5ScenarioCases_remote_depth_bounded :
    ∀ scenario ∈ r5ScenarioCases,
      ∀ child ∈ scenario.post.bChildren,
        child.depth ≤ Subagent.maxSubagentDepth := by
  native_decide

theorem r5ScenarioCases_depth_rejects_invocation_without_bridge_or_child :
    let depthCase := (r5ScenarioCases.find? fun c => c.name = "remote_depth_ceiling").getD
      (r5Case "missing" [])
    depthCase.post.rejectedInvocations = ["tool-call-depth-ceiling"] ∧
    depthCase.post.aBridges.length = 0 ∧ depthCase.post.bBridges.length = 0 ∧
    depthCase.post.aChildren.length = 0 ∧ depthCase.post.bChildren.length = 0 := by
  native_decide

end Conformance.ContractCases
